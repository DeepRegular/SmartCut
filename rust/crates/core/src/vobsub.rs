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
//! There are two things to do with one of these, and this module holds both.
//! One is to write it out unchanged, which is what everything above is for.
//! The other is to **read the picture out of it** so that it can be written
//! again as the kind of subtitle a transport stream does carry -- see
//! [`Reader`], and [`crate::pgs::write`] for the other half of that.
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
const TICK: f64 = 1024.0 / 90_000.0;

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

/// One subtitle, decoded: the picture a unit was carrying.
///
/// An index per pixel and the colours those index into, which is the shape
/// both formats keep a subtitle in -- so what [`crate::pgs::write`] does
/// with this is spell the same pixels differently.
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
        // How long the unit says it stands. The decoder counts from the
        // moment it appears, in milliseconds, and says nothing by saying
        // either nothing or everything.
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
            return Ok(Some(drawn));
        }
        Ok(None)
    }
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
