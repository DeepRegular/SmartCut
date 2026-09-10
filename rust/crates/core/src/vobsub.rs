//! A DVD's subtitles, written out beside the cut.
//!
//! A DVD draws its subtitles the way a Blu-ray does -- a run-length coded
//! picture with commands saying where to put it -- but where a Blu-ray's
//! graphics have a stream type of their own that a transport stream can
//! carry ([`crate::pgs`]), a DVD's have none. Written into a `.ts` they
//! become private data of no stated kind, and everything reads them back as
//! `bin_data`: carried, declared, and invisible. There is no number to
//! correct that with, the way `0x90` corrects a Blu-ray's.
//!
//! So they go beside the cut instead, in the form the rest of the world
//! already reads them in: a **VobSub** pair.
//!
//! ```text
//! cut_title.ts     the cut
//! cut_title.idx    what the subtitles are, and when each one appears
//! cut_title.sub    the subtitle pictures themselves, untouched
//! ```
//!
//! Nothing is re-encoded. The `.sub` holds the disc's own units back in the
//! program stream framing they arrived in, and the `.idx` is a text file of
//! times and file positions with the palette at the top. A player told to
//! open the cut finds them by name.
//!
//! ## What one subtitle is
//!
//! A **sub-picture unit**: its own length, then the picture, then a chain of
//! display control sequences. Each sequence carries a delay from the unit's
//! own timestamp and the commands to run then -- which colours, which corner
//! of the screen, put it up, take it down.
//!
//! ```text
//! size  dcsqt   [ picture ]   [ delay next  PAL ALPHA AREA RLE START END ]
//!                             [ delay next  STOP END                    ]
//! ```
//!
//! There are three things to do with one of these, and this module holds all
//! three. One is to write it out unchanged, which is what everything above is
//! for. The second is to **read the picture out of it** so that it can be
//! written again as the kind of subtitle a transport stream does carry -- see
//! [`Reader`], and [`crate::pgs::write`] for the other half of that.
//!
//! The third is that same journey the other way. A Blu-ray's subtitles *can*
//! travel inside a cut and are not always wanted there: a pair beside the cut
//! is what an editor, a player on an old set-top box, or a subtitle tool
//! reads without being taught anything. So a display set is decoded
//! ([`crate::pgs::read`]) and **written back as one of these units** --
//! [`unit`], and [`Ink`] for the palette that has to be invented on the way,
//! since a Blu-ray carries its colours in the stream and a DVD does not carry
//! them at all.
//!
//! **The second sequence is optional, and often absent.** Of the units on
//! the disc measured here, not one carries a STOP: each subtitle stands
//! until the next unit replaces it. That is the whole reason this module
//! edits anything at all -- a cut that ends while a subtitle is up would
//! otherwise leave it up, with nothing coming to take it down. See
//! [`stopped_after`].

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use std::io::Write;

/// What a delay in a display control sequence counts in: 1024 ticks of the
/// 90 kHz clock, which is 11.378 ms.
///
/// Public because it is the resolution of everything a unit says about time:
/// a stop written to land at an exact instant lands within one of these of
/// it, and a caller comparing the two has to know that.
pub const TICK: f64 = 1024.0 / 90_000.0;

/// Commands, of the ten a unit may carry, that this has to know by name.
const STOP: u8 = 0x02;
const END: u8 = 0xFF;

/// How long each subtitle is written at, and where each one starts.
///
/// A `.sub` is a program stream, and a program stream is read by seeking to
/// a byte position and reading forward -- so every unit starts at a sector
/// boundary and the `.idx` says which one.
const SECTOR: usize = 2048;

/// One sub-picture unit's display control sequences, as offsets into it.
///
/// The chain ends where a sequence points at itself, which is how the format
/// says "no more" without a count.
fn sequences(spu: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let size = u16be(spu, 0) as usize;
    let mut at = u16be(spu, 2) as usize;
    while at + 4 <= size.min(spu.len()) && !out.contains(&at) {
        out.push(at);
        let next = u16be(spu, at + 2) as usize;
        if next == at || next == 0 {
            break;
        }
        at = next;
    }
    out
}

/// Whether one sequence takes the display down.
fn takes_it_down(spu: &[u8], at: usize) -> bool {
    let size = u16be(spu, 0) as usize;
    let mut p = at + 4;
    while p < size.min(spu.len()) {
        match spu[p] {
            END => return false,
            STOP => return true,
            0x00 | 0x01 => p += 1,
            0x03 | 0x04 => p += 3,
            0x05 => p += 7,
            0x06 => p += 5,
            0x07 => return false, // a colour change, whose length varies
            _ => return false,
        }
    }
    false
}

/// When this unit takes its own display down, in seconds after the moment it
/// is shown -- `None` where it never does, which is what leaves a subtitle
/// standing until the next one arrives.
pub fn stops_after(spu: &[u8]) -> Option<f64> {
    sequences(spu)
        .into_iter()
        .find(|&at| takes_it_down(spu, at))
        .map(|at| u16be(spu, at) as f64 * TICK)
}

/// The same unit, with its display taken down `after` seconds from the
/// moment it is shown.
///
/// Two shapes, because units come in two: one that already says when to
/// stop has that delay rewritten, and one that never says gets a sequence
/// of its own appended -- four bytes of header, the command, and the end
/// marker -- with the sequence before it pointed at the new one and the
/// unit's own length grown to cover it.
///
/// A unit that already stops earlier than asked is left alone: the disc's
/// own timing is nearer the truth than the cut's boundary.
pub fn stopped_after(spu: &[u8], after: f64) -> Vec<u8> {
    let delay = (after.max(0.0) / TICK).round().min(u16::MAX as f64) as u16;
    let size = u16be(spu, 0) as usize;
    if size > spu.len() || size < 4 {
        return spu.to_vec();
    }
    let mut out = spu[..size].to_vec();
    let chain = sequences(&out);
    if let Some(&at) = chain.iter().find(|&&at| takes_it_down(&out, at)) {
        if u16be(&out, at) > delay {
            out[at..at + 2].copy_from_slice(&delay.to_be_bytes());
        }
        return out;
    }
    let Some(&last) = chain.last() else {
        return out;
    };
    let added = out.len();
    out[last + 2..last + 4].copy_from_slice(&(added as u16).to_be_bytes());
    out.extend_from_slice(&delay.to_be_bytes());
    out.extend_from_slice(&(added as u16).to_be_bytes()); // pointing at itself: the end
    out.push(STOP);
    out.push(END);
    let grown = out.len() as u16;
    out[0..2].copy_from_slice(&grown.to_be_bytes());
    out
}

/// A unit that puts nothing up and takes down whatever is there.
///
/// Ten bytes: its own length, where the sequences begin, and one sequence
/// that runs at once and says stop. Discs send these themselves, which is
/// what makes them the right thing to end a kept range with -- a subtitle
/// standing when the cut ends is taken down by the same means the disc would
/// have used, rather than by editing the unit that put it up.
pub fn take_down() -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.extend_from_slice(&10u16.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // at once
    out.extend_from_slice(&4u16.to_be_bytes()); // pointing at itself: the end
    out.push(STOP);
    out.push(END);
    out
}

/// One subtitle, decoded: the picture a unit or a display set was carrying.
///
/// An index per pixel and the colours those index into, which is the shape
/// both formats keep a subtitle in -- so what [`crate::pgs::write`] does
/// with this is spell the same pixels differently, and what [`unit`] does
/// with it is spell them back.
pub struct Drawn {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// One byte per pixel, rows first.
    pub indices: Vec<u8>,
    /// Red, green, blue and how opaque, one for each index.
    pub palette: Vec<(u8, u8, u8, u8)>,
    /// How long after it appears the unit says it should go -- `None` where
    /// it never says, which is most of them.
    pub until: Option<f64>,
}

/// Reads the picture out of a unit.
///
/// libavcodec's own decoder does the reading: the run-length coding, the
/// display commands, the cropping, and the odd corners of a format that has
/// been in the field since 1996. What this adds is the one thing the decoder
/// cannot know -- **the palette**, which is not in the stream at all (see
/// [`Palette`]) -- handed over in the form the decoder looks for, which is
/// the same text the `.idx` beside a cut is written in.
pub struct Reader {
    decoder: ff::codec::decoder::Subtitle,
}

impl Reader {
    /// Open one for a title of this palette and this screen.
    pub fn open(palette: &Palette, screen: (u16, u16)) -> Result<Reader> {
        let colours: Vec<String> = palette.0.iter().map(|c| format!("{c:06x}")).collect();
        let told = format!(
            "size: {}x{}\npalette: {}\n",
            screen.0,
            screen.1,
            colours.join(", ")
        );
        let mut ctx = ff::codec::context::Context::new();
        unsafe {
            let p = ctx.as_mut_ptr();
            (*p).codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
            (*p).codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_DVD_SUBTITLE;
            (*p).width = screen.0 as i32;
            (*p).height = screen.1 as i32;
            let n = told.len();
            let room = n + ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            let buf = ff::ffi::av_mallocz(room) as *mut u8;
            if buf.is_null() {
                return Err(anyhow!("no room for the palette"));
            }
            std::ptr::copy_nonoverlapping(told.as_ptr(), buf, n);
            (*p).extradata = buf;
            (*p).extradata_size = n as i32;
        }
        Ok(Reader {
            decoder: ctx.decoder().subtitle()?,
        })
    }

    /// The picture in one unit, or `None` where it carries none -- which is
    /// what a unit that only takes the last one down amounts to.
    pub fn read(&mut self, unit: &[u8]) -> Result<Option<Drawn>> {
        let packet = ff::Packet::copy(unit);
        let mut sub = ff::codec::subtitle::Subtitle::new();
        if !self.decoder.decode(&packet, &mut sub)? {
            return Ok(None);
        }
        Ok(drawn_from(&sub))
    }
}

/// The picture in a decoded subtitle, whichever decoder read it.
///
/// A DVD's units and a Blu-ray's display sets come out of libavcodec in the
/// same shape -- one byte per pixel and a table of colours to look them up in
/// -- so both readers end here. `None` where the subtitle draws nothing,
/// which is what the thing that clears a screen decodes to in either format.
pub(crate) fn drawn_from(sub: &ff::codec::subtitle::Subtitle) -> Option<Drawn> {
    // How long the subtitle says it stands. The decoder counts from the
    // moment it appears, in milliseconds, and says nothing by saying either
    // nothing or everything.
    let until = match sub.end() {
        0 | u32::MAX => None,
        ms => Some(ms as f64 / 1000.0),
    };
    for rect in sub.rects() {
        let ff::codec::subtitle::Rect::Bitmap(bitmap) = rect else {
            continue;
        };
        let drawn = unsafe {
            let r = bitmap.as_ptr();
            let (w, h) = ((*r).w as usize, (*r).h as usize);
            let stride = (*r).linesize[0] as usize;
            if w == 0 || h == 0 || (*r).data[0].is_null() {
                continue;
            }
            let mut indices = Vec::with_capacity(w * h);
            for row in 0..h {
                let line = std::slice::from_raw_parts((*r).data[0].add(row * stride), w);
                indices.extend_from_slice(line);
            }
            // The colours, as libavcodec hands them over: one word each,
            // opacity in the top byte.
            let colours = ((*r).nb_colors as usize).min(256);
            let table = (*r).data[1] as *const u32;
            let palette = (0..colours)
                .map(|i| {
                    let c = if table.is_null() { 0 } else { *table.add(i) };
                    (
                        ((c >> 16) & 0xFF) as u8,
                        ((c >> 8) & 0xFF) as u8,
                        (c & 0xFF) as u8,
                        ((c >> 24) & 0xFF) as u8,
                    )
                })
                .collect();
            Drawn {
                x: (*r).x.max(0) as u16,
                y: (*r).y.max(0) as u16,
                width: w as u16,
                height: h as u16,
                indices,
                palette,
                until,
            }
        };
        return Some(drawn);
    }
    None
}

/// The sixteen colours a title's subtitles are drawn from.
///
/// Not in the stream: a DVD keeps them in the index, beside the chain of
/// cells they belong to, so a unit that says "colour 4" means nothing
/// without the disc that carried it. This is why a `.sub` on its own is not
/// enough and the `.idx` has to be written with it.
#[derive(Debug, Clone, Copy)]
pub struct Palette(pub [u32; 16]);

impl Palette {
    /// The palette a DVD writes: sixteen entries of luma and two colour
    /// differences, in the studio range television uses.
    pub fn from_clut(clut: &[u8]) -> Palette {
        let mut out = [0u32; 16];
        for (i, slot) in out.iter_mut().enumerate() {
            let e = clut.get(i * 4..i * 4 + 4).unwrap_or(&[0, 0, 128, 128]);
            let (y, cr, cb) = (e[1] as f64, e[2] as f64, e[3] as f64);
            let luma = 1.164 * (y - 16.0);
            let clamp = |v: f64| v.clamp(0.0, 255.0).round() as u32;
            let r = clamp(luma + 1.596 * (cr - 128.0));
            let g = clamp(luma - 0.813 * (cr - 128.0) - 0.391 * (cb - 128.0));
            let b = clamp(luma + 2.018 * (cb - 128.0));
            *slot = (r << 16) | (g << 8) | b;
        }
        Palette(out)
    }

    /// What the index calls a grey run of sixteen, for a recording whose own
    /// palette could not be found. Legible, and honestly not the disc's.
    pub fn grey() -> Palette {
        let mut out = [0u32; 16];
        for (i, slot) in out.iter_mut().enumerate() {
            let v = (i as u32 * 17).min(255);
            *slot = (v << 16) | (v << 8) | v;
        }
        Palette(out)
    }
}

/// The sixteen colours an index is written with, filled in as they are met.
///
/// A DVD's palette comes off the disc whole: sixteen entries sitting in the
/// index beside the cells they belong to, and [`Palette::from_clut`] is the
/// whole of the work. **A Blu-ray has no such sixteen.** Every display set
/// carries its own colours in the stream -- as many as 256 of them, and a
/// different set each time -- so a pair written out of one has to invent the
/// palette its `.idx` puts at the top.
///
/// This is that palette, built as the subtitles go past. Each one is reduced
/// to the four colours a unit may carry (see [`unit`]) and each of those four
/// is written down here under the number the index will call it by. A colour
/// already written is used again, which is what keeps a stream of white text
/// with a black edge down to three or four entries over a whole film. Where
/// sixteen are spoken for, the nearest of them stands in for the seventeenth:
/// the format has no room for more, and a subtitle drawn in a colour a shade
/// off is a subtitle.
#[derive(Debug, Default)]
pub struct Ink {
    colours: Vec<u32>,
}

impl Ink {
    /// The number this colour is written under, adding it where there is
    /// still room and taking the nearest already written where there is not.
    ///
    /// A colour near enough to one already written is that one. It has to be:
    /// the four colours of a subtitle are an average of what it was drawn in,
    /// and the same white text averages a shade differently in every line of
    /// it. Written down as they came, one film's white would spend the whole
    /// palette on itself.
    pub fn intern(&mut self, rgb: (u8, u8, u8)) -> u8 {
        let mut best = (0usize, u32::MAX);
        for (at, &c) in self.colours.iter().enumerate() {
            let d = apart(unpack(c), rgb);
            if d < best.1 {
                best = (at, d);
            }
        }
        if best.1 <= NEAR || self.colours.len() == 16 {
            return best.0 as u8;
        }
        self.colours
            .push(((rgb.0 as u32) << 16) | ((rgb.1 as u32) << 8) | rgb.2 as u32);
        (self.colours.len() - 1) as u8
    }

    /// What has been written down, as the index writes it. The entries
    /// nobody asked for are black, and nothing points at them.
    pub fn palette(&self) -> Palette {
        let mut out = [0u32; 16];
        for (slot, &c) in out.iter_mut().zip(self.colours.iter()) {
            *slot = c;
        }
        Palette(out)
    }
}

/// How far apart two colours are, weighted the way the eye weights them:
/// green most, then red, then blue. Squared, because only the order matters.
fn apart(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    2 * d(a.0, b.0) + 4 * d(a.1, b.1) + 3 * d(a.2, b.2)
}

/// Near enough to be the same colour: eight levels of the 255 in each of the
/// three, by the measure above. Two greys that far apart are one grey to
/// anybody watching, and telling them apart costs an entry of the sixteen.
const NEAR: u32 = 576;

fn unpack(c: u32) -> (u8, u8, u8) {
    (
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

/// A picture reduced to what one unit may carry: four colours, how opaque
/// each is, and which of the four every colour of the original became.
///
/// Four is the format, not a choice. A DVD's unit says two bits per pixel and
/// names four entries of the sixteen in the index -- background, pattern, and
/// two emphases -- which is exactly what a subtitle is made of: nothing, the
/// letter, its edge, and the blend between them.
struct Slots {
    rgb: [(u8, u8, u8); 4],
    /// Opacity in the four bits a unit gives it: 0 invisible, 15 solid.
    alpha: [u8; 4],
    /// One entry per colour of the original, saying which slot it became.
    of: [u8; 256],
}

/// Opacity at or below which a colour is taken to be nothing at all, so that
/// the near-transparent fringe a decoder leaves around a letter does not
/// spend one of the three slots that are worth having.
const SHEER: u8 = 24;

/// Reduce a picture to four colours.
///
/// Slot zero is the background and is always transparent: a subtitle is
/// mostly nothing, and a unit whose background were opaque would draw a black
/// box across the film. The other three are found by clustering the colours
/// that are actually drawn -- weighted by how many pixels each covers, so a
/// letter counts for more than the two pixels of a stray -- starting from the
/// most used and then from whatever is furthest from what is already chosen.
///
/// Starting from the three most used would not do. The shades of one letter's
/// interior are used more than its edge is, and a subtitle drawn in three
/// shades of white with a black outline would come out as three whites with
/// no outline at all.
fn reduce(drawn: &Drawn) -> Slots {
    let mut count = [0usize; 256];
    for &i in &drawn.indices {
        count[i as usize] += 1;
    }
    // The colours that are drawn at all, and how much of the picture each is.
    let drawn_in: Vec<(usize, (u8, u8, u8), u8)> = (0..drawn.palette.len().min(256))
        .filter(|&i| count[i] > 0 && drawn.palette[i].3 > SHEER)
        .map(|i| {
            let (r, g, b, a) = drawn.palette[i];
            (i, (r, g, b), a)
        })
        .collect();

    let mut slots = Slots {
        rgb: [(0, 0, 0); 4],
        alpha: [0; 4],
        of: [0u8; 256],
    };
    if drawn_in.is_empty() {
        return slots;
    }

    // Where the clusters start. The most used colour, and then the colour
    // whose distance from everything already chosen, multiplied by how much
    // of the picture it covers, is greatest.
    let mut centres: Vec<((u8, u8, u8), u8)> = Vec::new();
    let first = drawn_in
        .iter()
        .max_by_key(|(i, _, _)| count[*i])
        .expect("not empty");
    centres.push((first.1, first.2));
    while centres.len() < 3 && centres.len() < drawn_in.len() {
        let pick = drawn_in
            .iter()
            .max_by_key(|(i, rgb, a)| {
                let near = centres
                    .iter()
                    .map(|c| apart(c.0, *rgb) + 4 * (c.1 as i32 - *a as i32).pow(2) as u32)
                    .min()
                    .unwrap_or(0);
                (near as u64) * (count[*i] as u64)
            })
            .expect("not empty");
        if centres.iter().any(|c| c.0 == pick.1 && c.1 == pick.2) {
            break;
        }
        centres.push((pick.1, pick.2));
    }

    // And then the usual settling: every colour to the nearest centre, every
    // centre to the middle of what came to it. Four passes, which is more
    // than three clusters over a few dozen colours ever needs.
    let mut belongs: Vec<usize> = vec![0; drawn_in.len()];
    for _ in 0..4 {
        for (k, (_, rgb, a)) in drawn_in.iter().enumerate() {
            belongs[k] = centres
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| apart(c.0, *rgb) + 4 * (c.1 as i32 - *a as i32).pow(2) as u32)
                .map_or(0, |(at, _)| at);
        }
        for (at, centre) in centres.iter_mut().enumerate() {
            let mut total = [0u64; 4];
            let mut weight = 0u64;
            for (k, (i, rgb, a)) in drawn_in.iter().enumerate() {
                if belongs[k] != at {
                    continue;
                }
                let n = count[*i] as u64;
                total[0] += rgb.0 as u64 * n;
                total[1] += rgb.1 as u64 * n;
                total[2] += rgb.2 as u64 * n;
                total[3] += *a as u64 * n;
                weight += n;
            }
            if weight == 0 {
                continue;
            }
            let mean = |v: u64| (v / weight) as u8;
            *centre = (
                (mean(total[0]), mean(total[1]), mean(total[2])),
                mean(total[3]),
            );
        }
    }

    for (at, (rgb, a)) in centres.iter().enumerate() {
        slots.rgb[at + 1] = *rgb;
        // Fifteen is solid and the format has no sixteen.
        slots.alpha[at + 1] = ((*a as u16 * 15 + 127) / 255) as u8;
    }
    for (k, (i, _, _)) in drawn_in.iter().enumerate() {
        slots.of[*i] = belongs[k] as u8 + 1;
    }
    slots
}

/// The unit that draws this picture, with its colours written into `ink`.
///
/// `None` where there is nothing to draw, or where the unit will not fit: a
/// sub-picture unit states its own length in two bytes, so 65535 is the whole
/// of what one may be. A subtitle never comes near it -- a line of text is a
/// few kilobytes run-length coded -- and a full-screen picture can, which is
/// why the caller is told rather than handed something a player will refuse.
///
/// The shape is the disc's own: the length, where the commands begin, the
/// picture in two fields, and one display control sequence that names the
/// four colours, says how opaque each is, says where on the screen it goes
/// and where in here its two fields are, and puts it up. A second sequence
/// takes it down again where the picture says when it should go; where it
/// says nothing, none is written and the subtitle stands until the next one
/// replaces it -- which is what a disc's own units do, and what the two ends
/// of a kept range are mended on the strength of.
pub fn unit(drawn: &Drawn, ink: &mut Ink) -> Option<Vec<u8>> {
    let (w, h) = (drawn.width as usize, drawn.height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let slots = reduce(drawn);
    if slots.alpha[1..].iter().all(|&a| a == 0) {
        return None;
    }
    let clut: Vec<u8> = slots
        .rgb
        .iter()
        .zip(slots.alpha.iter())
        // A slot nothing is drawn in is not worth an entry of the sixteen:
        // one film's subtitles would otherwise fill the palette with the
        // black of every background they never drew.
        .map(|(rgb, &a)| if a == 0 { 0 } else { ink.intern(*rgb) })
        .collect();

    let (x1, y1) = (drawn.x, drawn.y);
    // The area a unit states is inclusive at both ends, which is why a
    // subtitle one pixel wide has x2 == x1 rather than an empty span. Twelve
    // bits each is what it states them in: a 4K screen is inside that and
    // nothing a disc carries is outside it, but a picture that would be is
    // refused rather than wrapped around.
    let (x2, y2) = (x1 + drawn.width - 1, y1 + drawn.height - 1);
    if x2 > 0xFFF || y2 > 0xFFF {
        return None;
    }

    // Four bytes of header, then the picture in its two fields, each of them
    // starting on an even address as a disc's own units do.
    let mut out = vec![0u8; 4];
    let top_at = out.len();
    out.extend_from_slice(&field(drawn, &slots, 0));
    if out.len() % 2 != 0 {
        out.push(0xFF);
    }
    let bottom_at = out.len();
    out.extend_from_slice(&field(drawn, &slots, 1));
    if out.len() % 2 != 0 {
        out.push(0xFF);
    }
    let dcsqt = out.len();
    // The sequence below comes to twenty-four bytes: four of header, three
    // each for the colours and the contrast, seven for the area, five for
    // where the fields are, and one each to put it up and to end.
    let first_ends = dcsqt + 24;
    let next = if drawn.until.is_some() {
        first_ends
    } else {
        dcsqt // pointing at itself: the end
    };
    out.extend_from_slice(&0u16.to_be_bytes()); // at once
    out.extend_from_slice(&(next as u16).to_be_bytes());
    out.push(0x03);
    out.push((clut[3] << 4) | clut[2]);
    out.push((clut[1] << 4) | clut[0]);
    out.push(0x04);
    out.push((slots.alpha[3] << 4) | slots.alpha[2]);
    out.push((slots.alpha[1] << 4) | slots.alpha[0]);
    out.push(0x05);
    out.push((x1 >> 4) as u8);
    out.push((((x1 & 0x0F) << 4) | (x2 >> 8)) as u8);
    out.push(x2 as u8);
    out.push((y1 >> 4) as u8);
    out.push((((y1 & 0x0F) << 4) | (y2 >> 8)) as u8);
    out.push(y2 as u8);
    out.push(0x06);
    out.extend_from_slice(&(top_at as u16).to_be_bytes());
    out.extend_from_slice(&(bottom_at as u16).to_be_bytes());
    out.push(0x01); // put it up
    out.push(END);
    debug_assert_eq!(
        out.len(),
        first_ends,
        "the second sequence is placed by hand"
    );
    if let Some(after) = drawn.until {
        let delay = (after.max(0.0) / TICK).round().min(u16::MAX as f64) as u16;
        out.extend_from_slice(&delay.to_be_bytes());
        out.extend_from_slice(&(first_ends as u16).to_be_bytes()); // pointing at itself: the end
        out.push(STOP);
        out.push(END);
    }
    if out.len() > u16::MAX as usize {
        return None;
    }
    let size = out.len() as u16;
    out[0..2].copy_from_slice(&size.to_be_bytes());
    out[2..4].copy_from_slice(&(dcsqt as u16).to_be_bytes());
    Some(out)
}

/// One field of the picture, run-length coded: the even rows of it or the
/// odd ones.
///
/// A DVD keeps the two fields apart and addresses each on its own, which is
/// what a television drawing every other line wanted. Each row ends on a byte
/// boundary; a row that runs to its end in one colour says so with the code
/// that means "and the rest", which is most of them.
fn field(drawn: &Drawn, slots: &Slots, parity: usize) -> Vec<u8> {
    let (w, h) = (drawn.width as usize, drawn.height as usize);
    let mut nib = Nibbles::default();
    let mut y = parity;
    while y < h {
        let mut x = 0usize;
        while x < w {
            let at = |x: usize| {
                drawn
                    .indices
                    .get(y * w + x)
                    .map_or(0, |&i| slots.of[i as usize])
            };
            let colour = at(x);
            let mut run = 1usize;
            while x + run < w && at(x + run) == colour {
                run += 1;
            }
            if x + run == w && run > 15 {
                nib.rest(colour);
            } else {
                nib.run(colour, run);
            }
            x += run;
        }
        nib.align();
        y += 2;
    }
    nib.out
}

/// Four bits at a time, which is how a unit's picture is written.
#[derive(Default)]
struct Nibbles {
    out: Vec<u8>,
    /// Whether the last byte is half written.
    half: bool,
}

impl Nibbles {
    fn nibble(&mut self, v: u8) {
        if self.half {
            if let Some(b) = self.out.last_mut() {
                *b |= v & 0x0F;
            }
            self.half = false;
        } else {
            self.out.push((v & 0x0F) << 4);
            self.half = true;
        }
    }

    /// A run of one colour, in the shortest of the four codes that holds it.
    ///
    /// The codes are told apart by how many zeroes they open with, which caps
    /// a single one at 255 pixels; a longer run is written as several.
    fn run(&mut self, colour: u8, mut len: usize) {
        while len > 0 {
            let take = len.min(255);
            let v = ((take as u16) << 2) | colour as u16;
            match take {
                0..=3 => self.nibble(v as u8),
                4..=15 => {
                    self.nibble((v >> 4) as u8);
                    self.nibble(v as u8);
                }
                16..=63 => {
                    self.nibble(0);
                    self.nibble((v >> 4) as u8);
                    self.nibble(v as u8);
                }
                _ => {
                    self.nibble(0);
                    self.nibble((v >> 8) as u8);
                    self.nibble((v >> 4) as u8);
                    self.nibble(v as u8);
                }
            }
            len -= take;
        }
    }

    /// This colour to the end of the row, however far that is: a length of
    /// zero in the longest of the codes, which is what says so.
    fn rest(&mut self, colour: u8) {
        self.nibble(0);
        self.nibble(0);
        self.nibble(0);
        self.nibble(colour);
    }

    /// End the row. A row starts on a byte of its own.
    fn align(&mut self) {
        self.half = false;
    }
}

/// One subtitle stream: what it is called, and where each of its units went.
struct Stream {
    id: u8,
    language: Option<String>,
    at: Vec<(f64, u64)>,
}

/// The pair being built.
pub struct Sidecar {
    width: u16,
    height: u16,
    palette: Palette,
    streams: Vec<Stream>,
    sub: Vec<u8>,
}

impl Sidecar {
    pub fn new(width: u16, height: u16, palette: Palette) -> Sidecar {
        Sidecar {
            width,
            height,
            palette,
            streams: Vec::new(),
            sub: Vec::new(),
        }
    }

    /// Say a stream exists before anything of it has been written.
    ///
    /// So that the order of the streams in the index is the order the disc
    /// declares them in, rather than the order in which each happened to
    /// send its first subtitle -- which on a film with a commentary track is
    /// not the same thing.
    pub fn declare(&mut self, id: u8, language: Option<String>) {
        if !self.streams.iter().any(|s| s.id == id) {
            self.streams.push(Stream {
                id,
                language,
                at: Vec::new(),
            });
        }
    }

    /// Write the palette over, for a pair whose colours were invented on the
    /// way out rather than read off a disc. See [`Ink`].
    ///
    /// Late, because it has to be: a palette built as the subtitles go past
    /// is not finished until the last of them has gone past, and the index is
    /// written after that.
    pub fn recolour(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Whether anything at all was written. Nothing means no files.
    pub fn is_empty(&self) -> bool {
        self.streams.iter().all(|s| s.at.is_empty())
    }

    /// How many units each stream carried, for the note that says so.
    pub fn counts(&self) -> Vec<(u8, usize)> {
        self.streams.iter().map(|s| (s.id, s.at.len())).collect()
    }

    /// Add one subtitle, shown at `at` seconds on the cut's own timeline.
    pub fn add(&mut self, id: u8, at: f64, spu: &[u8]) {
        self.declare(id, None);
        let filepos = self.sub.len() as u64;
        self.write_unit(id, at, spu);
        if let Some(s) = self.streams.iter_mut().find(|s| s.id == id) {
            s.at.push((at.max(0.0), filepos));
        }
    }

    /// Put one unit into the `.sub` as the program stream packets it
    /// travelled in, starting on a sector of its own.
    ///
    /// The framing is the disc's: a pack header, then private stream 1
    /// carrying the unit's own id in front of its bytes, and padding to the
    /// end of the sector. A reader seeks to the position the index gives and
    /// demuxes forward from there, so the pack header has to be the first
    /// thing it meets.
    fn write_unit(&mut self, id: u8, at: f64, spu: &[u8]) {
        let stamp = (at.max(0.0) * 90_000.0).round() as u64;
        let mut rest = spu;
        let mut first = true;
        while !rest.is_empty() {
            let mut sector = Vec::with_capacity(SECTOR);
            sector.extend_from_slice(&pack_header(stamp));
            // What is left of the sector once the packet's own header and
            // the substream byte are in it. The header is nine bytes and
            // fourteen with a timestamp.
            let header = if first { 9 + 5 } else { 9 };
            let room = SECTOR - sector.len() - header - 1;
            let take = rest.len().min(room);
            let payload = &rest[..take];
            let length = header - 6 + 1 + take;
            sector.extend_from_slice(&[0x00, 0x00, 0x01, 0xBD]);
            sector.extend_from_slice(&(length as u16).to_be_bytes());
            sector.push(0x81); // no scrambling, original
            sector.push(if first { 0x80 } else { 0x00 });
            sector.push(if first { 5 } else { 0 });
            if first {
                sector.extend_from_slice(&timestamp(0b0010, stamp));
            }
            sector.push(id);
            sector.extend_from_slice(payload);
            // The rest of the sector is padding, which every reader skips.
            if sector.len() < SECTOR {
                let pad = SECTOR - sector.len();
                sector.extend_from_slice(&[0x00, 0x00, 0x01, 0xBE]);
                if pad >= 6 {
                    sector.extend_from_slice(&(pad as u16 - 6).to_be_bytes());
                    sector.resize(SECTOR, 0xFF);
                } else {
                    // No room for a padding packet's own header: the four
                    // bytes just written are as far as this can go.
                    sector.resize(SECTOR, 0xFF);
                }
            }
            sector.truncate(SECTOR);
            self.sub.extend_from_slice(&sector);
            rest = &rest[take..];
            first = false;
        }
    }

    /// The index, as the text a player reads.
    pub fn index(&self) -> String {
        let mut out = String::new();
        out.push_str("# VobSub index file, v7 (do not modify this line!)\n");
        out.push_str("#\n# Written by SmartCut, beside the cut these belong to.\n#\n");
        out.push_str(&format!("size: {}x{}\n", self.width, self.height));
        let colours: Vec<String> = self.palette.0.iter().map(|c| format!("{c:06x}")).collect();
        out.push_str(&format!("palette: {}\n", colours.join(", ")));
        out.push_str("langidx: 0\n");
        for (index, s) in self.streams.iter().enumerate() {
            // Two letters is what the format takes, and a disc says three.
            let lang = s.language.as_deref().unwrap_or("--");
            out.push_str(&format!(
                "\nid: {}, index: {index}\n",
                &lang[..lang.len().min(2)]
            ));
            for &(at, filepos) in &s.at {
                out.push_str(&format!(
                    "timestamp: {}, filepos: {filepos:09x}\n",
                    clock(at)
                ));
            }
        }
        out
    }

    /// Write the pair. `beside` is the cut itself; the two files take its
    /// name with their own extensions, which is what makes a player find
    /// them without being told.
    pub fn write(&self, beside: &str) -> Result<(String, String)> {
        let stem = std::path::Path::new(beside).with_extension("");
        let idx = stem.with_extension("idx");
        let sub = stem.with_extension("sub");
        std::fs::File::create(&idx)?.write_all(self.index().as_bytes())?;
        std::fs::File::create(&sub)?.write_all(&self.sub)?;
        Ok((
            idx.to_string_lossy().into_owned(),
            sub.to_string_lossy().into_owned(),
        ))
    }
}

/// `HH:MM:SS:mmm`, which is the one format the index writes a time in.
fn clock(at: f64) -> String {
    let ms = (at.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}:{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// A program stream pack header, carrying the clock the packets after it are
/// read against.
fn pack_header(stamp: u64) -> [u8; 14] {
    let mut out = [0u8; 14];
    out[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
    // The same 33 bits a timestamp carries, in the shape a pack writes them:
    // '01', three bits, marker, fifteen, marker, fifteen, marker.
    let scr = stamp;
    out[4] = 0x44 | (((scr >> 30) & 0x07) as u8) << 3 | ((scr >> 28) & 0x03) as u8;
    out[5] = ((scr >> 20) & 0xFF) as u8;
    out[6] = 0x04 | (((scr >> 15) & 0x1F) as u8) << 3 | ((scr >> 13) & 0x03) as u8;
    out[7] = ((scr >> 5) & 0xFF) as u8;
    out[8] = 0x04 | ((scr & 0x1F) as u8) << 3;
    out[9] = 0x01; // the extension, which nothing here uses
                   // The rate the stream is read at, in units of 50 bytes a second. A
                   // DVD's own figure, which is what a player sizes its buffer from.
    let rate: u32 = 25200;
    out[10] = ((rate >> 14) & 0xFF) as u8;
    out[11] = ((rate >> 6) & 0xFF) as u8;
    out[12] = (((rate & 0x3F) as u8) << 2) | 0x03;
    out[13] = 0xF8; // reserved, and no stuffing
    out
}

/// A presentation time in the five bytes a packet writes it in.
fn timestamp(marker: u8, stamp: u64) -> [u8; 5] {
    [
        (marker << 4) | (((stamp >> 30) & 0x07) as u8) << 1 | 0x01,
        ((stamp >> 22) & 0xFF) as u8,
        ((((stamp >> 15) & 0x7F) as u8) << 1) | 0x01,
        ((stamp >> 7) & 0xFF) as u8,
        (((stamp & 0x7F) as u8) << 1) | 0x01,
    ]
}

fn u16be(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([
        b.get(at).copied().unwrap_or(0),
        b.get(at + 1).copied().unwrap_or(0),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit that puts a picture up and never says when to take it down,
    /// which is what the disc measured here sends.
    fn standing(size: u16) -> Vec<u8> {
        let mut out = vec![0u8; size as usize];
        let dcsqt = size as usize - 12;
        out[0..2].copy_from_slice(&size.to_be_bytes());
        out[2..4].copy_from_slice(&(dcsqt as u16).to_be_bytes());
        out[dcsqt..dcsqt + 2].copy_from_slice(&0u16.to_be_bytes());
        out[dcsqt + 2..dcsqt + 4].copy_from_slice(&(dcsqt as u16).to_be_bytes());
        out[dcsqt + 4] = 0x01; // put it up
        out[dcsqt + 5] = END;
        out
    }

    /// And one that does say.
    fn timed(size: u16, delay: u16) -> Vec<u8> {
        let mut out = standing(size);
        let first = u16be(&out, 2) as usize;
        let second = out.len();
        out[first + 2..first + 4].copy_from_slice(&(second as u16).to_be_bytes());
        out.extend_from_slice(&delay.to_be_bytes());
        out.extend_from_slice(&(second as u16).to_be_bytes());
        out.push(STOP);
        out.push(END);
        let grown = out.len() as u16;
        out[0..2].copy_from_slice(&grown.to_be_bytes());
        out
    }

    #[test]
    fn reads_when_a_unit_takes_itself_down() {
        assert!(stops_after(&standing(64)).is_none());
        let after = stops_after(&timed(64, 200)).expect("this one says");
        assert!((after - 200.0 * TICK).abs() < 1e-9);
    }

    /// A unit that never stops is given a sequence that stops it, and the
    /// chain that led there is pointed at it.
    #[test]
    fn a_standing_unit_can_be_taken_down() {
        let out = stopped_after(&standing(64), 1.5);
        let after = stops_after(&out).expect("now it says");
        assert!((after - 1.5).abs() < TICK);
        assert_eq!(
            u16be(&out, 0) as usize,
            out.len(),
            "the length has to grow with it"
        );
        // The picture is untouched: only the tail is new.
        assert_eq!(&out[4..52], &standing(64)[4..52]);
    }

    /// One that already stops has the delay rewritten instead of a second
    /// sequence added.
    #[test]
    fn a_timed_unit_is_taken_down_earlier() {
        let unit = timed(64, 400);
        let out = stopped_after(&unit, 1.0);
        assert_eq!(out.len(), unit.len(), "nothing is added to one that says");
        let after = stops_after(&out).expect("still says");
        assert!((after - 1.0).abs() < TICK);
    }

    /// And one that stops of its own accord before the cut asks it to is
    /// left exactly as it arrived.
    #[test]
    fn a_unit_that_stops_first_is_left_alone() {
        let unit = timed(64, 40);
        assert_eq!(stopped_after(&unit, 5.0), unit);
    }

    /// The unit that ends a range says stop and nothing else.
    #[test]
    fn a_range_can_be_ended() {
        let unit = take_down();
        assert_eq!(unit.len(), 10);
        assert_eq!(u16be(&unit, 0) as usize, unit.len());
        assert_eq!(stops_after(&unit), Some(0.0));
    }

    /// Every unit starts on a sector of its own, and the index says which.
    #[test]
    fn every_unit_starts_on_a_sector() {
        let mut side = Sidecar::new(720, 480, Palette::grey());
        side.add(0x20, 1.0, &standing(64));
        side.add(0x20, 2.0, &standing(64));
        assert_eq!(side.sub.len(), 2 * SECTOR);
        let idx = side.index();
        assert!(
            idx.contains("timestamp: 00:00:01:000, filepos: 000000000"),
            "{idx}"
        );
        assert!(
            idx.contains("timestamp: 00:00:02:000, filepos: 000000800"),
            "{idx}"
        );
    }

    /// A unit too big for one sector is split across as many as it needs,
    /// and the index still points at the first.
    #[test]
    fn a_long_unit_spans_sectors() {
        let mut side = Sidecar::new(720, 480, Palette::grey());
        side.add(0x20, 1.0, &standing(5000));
        assert_eq!(side.sub.len(), 3 * SECTOR);
        assert!(side.index().contains("filepos: 000000000"));
    }

    /// The streams are named in the order they were declared, whatever order
    /// they turn up in.
    #[test]
    fn the_index_names_every_stream_it_was_told_about() {
        let mut side = Sidecar::new(720, 480, Palette::grey());
        side.declare(0x20, Some("eng".into()));
        side.declare(0x21, Some("jpn".into()));
        side.add(0x21, 1.0, &standing(64));
        let idx = side.index();
        assert!(idx.contains("id: en, index: 0"), "{idx}");
        assert!(idx.contains("id: jp, index: 1"), "{idx}");
        assert!(!side.is_empty());
    }

    /// A picture, in the shape a decoder hands one over: four colours, one
    /// of them nothing, and a border drawn in one of the other three so that
    /// nothing is cropped off the edges on the way back.
    ///
    /// A decoder trims the fully transparent rows and columns from around a
    /// subtitle -- which is right, and would make a round trip compare two
    /// different rectangles.
    fn picture(width: u16, height: u16) -> Drawn {
        let (w, h) = (width as usize, height as usize);
        let mut indices = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
                indices[y * w + x] = if edge {
                    2
                } else if x % 37 == 0 {
                    3
                } else if (x / 5 + y / 3) % 3 == 0 {
                    1
                } else {
                    0
                };
            }
        }
        Drawn {
            x: 40,
            y: 100,
            width,
            height,
            indices,
            palette: vec![
                (0, 0, 0, 0),         // nothing
                (255, 255, 255, 255), // the letter
                (16, 24, 200, 255),   // its edge
                (200, 190, 32, 153),  // and a blend, half there
            ],
            until: None,
        }
    }

    /// The encoder and libavcodec's own decoder, back to back. Every pixel
    /// comes back the colour it went in as, at the same place on the screen.
    ///
    /// This is the test the format is checked by: a unit written wrong is
    /// read wrong here, and reading is not this program's own code.
    fn round_trip(width: u16, height: u16) {
        let drawn = picture(width, height);
        let mut ink = Ink::default();
        let spu = unit(&drawn, &mut ink).expect("small enough to be a unit");
        assert_eq!(u16be(&spu, 0) as usize, spu.len(), "the length is its own");
        let mut reader = Reader::open(&ink.palette(), (720, 480)).expect("a decoder");
        let back = reader
            .read(&spu)
            .expect("decodes")
            .expect("and draws something");
        assert_eq!(
            (back.x, back.y, back.width, back.height),
            (drawn.x, drawn.y, drawn.width, drawn.height),
            "the same rectangle of the screen"
        );
        assert_eq!(back.indices.len(), drawn.indices.len());
        for (n, (&before, &after)) in drawn.indices.iter().zip(back.indices.iter()).enumerate() {
            let want = drawn.palette[before as usize];
            let got = back.palette[after as usize];
            // A pixel that draws nothing has no colour to be compared: what
            // it points at is whatever the background slot was left naming,
            // and what makes it nothing is the opacity below.
            if want.3 > 0 {
                assert_eq!(
                    (want.0, want.1, want.2),
                    (got.0, got.1, got.2),
                    "pixel {n} of {width}x{height} came back another colour"
                );
            }
            // Opacity survives as the four bits a unit gives it, which is
            // seventeen steps of the 255 it went in with.
            assert!(
                want.3.abs_diff(got.3) <= 17,
                "pixel {n} came back {} opaque, not {}",
                got.3,
                want.3
            );
        }
    }

    #[test]
    fn a_small_picture_survives_being_written_as_a_unit() {
        round_trip(48, 20);
    }

    /// The same, wide enough that the runs need every code the format has --
    /// including the one that means "and the rest of the row".
    #[test]
    fn a_wide_picture_survives_being_written_as_a_unit() {
        round_trip(720, 48);
    }

    /// A unit written from a picture that says when it goes says so too, in
    /// the second sequence, and one that says nothing carries only the first.
    #[test]
    fn a_written_unit_can_say_when_it_goes() {
        let mut ink = Ink::default();
        let standing = unit(&picture(32, 16), &mut ink).expect("a unit");
        assert_eq!(stops_after(&standing), None, "nothing says when this goes");
        let mut timed = picture(32, 16);
        timed.until = Some(2.5);
        let timed = unit(&timed, &mut ink).expect("a unit");
        let after = stops_after(&timed).expect("this one says");
        assert!((after - 2.5).abs() < TICK, "{after}");
        assert_eq!(u16be(&timed, 0) as usize, timed.len());
    }

    /// A picture that draws nothing is not a unit. What clears a screen is
    /// [`take_down`], which is ten bytes rather than a picture of nothing.
    #[test]
    fn a_picture_of_nothing_is_not_written() {
        let mut ink = Ink::default();
        let empty = Drawn {
            x: 0,
            y: 0,
            width: 16,
            height: 8,
            indices: vec![0; 16 * 8],
            palette: vec![(0, 0, 0, 0), (255, 255, 255, 255)],
            until: None,
        };
        assert!(unit(&empty, &mut ink).is_none());
    }

    /// The palette fills up as colours are met, keeps the number it gave a
    /// colour the first time, and stands the nearest of the sixteen in for
    /// the seventeenth.
    #[test]
    fn the_palette_is_built_as_the_colours_are_met() {
        let mut ink = Ink::default();
        assert_eq!(ink.intern((255, 255, 255)), 0);
        assert_eq!(ink.intern((0, 0, 0)), 1);
        assert_eq!(
            ink.intern((255, 255, 255)),
            0,
            "the same colour, the same id"
        );
        assert_eq!(
            ink.intern((252, 253, 255)),
            0,
            "and near enough is the same"
        );
        for i in 2..16u8 {
            assert_eq!(ink.intern((0, i * 16, 0)), i);
        }
        // Full. A colour nothing like the rest is written as the nearest.
        assert_eq!(ink.intern((250, 250, 250)), 0);
        assert_eq!(ink.palette().0[0], 0xffffff);
        assert_eq!(ink.palette().0[1], 0x000000);
    }

    /// Three colours and a background is what a unit holds, and a picture
    /// drawn in more is reduced to them rather than refused.
    #[test]
    fn a_picture_of_many_colours_is_reduced_to_four() {
        let (w, h) = (64usize, 16usize);
        let mut palette = vec![(0, 0, 0, 0)];
        // A white letter, a black edge, and thirty shades between them,
        // which is what a decoder hands over for antialiased text.
        for i in 0..30u8 {
            let v = i * 8;
            palette.push((v, v, v, 255));
        }
        let mut indices = vec![0u8; w * h];
        for (n, slot) in indices.iter_mut().enumerate() {
            *slot = if n % 7 == 0 { 0 } else { (n % 30 + 1) as u8 };
        }
        let drawn = Drawn {
            x: 0,
            y: 0,
            width: w as u16,
            height: h as u16,
            indices,
            palette,
            until: None,
        };
        let mut ink = Ink::default();
        let spu = unit(&drawn, &mut ink).expect("a unit");
        assert!(
            ink.palette().0.iter().filter(|&&c| c != 0).count() <= 3,
            "one picture cannot spend more than three of the sixteen"
        );
        let mut reader = Reader::open(&ink.palette(), (720, 480)).expect("a decoder");
        let back = reader.read(&spu).expect("decodes").expect("draws");
        assert_eq!(back.width, w as u16);
        // The shades came back as the nearest of the three that were kept,
        // which is what four colours means -- but nothing became nothing.
        for (&before, &after) in drawn.indices.iter().zip(back.indices.iter()) {
            let want = drawn.palette[before as usize];
            let got = back.palette[after as usize];
            assert_eq!(want.3 == 0, got.3 == 0, "a drawn pixel stayed drawn");
        }
    }

    /// The palette is the disc's, converted to what the index writes.
    #[test]
    fn the_palette_comes_out_as_colours() {
        // Black, white, and a red: luma, then the two colour differences.
        let clut = [0, 16, 128, 128, 0, 235, 128, 128, 0, 81, 240, 90];
        let p = Palette::from_clut(&clut);
        assert_eq!(p.0[0], 0x000000);
        assert_eq!(p.0[1], 0xffffff);
        assert!((p.0[2] >> 16).abs_diff(0xfd) <= 2, "red, near enough");
        assert!((p.0[2] & 0xFFFF) < 0x0808, "and red only");
        // Nothing said about the rest, and something legible written anyway.
        assert_eq!(p.0[15], 0x000000);
    }
}
