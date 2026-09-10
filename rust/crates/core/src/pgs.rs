//! The graphics a Blu-ray draws its subtitles with, and what a cut owes them
//! at either end of a kept range.
//!
//! A broadcast writes its subtitles: each ARIB statement is one packet that
//! says what to put on the plane, and carrying one across a cut is carrying
//! one packet. A disc draws them instead. What travels is a *display set* --
//! a composition saying where things go, a window, a palette, and the
//! picture itself run-length coded -- and each piece is its own PES packet
//! with its own timestamp:
//!
//!   PCS  WDS  PDS  ODS  END      the subtitle appears
//!   PCS  WDS  END                and later, the plane is cleared
//!
//! So the packets are timed the way a caption's are and travel the same way,
//! but they are timed *in groups*, and a group means nothing in halves. That
//! is the whole of the difference, and it comes to three rules:
//!
//!   * a display set is carried whole or not at all,
//!   * what was on screen when a range opens has to be put up again, since
//!     the set that drew it was left behind with the material before the cut,
//!   * and what is on screen when a range ends has to be taken down, since
//!     the set that would have cleared it was left behind with the material
//!     after the cut.
//!
//! Both ends are answered with bytes already in the stream. The set to put
//! up is one that was read and held; the set to take down is the standing
//! one's own composition with its objects removed, which is what the disc
//! itself sends to clear a plane.
//!
//! ## What a disc actually sends
//!
//! Measured over half an hour of one disc's feature: 1334 display sets, of
//! which 386 open an epoch, 561 are acquisition points -- the same subtitle
//! sent again, whole, so that a player joining mid-way has it -- and 387
//! clear the plane. A subtitle stands for 2.3 seconds at the median and for
//! 116 at the longest, but the gap between one self-contained set and the
//! next never exceeds 2.8 seconds: however long a subtitle stays up, this
//! disc keeps re-sending it.
//!
//! That is what makes the opening rule cheap. The set to put up again is
//! nearly always a few seconds back, and [`LOOKBACK`] is the window the
//! cutter reads before a range for it. Not every disc sends acquisition
//! points, and on one that does not a subtitle standing longer than the
//! window is lost at that one boundary -- which is what happens today to
//! every subtitle on every disc.

use anyhow::Result;
use std::io::Write;

/// A composition: where the objects go and what state the epoch is in.
pub const PCS: u8 = 0x16;
/// A window: the rectangle of the plane the composition draws inside.
pub const WDS: u8 = 0x17;
/// The end of a display set. Every set has one and it carries nothing.
pub const END: u8 = 0x80;

/// How far before a kept range the graphics are read, so that whatever is on
/// screen at the cut can be put up again.
///
/// Eight seconds, from what discs do rather than from what the format
/// allows: the disc measured in this module's notes never leaves more than
/// 2.8 seconds between one self-contained display set and the next, and 99
/// subtitles in 100 stand for less than 6 seconds. So this covers a disc
/// that re-sends its subtitles several times over, and covers all but the
/// longest-standing subtitle on a disc that never re-sends one at all.
///
/// It is read, not decoded: eight seconds of a disc's stream is a second of
/// reading, once per kept range, and only where the recording has graphics
/// in it at all.
pub const LOOKBACK: f64 = 8.0;

/// One packet of a display set, held for as long as it may need to be sent
/// again.
///
/// The times are the recording's own, rebased: seconds from the start of the
/// recording, which is what everything else in the cutter is measured in.
#[derive(Debug, Clone)]
pub struct Held {
    pub pts: f64,
    pub dts: f64,
    pub data: Vec<u8>,
}

/// The segments one packet carries.
///
/// Usually exactly one -- a disc gives every segment its own PES packet --
/// but the format allows several to share one and a demuxer hands over what
/// it was given, so this reads a run rather than a single header.
pub fn segments(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let head = data.get(at..at.checked_add(3)?)?;
        let len = u16::from_be_bytes([head[1], head[2]]) as usize;
        let body = data.get(at + 3..at + 3 + len)?;
        at += 3 + len;
        Some((head[0], body))
    })
}

/// What a composition says about itself.
///
/// Only the three fields a cut has to act on, out of the eleven bytes every
/// composition opens with: the screen it is drawn on, the rate it is shown
/// at, its own number, its state, its palette, and how many objects it puts
/// up. `state` is why a set can be put up again at all -- an epoch start or
/// an acquisition point carries its own palette and its own picture, and a
/// normal case does not: it changes a composition whose pieces arrived
/// earlier and cannot stand alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Composition {
    pub number: u16,
    pub state: u8,
    pub objects: u8,
}

/// A composition that carries everything it needs: an epoch start.
pub const EPOCH_START: u8 = 0x80;
/// The same subtitle sent again, whole, for a player that joined late.
pub const ACQUISITION: u8 = 0x40;

impl Composition {
    /// Whether this set can be put up on its own, without whatever came
    /// before it.
    pub fn self_contained(&self) -> bool {
        matches!(self.state, EPOCH_START | ACQUISITION)
    }
}

/// Read a composition off a PCS body. `None` where the body is too short to
/// be one, which is a damaged recording rather than a format this does not
/// know: the fields read here have been in the same places since the format
/// was written.
pub fn composition(body: &[u8]) -> Option<Composition> {
    Some(Composition {
        number: u16::from_be_bytes([*body.get(5)?, *body.get(6)?]),
        state: *body.get(7)? & 0xC0,
        objects: *body.get(10)?,
    })
}

/// Rewrite a composition body as the one that clears the plane.
///
/// The disc's own clear is this and nothing else: the same screen, the same
/// palette, a fresh composition number, no objects. Copying the standing
/// composition rather than writing eleven bytes from nothing keeps the
/// screen size and the frame rate the disc declared, which are the two
/// fields a player has been known to check.
fn cleared(body: &[u8]) -> Vec<u8> {
    let mut out = body[..11.min(body.len())].to_vec();
    out.resize(11, 0);
    let next = composition(body).map_or(0, |c| c.number.wrapping_add(1));
    out[5..7].copy_from_slice(&next.to_be_bytes());
    out[7] = 0x00; // a normal case: the epoch continues, with nothing in it
    out[8] = 0x00; // and no palette update
    out[10] = 0x00; // and no objects, which is what empties the plane
    out
}

/// One segment wrapped back up as a packet's worth of bytes.
fn packed(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + body.len());
    out.push(kind);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// What one graphics stream has on screen, and what it would take to say so
/// again.
///
/// Fed every packet of the stream in the order the recording carries them,
/// including the ones before a kept range begins -- which is the point: the
/// set that drew what is on screen at the cut is one of those.
#[derive(Debug, Default)]
pub struct Plane {
    /// Packets of the set being read, which is not a set until its END.
    building: Vec<Held>,
    /// The composition and the window that set carries, as they arrive.
    /// A set with no composition in it is not a set at all: that is what a
    /// read joining half way through hands over.
    opening: Option<Vec<u8>>,
    framing: Option<Vec<u8>>,
    /// Whether the last complete set left anything on screen.
    occupied: bool,
    /// The last complete set that put something up and could put it up
    /// again. Kept whole, because putting it up again is sending it again.
    standing: Option<Vec<Held>>,
    /// The composition and window of the last complete set, whatever it did,
    /// which is what the clear is built out of. Taken from the last rather
    /// than from the standing one so that a composition which moved a
    /// subtitle is cleared out of the window it was moved to.
    last: Option<(Vec<u8>, Option<Vec<u8>>)>,
}

impl Plane {
    /// Forget everything, for a read that starts somewhere else.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Take one packet of the stream, and hand back the display set it
    /// completes.
    ///
    /// Nothing comes back until the END arrives, which is what makes a set
    /// the unit a cut carries: what is handed over is whole or is not handed
    /// over. A read that begins in the middle of one contributes to nothing
    /// and changes nothing.
    ///
    /// The same packets may be fed twice -- the lookback and the range's own
    /// read overlap -- and a set completed twice is the same set with the
    /// same effect on the plane. What decides whether it is *written* twice
    /// is where its first packet falls, which is the caller's business.
    pub fn feed(&mut self, held: Held) -> Option<Vec<Held>> {
        let mut ends = false;
        for (kind, body) in segments(&held.data) {
            match kind {
                // A composition opens a set. Anything still being built when
                // one arrives was never finished -- a read that began in the
                // middle of a set -- and is not part of this one.
                PCS => {
                    self.building.clear();
                    self.opening = Some(body.to_vec());
                    self.framing = None;
                }
                WDS => self.framing = Some(body.to_vec()),
                END => ends = true,
                _ => {}
            }
        }
        self.building.push(held);
        if !ends {
            return None;
        }
        let set = std::mem::take(&mut self.building);
        let opening = self.opening.take()?;
        let framing = self.framing.take();
        let c = composition(&opening)?;
        self.occupied = c.objects > 0;
        if c.objects > 0 && c.self_contained() {
            self.standing = Some(set.clone());
        }
        self.last = Some((opening, framing));
        Some(set)
    }

    /// Whether anything is on screen.
    pub fn occupied(&self) -> bool {
        self.occupied
    }

    /// Whether a display set is part-read: packets have arrived and the END
    /// that completes them has not.
    ///
    /// What a reader does with this is finish the set before it stops. A set
    /// straddling the end of a stretch being read is one set, and the reader
    /// that began it is the only one that will ever have all of it: the next
    /// read starts inside it and, having missed the composition, throws the
    /// rest away.
    pub fn building(&self) -> bool {
        !self.building.is_empty()
    }

    /// The set to send again so that a range opens with what was on screen,
    /// timed to land at `at`.
    ///
    /// The set keeps its own internal spacing -- a display set decodes over
    /// several milliseconds and its pieces say so -- and is moved bodily so
    /// that the earliest of those instants is `at`. So the subtitle appears
    /// the same fraction of a second into the range that it would have taken
    /// to decode anywhere else, which is under a tenth of one.
    ///
    /// The composition is marked as opening an epoch whatever it was before.
    /// It is: nothing of this stream reached the output before it.
    pub fn replay(&self, at: f64) -> Vec<Held> {
        let Some(set) = self.standing.as_ref().filter(|_| self.occupied) else {
            return Vec::new();
        };
        let first = set
            .iter()
            .map(|h| h.dts.min(h.pts))
            .fold(f64::INFINITY, f64::min);
        if !first.is_finite() {
            return Vec::new();
        }
        set.iter()
            .map(|h| {
                let mut data = h.data.clone();
                // The one byte that is not a copy. See above.
                let state_at = segments(&data)
                    .next()
                    .and_then(|(kind, body)| (kind == PCS && body.len() > 7).then_some(3 + 7));
                if let Some(at) = state_at {
                    data[at] = EPOCH_START;
                }
                Held {
                    pts: at + (h.pts - first),
                    dts: at + (h.dts - first),
                    data,
                }
            })
            .collect()
    }

    /// The set that takes down what is on screen, timed to land at `at`.
    ///
    /// Empty where the plane is already clear, which is the ordinary case: a
    /// range that ends between two subtitles needs nothing.
    pub fn clear(&self, at: f64) -> Vec<Held> {
        if !self.occupied {
            return Vec::new();
        }
        let Some((opening, framing)) = self.last.as_ref() else {
            return Vec::new();
        };
        let mut out = vec![packed(PCS, &cleared(opening))];
        if let Some(window) = framing {
            out.push(packed(WDS, window));
        }
        out.push(packed(END, &[]));
        out.into_iter()
            .map(|data| Held {
                pts: at,
                dts: at,
                data,
            })
            .collect()
    }
}

/// A PGS elementary stream, written beside the cut.
///
/// The third destination, and the one that converts nothing at all. A `.sup`
/// is what a display set looks like outside a container: every segment as it
/// travelled, each behind ten bytes saying when it is decoded and when it is
/// shown. What goes in is the disc's own bytes, so a Blu-ray's subtitles come
/// out of this byte for byte -- and BDSup2Sub, Subtitle Edit and everything
/// else that works on a disc's subtitles read it without being taught
/// anything.
///
/// ```text
/// "PG"  pts  dts  type  length  [ the segment ]
///  2     4    4    1      2
/// ```
///
/// It holds **one stream**, where the pair a DVD's subtitles go in holds as
/// many as the disc had. A recording with two of them is written as two
/// files; see [`Sup::write`].
#[derive(Debug)]
pub struct Sup {
    /// What the file is called beside the cut, where the cut's own name will
    /// not do because another stream has taken it. See [`Sup::write`].
    tag: Option<String>,
    out: Vec<u8>,
    segments: usize,
}

impl Sup {
    pub fn new(tag: Option<String>) -> Sup {
        Sup {
            tag,
            out: Vec::new(),
            segments: 0,
        }
    }

    /// Take one packet of a display set, moved onto the output's clock by
    /// `offset`.
    ///
    /// A packet is usually one segment and the format allows it to be
    /// several, so this writes a header per segment rather than per packet.
    /// Both times are the packet's: every segment of a set is handed over
    /// and shown together, which is what the set being a set means.
    pub fn take(&mut self, held: &Held, offset: f64) {
        let stamp = |t: f64| {
            ((t + offset).max(0.0) * 90_000.0)
                .round()
                .min(u32::MAX as f64) as u32
        };
        let (pts, dts) = (stamp(held.pts), stamp(held.dts));
        for (kind, body) in segments(&held.data) {
            self.out.extend_from_slice(b"PG");
            self.out.extend_from_slice(&pts.to_be_bytes());
            self.out.extend_from_slice(&dts.to_be_bytes());
            self.out.push(kind);
            self.out
                .extend_from_slice(&(body.len() as u16).to_be_bytes());
            self.out.extend_from_slice(body);
            self.segments += 1;
        }
    }

    /// Whether anything at all was written. Nothing means no file.
    pub fn is_empty(&self) -> bool {
        self.segments == 0
    }

    /// How many segments it carries, for the note that says so.
    pub fn count(&self) -> usize {
        self.segments
    }

    /// Write it. `beside` is the cut itself; the file takes its name, which
    /// is what makes a tool that opens the cut find them together.
    ///
    /// Where a recording carries more than one of these they cannot all be
    /// that name, so each takes the language the disc says it is in --
    /// `cut_title.eng.sup` -- or the number it sits on where the disc says
    /// nothing or says the same thing twice.
    pub fn write(&self, beside: &str) -> Result<String> {
        let stem = std::path::Path::new(beside).with_extension("");
        let at = match &self.tag {
            None => stem.with_extension("sup"),
            Some(tag) => stem.with_extension(format!("{tag}.sup")),
        };
        std::fs::File::create(&at)?.write_all(&self.out)?;
        Ok(at.to_string_lossy().into_owned())
    }
}

/// Reading a display set back into the picture it draws.
///
/// The other direction from [`write`], and for the other destination. A
/// Blu-ray's subtitles can travel inside a cut untouched, which is what
/// [`Plane`] is for, and they are not always wanted there: a `.idx` and
/// `.sub` pair beside the cut is what an editor, a subtitle tool, or a player
/// that has never heard of a display set reads without being taught anything.
/// So the set is decoded here and written back out as a DVD's kind of unit --
/// see [`crate::vobsub::unit`], which is the other half of this.
///
/// libavcodec's own decoder does the reading, and unlike a DVD's it needs
/// nothing told to it: a display set carries its own colours. What it does
/// need is **every set, in order**. A set that is not self-contained changes
/// a composition whose picture and palette arrived earlier, so a decoder that
/// was not shown those has nothing to change; the same decoder is kept for
/// the whole of a stream for that reason.
pub mod read {
    use anyhow::Result;
    use ffmpeg_next as ff;

    /// One graphics stream being read back into pictures.
    pub struct Reader {
        decoder: ff::codec::decoder::Subtitle,
    }

    impl Reader {
        /// Open one for a recording of this screen size.
        pub fn open(screen: (u16, u16)) -> Result<Reader> {
            let mut ctx = ff::codec::context::Context::new();
            unsafe {
                let p = ctx.as_mut_ptr();
                (*p).codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
                (*p).codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE;
                (*p).width = screen.0 as i32;
                (*p).height = screen.1 as i32;
            }
            Ok(Reader {
                decoder: ctx.decoder().subtitle()?,
            })
        }

        /// The picture one display set puts up, or `None` where it puts up
        /// none -- which is what the set that clears the plane amounts to.
        ///
        /// `set` is the segments of one whole set, end to end, which is how
        /// the decoder wants them: it reads a run of segments and answers at
        /// the END that finishes them.
        pub fn read(&mut self, set: &[u8]) -> Result<Option<crate::vobsub::Drawn>> {
            let packet = ff::Packet::copy(set);
            let mut sub = ff::codec::subtitle::Subtitle::new();
            if !self.decoder.decode(&packet, &mut sub)? {
                return Ok(None);
            }
            Ok(crate::vobsub::drawn_from(&sub))
        }
    }
}

/// Writing display sets, rather than reading them.
///
/// A cut of a Blu-ray never needs this: the sets it carries were written by
/// the disc. A cut of a **DVD** does, if its subtitles are to travel inside
/// the file at all -- a DVD's own subtitle format has no stream type a
/// transport stream can carry, so the picture is taken out of one and put
/// into the other. See [`crate::vobsub`] for the other answer to the same
/// problem, which is not to convert at all.
///
/// What is converted is a picture and a palette, and both formats hold
/// exactly that: an index per pixel, run-length coded, and a table of
/// colours. Neither is resampled and neither loses a pixel. What changes is
/// how the runs are spelled and how the colours are written down -- a DVD
/// says red, green, blue and how opaque; a Blu-ray says luma, two colour
/// differences and how opaque.
pub mod write {
    use super::{Held, END, EPOCH_START, PCS, WDS};

    /// Where on the screen a subtitle is drawn: from the left, from the
    /// top, and how wide and how tall.
    pub type Window = (u16, u16, u16, u16);

    /// A palette entry: what the disc drew with.
    pub const PDS: u8 = 0x14;
    /// The picture itself.
    pub const ODS: u8 = 0x15;

    /// How far in front of the moment it is shown a display set is handed
    /// over, so a decoder has it ready.
    ///
    /// A disc leaves itself between two and seventy milliseconds, depending
    /// on how big the picture is. This is the same order and one number: it
    /// costs nothing to be early, and the first subtitle of a cut has its
    /// lead clipped at the start of the file rather than being late.
    const LEAD: f64 = 0.06;

    /// One picture to put on the screen.
    pub struct Picture<'a> {
        /// Where it goes, from the top left of the screen.
        pub x: u16,
        pub y: u16,
        pub width: u16,
        pub height: u16,
        /// One byte per pixel, rows first, `width` of them to a row.
        pub indices: &'a [u8],
        /// What each of those bytes means: red, green, blue, and how opaque.
        pub palette: &'a [(u8, u8, u8, u8)],
    }

    /// The display set that puts `picture` up at `at`, on a screen of
    /// `screen`.
    ///
    /// Five segments, which is what a disc sends: the composition, the
    /// window it draws inside, the palette, the picture, and the end. The
    /// composition opens an epoch, because a converted subtitle stands on
    /// nothing that came before it.
    pub fn draw(screen: (u16, u16), number: u16, at: f64, picture: &Picture) -> Vec<Held> {
        let dts = (at - LEAD).max(0.0);
        let window = (picture.x, picture.y, picture.width, picture.height);
        let mut out = vec![
            held(
                dts,
                at,
                PCS,
                &composition(screen, number, EPOCH_START, Some(picture)),
            ),
            held(dts, dts, WDS, &windows(window)),
            held(dts, dts, PDS, &palette(picture.palette)),
        ];
        for segment in objects(picture) {
            out.push(held(dts, dts, ODS, &segment));
        }
        out.push(held(dts, dts, END, &[]));
        out
    }

    /// The display set that takes it down again.
    ///
    /// The same shape a disc's own clear has: a composition with no objects
    /// in it, and the window it is clearing.
    pub fn erase(screen: (u16, u16), number: u16, at: f64, window: Window) -> Vec<Held> {
        vec![
            held(at, at, PCS, &composition(screen, number, 0x00, None)),
            held(at, at, WDS, &windows(window)),
            held(at, at, END, &[]),
        ]
    }

    fn held(dts: f64, pts: f64, kind: u8, body: &[u8]) -> Held {
        let mut data = Vec::with_capacity(3 + body.len());
        data.push(kind);
        data.extend_from_slice(&(body.len() as u16).to_be_bytes());
        data.extend_from_slice(body);
        Held { pts, dts, data }
    }

    /// The eleven bytes every composition opens with, and the object it puts
    /// up where there is one.
    fn composition(
        screen: (u16, u16),
        number: u16,
        state: u8,
        picture: Option<&Picture>,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(19);
        out.extend_from_slice(&screen.0.to_be_bytes());
        out.extend_from_slice(&screen.1.to_be_bytes());
        // The rate the pictures under it arrive at, which a composition
        // states and nothing here varies: a subtitle is shown for as long as
        // its own timing says, whatever the pictures are doing.
        out.push(0x10);
        out.extend_from_slice(&number.to_be_bytes());
        out.push(state);
        out.push(0x00); // no palette update
        out.push(0x00); // palette zero, which is the only one written
        match picture {
            None => out.push(0),
            Some(p) => {
                out.push(1);
                out.extend_from_slice(&0u16.to_be_bytes()); // object zero, likewise
                out.push(0x00); // in window zero
                out.push(0x00); // and not cropped
                out.extend_from_slice(&p.x.to_be_bytes());
                out.extend_from_slice(&p.y.to_be_bytes());
            }
        }
        out
    }

    /// One window, which is all a subtitle needs.
    fn windows(window: Window) -> Vec<u8> {
        let mut out = Vec::with_capacity(10);
        out.push(1);
        out.push(0);
        for v in [window.0, window.1, window.2, window.3] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// The colours, as luma, two colour differences and an opacity.
    ///
    /// The conversion is the one television has used since colour: the same
    /// coefficients a DVD's own palette is written in, run the other way.
    /// Entries that are fully transparent are written anyway -- a decoder
    /// reads the table by index, and a gap in it is a colour that means
    /// nothing.
    fn palette(colours: &[(u8, u8, u8, u8)]) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + colours.len() * 5);
        out.push(0x00); // palette zero
        out.push(0x00); // version zero
        for (i, &(r, g, b, a)) in colours.iter().enumerate().take(255) {
            let (r, g, b) = (r as f64, g as f64, b as f64);
            let y = 16.0 + 0.257 * r + 0.504 * g + 0.098 * b;
            let cb = 128.0 - 0.148 * r - 0.291 * g + 0.439 * b;
            let cr = 128.0 + 0.439 * r - 0.368 * g - 0.071 * b;
            let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
            out.push(i as u8);
            out.push(byte(y));
            out.push(byte(cr));
            out.push(byte(cb));
            out.push(a);
        }
        out
    }

    /// The picture, run-length coded, in as many segments as it takes.
    ///
    /// One is the ordinary answer: a subtitle compresses to a few kilobytes
    /// and a segment holds sixty. The split is here because the format's
    /// length field is sixteen bits and a full-screen picture of noise would
    /// not fit -- and because a reader that meets a split it does not expect
    /// shows nothing at all.
    fn objects(picture: &Picture) -> Vec<Vec<u8>> {
        let data = rle(
            picture.indices,
            picture.width as usize,
            picture.height as usize,
        );
        // Everything but the run: the object, its version, the flags, the
        // length, and the shape. A continuation carries only the first four.
        const HEAD: usize = 2 + 1 + 1 + 3;
        const ROOM: usize = 0xFFE0;
        let mut out = Vec::new();
        let mut rest = &data[..];
        let mut first = true;
        while first || !rest.is_empty() {
            let room = ROOM - HEAD - if first { 4 } else { 0 };
            let take = rest.len().min(room);
            let last = take == rest.len();
            let mut seg = Vec::with_capacity(HEAD + 4 + take);
            seg.extend_from_slice(&0u16.to_be_bytes()); // object zero
            seg.push(0x00); // version zero
            seg.push(match (first, last) {
                (true, true) => 0xC0,
                (true, false) => 0x80,
                (false, true) => 0x40,
                (false, false) => 0x00,
            });
            if first {
                // The length the *whole* object comes to, which only the
                // first segment states: the shape and the run together.
                let whole = data.len() + 4;
                seg.extend_from_slice(&(whole as u32).to_be_bytes()[1..]);
                seg.extend_from_slice(&picture.width.to_be_bytes());
                seg.extend_from_slice(&picture.height.to_be_bytes());
            }
            seg.extend_from_slice(&rest[..take]);
            out.push(seg);
            rest = &rest[take..];
            first = false;
        }
        out
    }

    /// Run-length code one picture, a row at a time.
    ///
    /// Four ways to write a run, and the shortest that fits is the one used:
    /// a single pixel of a colour is that colour's own byte, and everything
    /// else is an escape, a length, and -- unless the colour is the
    /// transparent zero -- the colour. Every row ends with the escape twice.
    fn rle(indices: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for row in 0..height {
            let Some(line) = indices.get(row * width..row * width + width) else {
                break;
            };
            let mut at = 0;
            while at < width {
                let colour = line[at];
                let mut run = 1;
                while at + run < width && line[at + run] == colour {
                    run += 1;
                }
                write_run(&mut out, colour, run);
                at += run;
            }
            out.push(0x00);
            out.push(0x00);
        }
        out
    }

    fn write_run(out: &mut Vec<u8>, colour: u8, mut run: usize) {
        while run > 0 {
            let take = run.min(0x3FFF);
            match (colour, take) {
                // Two of a colour cost two bytes either way; written plainly
                // they cost nothing to read.
                (c, n) if c != 0 && n <= 2 => out.extend(std::iter::repeat_n(c, n)),
                (0, n) if n <= 0x3F => out.extend_from_slice(&[0x00, n as u8]),
                (0, n) => out.extend_from_slice(&[0x00, 0x40 | (n >> 8) as u8, n as u8]),
                (c, n) if n <= 0x3F => out.extend_from_slice(&[0x00, 0x80 | n as u8, c]),
                (c, n) => out.extend_from_slice(&[0x00, 0xC0 | (n >> 8) as u8, n as u8, c]),
            }
            run -= take;
        }
    }

    /// Read a run-length coded picture back. **For the tests**, which is
    /// where the only reader of one of these lives: nothing in the program
    /// decodes a subtitle it has just written.
    #[cfg(test)]
    pub fn unrle(data: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut out = vec![0u8; width * height];
        let (mut at, mut x, mut y) = (0usize, 0usize, 0usize);
        while at < data.len() && y < height {
            let (colour, run, step) = if data[at] != 0 {
                (data[at], 1usize, 1usize)
            } else {
                let b = *data.get(at + 1).unwrap_or(&0);
                match (b & 0xC0, b) {
                    (_, 0) => {
                        y += 1;
                        x = 0;
                        at += 2;
                        continue;
                    }
                    (0x00, _) => (0, (b & 0x3F) as usize, 2),
                    (0x40, _) => (0, (((b & 0x3F) as usize) << 8) | data[at + 2] as usize, 3),
                    (0x80, _) => (data[at + 2], (b & 0x3F) as usize, 3),
                    _ => (
                        data[at + 3],
                        (((b & 0x3F) as usize) << 8) | data[at + 2] as usize,
                        4,
                    ),
                }
            };
            for _ in 0..run {
                if x < width {
                    out[y * width + x] = colour;
                }
                x += 1;
            }
            at += step;
        }
        out
    }

    /// The state a converted stream is in, so that one subtitle replaces the
    /// last and the last is taken down when a range ends.
    ///
    /// A DVD's units say when to appear and, sometimes, when to go; a
    /// Blu-ray's sets have to be told. This is what remembers the
    /// difference.
    #[derive(Debug, Default)]
    pub struct Composer {
        /// The next composition number. A disc counts these up across the
        /// whole recording and so does this.
        number: u16,
        /// The window the standing picture is drawn in, and when it should
        /// come down -- `None` where nothing said, which leaves it standing
        /// until the next one or the end of the range.
        standing: Option<(Window, Option<f64>)>,
    }

    impl Composer {
        /// Put a picture up at `at`, taking down whatever was there if its
        /// time has come. `until` is when this one should go.
        pub fn show(
            &mut self,
            screen: (u16, u16),
            at: f64,
            until: Option<f64>,
            picture: &Picture,
        ) -> Vec<Held> {
            let mut out = self.take_down_before(screen, at);
            self.number = self.number.wrapping_add(1);
            out.extend(draw(screen, self.number, at, picture));
            self.standing = Some(((picture.x, picture.y, picture.width, picture.height), until));
            out
        }

        /// Take down whatever is standing, at the moment it was due or at
        /// `at`, whichever comes first.
        pub fn take_down(&mut self, screen: (u16, u16), at: f64) -> Vec<Held> {
            let Some((window, until)) = self.standing.take() else {
                return Vec::new();
            };
            self.number = self.number.wrapping_add(1);
            let when = until.filter(|&u| u < at).unwrap_or(at);
            erase(screen, self.number, when, window)
        }

        /// Whether anything is on screen.
        pub fn standing(&self) -> bool {
            self.standing.is_some()
        }

        fn take_down_before(&mut self, screen: (u16, u16), at: f64) -> Vec<Held> {
            match self.standing {
                // The one that is up says when it goes, and it goes before
                // this one arrives: an empty screen in between, as the disc
                // had it.
                Some((_, Some(until))) if until <= at => self.take_down(screen, until),
                // Otherwise the new composition replaces it, which is what a
                // fresh epoch means.
                _ => {
                    self.standing = None;
                    Vec::new()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcs(number: u16, state: u8, objects: u8) -> Vec<u8> {
        // The eleven bytes every composition opens with: the screen, the
        // rate it is shown at, the number, the state, the palette, and how
        // many objects go up.
        let mut body = vec![0u8; 11];
        body[0..2].copy_from_slice(&1920u16.to_be_bytes());
        body[2..4].copy_from_slice(&1080u16.to_be_bytes());
        body[4] = 0x10;
        body[5..7].copy_from_slice(&number.to_be_bytes());
        body[7] = state;
        body[10] = objects;
        packed(PCS, &body)
    }

    fn one(kind: u8, len: usize) -> Vec<u8> {
        packed(kind, &vec![0u8; len])
    }

    fn held(at: f64, data: Vec<u8>) -> Held {
        Held {
            pts: at,
            dts: at,
            data,
        }
    }

    /// What a `.sup` is: every segment as it travelled, behind ten bytes
    /// saying when it is decoded and when it is shown.
    ///
    /// Read back here the way the readers that matter read it -- the magic,
    /// the two times, the type and the length -- because nothing else in
    /// this program ever reads one.
    #[test]
    fn a_sup_is_the_segments_with_their_times_in_front() {
        let mut sup = Sup::new(None);
        let mut set = pcs(7, EPOCH_START, 1);
        set.extend(one(WDS, 10));
        // One packet carrying three segments, which the format allows and a
        // demuxer hands over as it was given: three records, not one.
        set.extend(one(END, 0));
        sup.take(&held(2.0, set), 0.5);
        assert_eq!(sup.count(), 3);
        assert!(!sup.is_empty());

        let mut at = 0usize;
        let mut kinds = Vec::new();
        while at < sup.out.len() {
            assert_eq!(&sup.out[at..at + 2], b"PG", "every record opens with it");
            let stamp = |from: usize| {
                u32::from_be_bytes(sup.out[from..from + 4].try_into().expect("four bytes"))
            };
            // 2.0 seconds and half a second of offset, in 90 kHz.
            assert_eq!(stamp(at + 2), 225_000);
            assert_eq!(stamp(at + 6), 225_000);
            let len = u16::from_be_bytes([sup.out[at + 11], sup.out[at + 12]]) as usize;
            kinds.push(sup.out[at + 10]);
            at += 13 + len;
        }
        assert_eq!(kinds, [PCS, WDS, END]);
        assert_eq!(at, sup.out.len(), "and nothing left over");
    }

    /// The only stream takes the cut's own name; where there is a second,
    /// each takes something of its own. What that is, is the caller's.
    #[test]
    fn a_sup_is_named_after_the_cut() {
        let at = std::env::temp_dir().join("smartcut-sup-test");
        std::fs::create_dir_all(&at).expect("a place to write");
        let cut = at.join("cut_title.ts");
        let cut = cut.to_string_lossy().into_owned();

        let mut only = Sup::new(None);
        only.take(&held(0.0, one(END, 0)), 0.0);
        let wrote = only.write(&cut).expect("writes");
        assert!(wrote.ends_with("cut_title.sup"), "{wrote}");

        let mut second = Sup::new(Some("eng".into()));
        second.take(&held(0.0, one(END, 0)), 0.0);
        let wrote = second.write(&cut).expect("writes");
        assert!(wrote.ends_with("cut_title.eng.sup"), "{wrote}");
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A packet that carries several segments at once is read as several.
    #[test]
    fn reads_a_run_of_segments() {
        let mut data = pcs(1, EPOCH_START, 1);
        data.extend(one(WDS, 10));
        data.extend(one(END, 0));
        let kinds: Vec<u8> = segments(&data).map(|(k, _)| k).collect();
        assert_eq!(kinds, [PCS, WDS, END]);
    }

    /// A length that runs past the end of the packet stops the read rather
    /// than reading whatever is next in memory.
    #[test]
    fn a_truncated_segment_ends_the_run() {
        let data = vec![PCS, 0xFF, 0xFF, 0x00];
        assert_eq!(segments(&data).count(), 0);
    }

    /// The plane follows what the sets say: up on an epoch start, still up
    /// through an acquisition point, down on a composition with no objects.
    #[test]
    fn follows_what_is_on_screen() {
        let mut plane = Plane::default();
        for (at, data) in [(1.0, pcs(1, EPOCH_START, 1)), (1.1, one(END, 0))] {
            plane.feed(held(at, data));
        }
        assert!(plane.occupied());
        plane.feed(held(2.0, pcs(2, 0x00, 0)));
        plane.feed(held(2.1, one(END, 0)));
        assert!(!plane.occupied());
    }

    /// A set is not a set until its END. Half of one leaves the plane as it
    /// was, which is what a read that begins mid-set hands over.
    #[test]
    fn half_a_set_changes_nothing() {
        let mut plane = Plane::default();
        plane.feed(held(1.0, pcs(1, EPOCH_START, 1)));
        assert!(!plane.occupied());
    }

    /// What is put up again is the last set that could stand alone, timed to
    /// the instant asked for and keeping its own spacing.
    #[test]
    fn puts_the_standing_set_up_again() {
        let mut plane = Plane::default();
        plane.feed(Held {
            pts: 10.07,
            dts: 10.0,
            data: pcs(1, ACQUISITION, 1),
        });
        plane.feed(held(10.02, one(END, 0)));
        let again = plane.replay(100.0);
        assert_eq!(again.len(), 2);
        assert!((again[0].dts - 100.0).abs() < 1e-9);
        assert!((again[0].pts - 100.07).abs() < 1e-9);
        assert!((again[1].pts - 100.02).abs() < 1e-9);
        // And it says it opens an epoch, whatever it said before.
        let (kind, body) = segments(&again[0].data).next().unwrap();
        assert_eq!(kind, PCS);
        assert_eq!(composition(body).unwrap().state, EPOCH_START);
    }

    /// A composition that changes a subtitle drawn earlier cannot stand on
    /// its own, so it is not what gets put up again.
    #[test]
    fn only_a_self_contained_set_is_put_up_again() {
        let mut plane = Plane::default();
        plane.feed(held(1.0, pcs(1, EPOCH_START, 1)));
        plane.feed(held(1.1, one(END, 0)));
        plane.feed(held(2.0, pcs(2, 0x00, 1)));
        plane.feed(held(2.1, one(END, 0)));
        assert!(plane.occupied());
        let again = plane.replay(50.0);
        let (_, body) = segments(&again[0].data).next().unwrap();
        assert_eq!(composition(body).unwrap().number, 1);
    }

    /// The clear is the standing composition with its objects taken out, and
    /// the window it drew in.
    #[test]
    fn takes_down_what_is_on_screen() {
        let mut plane = Plane::default();
        plane.feed(held(1.0, pcs(7, EPOCH_START, 1)));
        plane.feed(held(1.1, one(WDS, 10)));
        plane.feed(held(1.2, one(END, 0)));
        let down = plane.clear(9.0);
        let kinds: Vec<u8> = down
            .iter()
            .filter_map(|h| segments(&h.data).next())
            .map(|(k, _)| k)
            .collect();
        assert_eq!(kinds, [PCS, WDS, END]);
        let (_, body) = segments(&down[0].data).next().unwrap();
        let c = composition(body).unwrap();
        assert_eq!(c.objects, 0);
        assert_eq!(c.state, 0x00);
        assert_eq!(c.number, 8);
        // The screen the disc declared travels with it.
        assert_eq!(&body[..5], &[0x07, 0x80, 0x04, 0x38, 0x10]);
        assert!(down.iter().all(|h| (h.pts - 9.0).abs() < 1e-9));
    }

    /// A range that ends with the plane already clear needs nothing.
    #[test]
    fn an_empty_plane_is_not_taken_down() {
        let mut plane = Plane::default();
        plane.feed(held(1.0, pcs(1, EPOCH_START, 0)));
        plane.feed(held(1.1, one(END, 0)));
        assert!(plane.clear(9.0).is_empty());
    }

    /// A picture written as a display set comes back as the same pixels.
    ///
    /// The whole of what the conversion has to get right: the runs, the
    /// row ends, and the shape stated in front of them.
    #[test]
    fn a_picture_survives_being_written() {
        // Two rows of a long transparent run, a word of colour, and a single
        // pixel -- which is each of the four ways a run can be spelled.
        let mut indices = vec![0u8; 200 * 4];
        indices[40..100].fill(2);
        indices[150] = 3;
        indices[200..400].fill(1);
        indices[400 + 7] = 4;
        let palette = [
            (0, 0, 0, 0),
            (255, 255, 255, 255),
            (255, 0, 0, 255),
            (0, 255, 0, 128),
            (0, 0, 255, 255),
        ];
        let picture = write::Picture {
            x: 10,
            y: 20,
            width: 200,
            height: 4,
            indices: &indices,
            palette: &palette,
        };
        let set = write::draw((720, 480), 7, 5.0, &picture);
        let kinds: Vec<u8> = set
            .iter()
            .filter_map(|h| segments(&h.data).next())
            .map(|(k, _)| k)
            .collect();
        assert_eq!(kinds, [PCS, WDS, write::PDS, write::ODS, END]);

        let (_, body) = segments(&set[0].data).next().unwrap();
        let c = composition(body).unwrap();
        assert_eq!((c.number, c.state, c.objects), (7, EPOCH_START, 1));
        // Where it goes on the screen, which is the last four bytes of the
        // composition's one object.
        assert_eq!(&body[15..19], [0x00, 0x0A, 0x00, 0x14]);

        let (_, ods) = segments(&set[3].data).next().unwrap();
        assert_eq!(&ods[7..11], [0x00, 0xC8, 0x00, 0x04], "the shape it states");
        let back = write::unrle(&ods[11..], 200, 4);
        assert_eq!(back, indices, "the pixels have to survive the round trip");
    }

    /// The picture is timed to be decoded before it is shown, and the
    /// composition that puts it up is the only piece shown at all.
    #[test]
    fn a_set_is_handed_over_before_it_is_shown() {
        let indices = [0u8; 4];
        let picture = write::Picture {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            indices: &indices,
            palette: &[(0, 0, 0, 0)],
        };
        let set = write::draw((720, 480), 1, 5.0, &picture);
        assert!((set[0].pts - 5.0).abs() < 1e-9);
        assert!(set[0].dts < set[0].pts, "decoded first, shown after");
        assert!(set.iter().skip(1).all(|h| h.pts <= set[0].dts + 1e-9));
        // And at the very start of a file there is nowhere earlier to be.
        let set = write::draw((720, 480), 1, 0.0, &picture);
        assert!(set.iter().all(|h| h.dts >= 0.0 && h.pts >= 0.0));
    }

    /// One subtitle replaces the last where the last had no end of its own,
    /// and the last is taken down at its own time where it had one.
    #[test]
    fn a_composer_keeps_one_thing_on_screen() {
        let indices = [1u8; 4];
        let palette = [(0, 0, 0, 0), (255, 255, 255, 255)];
        let picture = |x: u16| write::Picture {
            x,
            y: 0,
            width: 2,
            height: 2,
            indices: &indices,
            palette: &palette,
        };
        let mut composer = write::Composer::default();
        // Nothing said about when the first goes, so the second replaces it
        // and nothing is written in between.
        let first = composer.show((720, 480), 1.0, None, &picture(0));
        assert_eq!(first.len(), 5);
        let second = composer.show((720, 480), 2.0, Some(2.5), &picture(4));
        assert_eq!(second.len(), 5, "no clear between two that abut");
        // This one does say, so it is taken down at its own time.
        let third = composer.show((720, 480), 3.0, None, &picture(8));
        assert_eq!(third.len(), 8, "a clear, and then the next picture");
        assert!((third[0].pts - 2.5).abs() < 1e-9);
        // And what is left standing comes down when the range ends.
        assert!(composer.standing());
        let last = composer.take_down((720, 480), 9.0);
        assert_eq!(last.len(), 3);
        assert!((last[0].pts - 9.0).abs() < 1e-9);
        assert!(!composer.standing());
        assert!(composer.take_down((720, 480), 10.0).is_empty());
    }

    /// A part-read set says so, which is how a reader knows not to stop in
    /// the middle of one.
    #[test]
    fn a_part_read_set_says_so() {
        let mut plane = Plane::default();
        assert!(!plane.building());
        plane.feed(held(1.0, pcs(1, EPOCH_START, 1)));
        assert!(plane.building());
        plane.feed(held(1.2, one(END, 0)));
        assert!(!plane.building());
    }

    /// A set comes back whole at its END and not before, which is what lets
    /// the caller carry sets rather than packets.
    #[test]
    fn a_set_comes_back_at_its_end() {
        let mut plane = Plane::default();
        assert!(plane.feed(held(1.0, pcs(1, EPOCH_START, 1))).is_none());
        assert!(plane.feed(held(1.1, one(WDS, 10))).is_none());
        let set = plane
            .feed(held(1.2, one(END, 0)))
            .expect("the END completes it");
        assert_eq!(set.len(), 3);
        assert!((set[0].pts - 1.0).abs() < 1e-9);
    }

    /// A read that joins in the middle of a set contributes nothing: the
    /// pieces before the next composition are not a set and never become
    /// one.
    #[test]
    fn a_read_that_joins_mid_set_hands_over_nothing() {
        let mut plane = Plane::default();
        assert!(plane.feed(held(1.0, one(WDS, 10))).is_none());
        assert!(plane.feed(held(1.1, one(END, 0))).is_none());
        assert!(!plane.occupied());
    }
}
