//! Make a clip whose clock restarts read as one recording.
//!
//! **A recorder's clip is not always one run of time.** It stops and starts
//! -- the viewer paused it, or it was told to leave the commercials out --
//! and every time it does, the stream inside the file begins a fresh
//! sequence with a clock of its own. The bytes carry straight on; the times
//! do not. On the discs this was measured against, a forty-five minute
//! programme is five, six or seven such sequences in a single `.m2ts`, and
//! the playlist beside it plays them one after another as that many items.
//!
//! Handed the file whole, libavformat cannot even say how long it is: the
//! last timestamp in the file is 1.1 seconds and the first is 8.3, so the
//! duration comes back unknown and every seek lands somewhere else. That is
//! why such a clip used to be offered a sequence at a time, one row of the
//! list each -- see [`crate::disc`], and the `@` names in [`crate::input`].
//!
//! This is the other answer, and the one a player gives: put the sequences
//! back on one clock while the bytes are being read.
//!
//! **Nothing moves.** A presentation time lives in a fixed five bytes of a
//! PES header and a program clock reference in a fixed six of an adaptation
//! field, so adding to them changes no length and no offset. The clip's own
//! entry-point map still points where it pointed, the seek index still
//! addresses the same bytes, and the demuxer reads a stream whose clock runs
//! from one end to the other.
//!
//! **And nothing is remembered.** Which sequence a byte belongs to is a
//! property of where it is in the file, so the correction for any packet can
//! be worked out from its position alone. That is what makes the thing
//! seekable: libavformat may jump anywhere it likes, and the packet it lands
//! on is corrected by the same amount it would have been corrected by if the
//! file had been read from the beginning.

/// A source packet: the transport packet and the four bytes in front of it
/// that say when it arrived.
pub const SOURCE_PACKET: usize = 192;

/// The clock a transport stream states its times on, in ticks per second.
const TICK: f64 = 90_000.0;

/// The clock wraps here. Thirty-three bits is about 26.5 hours, which no
/// recording reaches and every arithmetic on one has to respect anyway:
/// a correction that pushes a time past the end starts again at the
/// beginning, exactly as the stream itself would.
const WRAP: i64 = 1 << 33;

/// What a table found by reading the stream is written down as beginning
/// with, ahead of its packet size. See [`Restamp::encode`].
const CAT: &str = "cat";

/// One stretch of a clip that keeps a single clock, and what to add to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Where the stretch begins, in bytes from the start of what is opened.
    /// It runs to the next piece, and the last one runs to the end.
    pub at: u64,
    /// What to add to every time inside it, in 90 kHz ticks. Negative where
    /// a sequence's own clock starts later than the place it has been given
    /// on the joined one.
    pub shift: i64,
    /// Where it begins on the joined clock, in 90 kHz ticks. The first piece
    /// begins where the clip's own clock begins; each one after it begins
    /// where the one before it ended.
    ///
    /// Carried because a join is a seam and a cut has to know: the pictures
    /// on either side belong to two recordings the recorder made minutes
    /// apart, and copying across one hands the decoder pictures whose
    /// references are not there. See [`crate::plan::plan_on`].
    pub from: i64,
}

/// Where one stretch of a joined clip gives way to the next.
///
/// **Both halves are needed and they answer different questions.** The time
/// is what the planner cuts at, because a range is a range of times. The byte
/// is what says which stretch a packet belongs to, and only it can: where a
/// stretch begins is read off an index that understates it at both ends, so
/// two stretches can overlap by a fraction of a second on the joined clock
/// and a time then picks out both of them. The byte is the boundary the
/// recorder actually wrote.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seam {
    /// The first byte of the stretch after the seam, counted from the front
    /// of the clip.
    pub at: u64,
    /// When that stretch begins, in seconds on the joined clock.
    pub time: f64,
    /// When the stretch *before* it stops, on the same clock: where its last
    /// picture stops being shown, where that is in front of `time`.
    ///
    /// The same instant as `time` on every clip whose sequence table can be
    /// believed, which is why it is written down separately only here: a
    /// recorder that gives a sequence a span longer than the pictures in it
    /// leaves a range planned to `time` asking for pictures the stretch does
    /// not have. See [`crate::index::mend_stretches`], which is the one place
    /// that moves this, and only ever earlier.
    pub ends: f64,
}

/// The correction to apply to a clip, piece by piece.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restamp {
    /// In order, first at zero. A table with fewer than two pieces corrects
    /// nothing and is not built: see [`Restamp::wanted`].
    pieces: Vec<Piece>,
    /// How long the packets are, where it is not a disc's 192: a transport
    /// stream of recordings joined end to end ([`cat_of`]) is 188 bytes to the
    /// packet as often as not. Zero is 192, the disc's size, which is what
    /// every table built from a clip index is.
    packet: usize,
    /// Whether this table was found by reading the stream ([`cat_of`]) rather
    /// than off a disc's own index. Such a recording reads on another clock
    /// than it did before 0.9.2, so what is kept for it is kept apart; see
    /// [`crate::input::Input::cache_salt`].
    found: bool,
}

impl Restamp {
    /// Build one from pieces already in order.
    pub fn new(pieces: Vec<Piece>) -> Restamp {
        Restamp {
            pieces,
            packet: 0,
            found: false,
        }
    }

    /// The packet size the corrections are applied in.
    pub fn packet(&self) -> usize {
        if self.packet == 0 {
            SOURCE_PACKET
        } else {
            self.packet
        }
    }

    /// Whether the table was found by reading the stream. See `found`.
    pub fn found(&self) -> bool {
        self.found
    }

    /// Whether there is anything to do. One piece is a clip that already
    /// reads straight through and is read as it always was.
    ///
    /// Several pieces is a table even where every correction in it is zero,
    /// which is a clip whose stretches happen to run on from one another to
    /// the tick. The seams are still seams -- a copy cannot be carried across
    /// one, see [`crate::plan::plan_on`] -- and a correction of nothing costs
    /// nothing: [`Restamp::apply`] leaves such a packet alone.
    pub fn wanted(&self) -> bool {
        self.pieces.len() > 1
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// The seams: where each stretch after the first begins, in bytes into
    /// the clip and in seconds on the joined clock. Empty where there is only
    /// one stretch.
    pub fn seams(&self) -> Vec<Seam> {
        self.pieces
            .iter()
            .skip(1)
            .map(|p| Seam {
                at: p.at,
                time: p.from as f64 / TICK,
                ends: p.from as f64 / TICK,
            })
            .collect()
    }

    /// What to add to a time read at `at` bytes into the clip.
    pub fn shift_at(&self, at: u64) -> i64 {
        match self.pieces.binary_search_by_key(&at, |p| p.at) {
            Ok(i) => self.pieces[i].shift,
            // Before the first piece there is nothing to go on, so the first
            // piece's own correction is used: a clip is named from its start
            // and there are no bytes before it to read.
            Err(0) => self.pieces.first().map_or(0, |p| p.shift),
            Err(i) => self.pieces[i - 1].shift,
        }
    }

    /// The same, in seconds, for the times this program works in.
    pub fn seconds_at(&self, at: u64) -> f64 {
        self.shift_at(at) as f64 / TICK
    }

    /// Correct every timestamp in `buf`, which holds whole source packets
    /// beginning at `at` bytes into the clip.
    ///
    /// A partial packet at the end is left alone: the caller reads in whole
    /// packets, and a tail shorter than one is the end of the file.
    pub fn apply(&self, at: u64, buf: &mut [u8]) {
        let size = self.packet();
        for (i, packet) in buf.chunks_exact_mut(size).enumerate() {
            let shift = self.shift_at(at + (i * size) as u64);
            if shift != 0 {
                // The four bytes in front are the arrival time, which is on
                // the recorder's own clock rather than the stream's and is
                // read by nothing here. Left as the recorder wrote it.
                stamp(&mut packet[size - 188..], shift);
            }
        }
    }

    /// Encode as text, for the name a demuxer is opened by.
    ///
    /// The table has to travel with the recording, and everything in this
    /// program travels as one string -- so it is written into the URL rather
    /// than read again at the other end. See [`crate::input::demux`].
    pub fn encode(&self) -> String {
        let pieces = self
            .pieces
            .iter()
            .map(|p| format!("{}:{}:{}", p.at, p.shift, p.from))
            .collect::<Vec<_>>()
            .join(",");
        // A disc's table is written exactly as it always was.
        if self.found {
            format!("{CAT}{};{pieces}", self.packet())
        } else {
            pieces
        }
    }

    /// Read back what [`Restamp::encode`] wrote. `None` for anything that is
    /// not a table, which is a URL this module did not write.
    pub fn decode(text: &str) -> Option<Restamp> {
        let (packet, found, text) = match text.strip_prefix(CAT) {
            Some(rest) => {
                let (size, rest) = rest.split_once(';')?;
                let size: usize = size.parse().ok()?;
                if size != 188 && size != SOURCE_PACKET {
                    return None;
                }
                (size, true, rest)
            }
            None => (0, false, text),
        };
        let mut pieces = Vec::new();
        for one in text.split(',') {
            let (at, rest) = one.split_once(':')?;
            let (shift, from) = rest.split_once(':')?;
            pieces.push(Piece {
                at: at.parse().ok()?,
                shift: shift.parse().ok()?,
                from: from.parse().ok()?,
            });
        }
        (!pieces.is_empty() && pieces.windows(2).all(|w| w[0].at < w[1].at)).then_some(Restamp {
            pieces,
            packet,
            found,
        })
    }
}

/// Correct the times in one 188-byte transport packet.
///
/// Two places carry one: the adaptation field's program clock reference,
/// which is what a decoder sets its own clock by, and the PES header's
/// presentation and decode times, which say when each picture and each
/// sound frame is wanted. Both are fixed-width fields at fixed offsets, so
/// both are read, added to, and written back where they were.
fn stamp(ts: &mut [u8], shift: i64) {
    if ts.len() < 188 || ts[0] != 0x47 {
        return; // not a transport packet; a clip is padded with these
    }
    let control = (ts[3] >> 4) & 0x3;
    let mut payload = 4;
    if control & 0x2 != 0 {
        let len = ts[4] as usize;
        payload = 5 + len;
        // A program clock reference is 33 bits of the 90 kHz clock and 9 of
        // the 27 MHz one under it. Only the top half moves.
        if len >= 7 && ts[5] & 0x10 != 0 && ts.len() >= 12 {
            let base = ((ts[6] as i64) << 25)
                | ((ts[7] as i64) << 17)
                | ((ts[8] as i64) << 9)
                | ((ts[9] as i64) << 1)
                | ((ts[10] as i64) >> 7);
            let moved = wrap(base.wrapping_add(shift));
            ts[6] = (moved >> 25) as u8;
            ts[7] = (moved >> 17) as u8;
            ts[8] = (moved >> 9) as u8;
            ts[9] = (moved >> 1) as u8;
            ts[10] = ((moved as u8) << 7) | (ts[10] & 0x7F);
        }
    }
    if control & 0x1 == 0 || payload >= ts.len() {
        return; // adaptation field only, or one that ran off the end
    }
    // A PES header only ever begins where a payload begins, and only in a
    // packet that says a unit starts in it.
    if ts[1] & 0x40 == 0 {
        return;
    }
    let pes = &mut ts[payload..];
    if pes.len() < 14 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
        return;
    }
    if !timed(pes[3]) || pes[6] & 0xC0 != 0x80 {
        return;
    }
    let flags = pes[7] >> 6;
    // Ten bits of PES header follow the flags, and the times are the first
    // thing in them: five bytes of presentation time, and five more of
    // decode time where the two differ.
    let header = pes[8] as usize;
    if flags & 0x2 != 0 && header >= 5 && pes.len() >= 14 {
        shift_time(&mut pes[9..14], shift);
    }
    if flags == 0x3 && header >= 10 && pes.len() >= 19 {
        shift_time(&mut pes[14..19], shift);
    }
}

/// Whether a PES packet with this stream id carries a header with times in
/// it. The seven that do not are tables and padding, and their fourth byte
/// is a length and then payload.
fn timed(stream_id: u8) -> bool {
    !matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    )
}

/// Add to one of the five-byte times a PES header states.
///
/// The value is 33 bits broken across the five with a marker bit at the end
/// of each of the last three and a four-bit tag at the front. Only the value
/// changes: the tag says whether this is a presentation time, a decode time,
/// or a presentation time with a decode time behind it, and that is still
/// true afterwards.
fn shift_time(at: &mut [u8], shift: i64) {
    let v = (((at[0] & 0x0E) as i64) << 29)
        | ((at[1] as i64) << 22)
        | (((at[2] & 0xFE) as i64) << 14)
        | ((at[3] as i64) << 7)
        | ((at[4] & 0xFE) as i64 >> 1);
    let moved = wrap(v.wrapping_add(shift));
    at[0] = (at[0] & 0xF0) | (((moved >> 30) as u8) << 1) | 0x01;
    at[1] = (moved >> 22) as u8;
    at[2] = (((moved >> 15) as u8) << 1) | 0x01;
    at[3] = (moved >> 7) as u8;
    at[4] = ((moved as u8) << 1) | 0x01;
}

/// A time that ran off either end of the clock comes back on at the other,
/// which is what the clock itself does.
///
/// Its callers add with `wrapping_add`: a shift comes out of a URL, and one
/// read as nearly `i64::MAX` would otherwise overflow before it got here.
/// Two to the sixty-four is a whole number of turns of this clock, so the
/// wrapped sum lands where the true one would.
fn wrap(v: i64) -> i64 {
    v.rem_euclid(WRAP)
}

// --- recordings joined end to end --------------------------------------

/// What reading a bare transport stream for a clock that goes back found.
///
/// **Two recordings in one transport stream.** Files joined end to end --
/// `cat a.ts b.ts`, the clips of a dashcam or a camera put together, the
/// parts of a capture saved one after another -- each bring a clock of their
/// own, usually from the same starting value, and read as one recording their
/// pictures were placed on top of each other: every cut, even of the whole
/// file, kept one of the parts and said the rest "could not be placed". A
/// recorder's clip is the same thing with an index that says so
/// ([`crate::disc::clip_restamp`]); here the stream itself is the only
/// witness, so it is read for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cat {
    /// One clock from end to end, or nothing this could read: the
    /// recording is opened as it always was. Every recording but the joined
    /// ones.
    One,
    /// Recordings one after another, and the table that puts them on one
    /// clock in the order the file holds them.
    Joined(Restamp),
    /// The clock goes back and the two sides cannot be told apart safely
    /// (too small a step, or packets of both clocks mixed together). Said
    /// rather than cut: a cut would quietly lose one side.
    Tangled(String),
}

/// How many places a large file's clock is looked at where its clock ends
/// short of its bytes; see [`cat_of`].
const CAT_SAMPLES: u64 = 64;

/// What is read at each of them, and the most that is read looking for a
/// clock reference there.
const CAT_WINDOW: usize = 128 << 10;
const CAT_WINDOW_MAX: usize = 1 << 20;

/// A file this size or smaller is read whole rather than looked at in places:
/// that costs no more than the places would.
const CAT_WHOLE: u64 = 16 << 20;

/// A clock reference this far behind the one before it is a clock that went
/// back. Within one recording the reference never goes back at all.
const CAT_BACK: i64 = 4_500;

/// The smallest step back the two sides of a join can be told apart across:
/// a picture is sent up to a second or two ahead of the moment it is shown,
/// so the first stamps of the recording after the join can sit that far
/// above its clock, and below this step they reach back up into the end of
/// the recording before it.
const CAT_APART: i64 = 4 * 90_000;

/// How many clock references in a row a new clock has to hold for before it
/// is believed. A single reference hit by reception damage is off by a power
/// of two and the next one is back where it was.
const CAT_HOLD: usize = 3;

/// How far a picture's stamp can sit from the clock it was sent by before it
/// is not believed as a picture's (reception damage) when the ends of a
/// stretch are measured.
const CAT_SANE: (i64, i64) = (-90_000, 5 * 90_000);

/// The distance from `from` to `to` on the 33-bit clock, the short way round.
fn apart(to: i64, from: i64) -> i64 {
    let d = (to - from).rem_euclid(WRAP);
    if d >= WRAP / 2 {
        d - WRAP
    } else {
        d
    }
}

/// The clock reference a transport packet carries, if any.
fn pcr_in(ts: &[u8]) -> Option<i64> {
    let control = (ts[3] >> 4) & 0x3;
    if control & 0x2 == 0 || ts[4] < 7 || ts[5] & 0x10 == 0 {
        return None;
    }
    Some(
        ((ts[6] as i64) << 25)
            | ((ts[7] as i64) << 17)
            | ((ts[8] as i64) << 9)
            | ((ts[9] as i64) << 1)
            | ((ts[10] as i64) >> 7),
    )
}

/// What a PES carries, as far as its stream id says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Picture,
    /// MPEG audio or AAC.
    Sound,
    /// Anything else with a time: AC-3 and captions alike (`0xBD`), VC-1.
    Other,
}

/// The presentation and decode times a PES header starting in this packet
/// carries (the decode time is the presentation time where it states none),
/// and what it carries.
fn stamp_in(ts: &[u8]) -> Option<(i64, i64, Kind)> {
    if ts[1] & 0x40 == 0 {
        return None;
    }
    let control = (ts[3] >> 4) & 0x3;
    if control & 0x1 == 0 {
        return None;
    }
    let payload = if control & 0x2 != 0 { 5 + ts[4] as usize } else { 4 };
    let pes = ts.get(payload..)?;
    if pes.len() < 14 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
        return None;
    }
    if !timed(pes[3]) || pes[6] & 0xC0 != 0x80 {
        return None;
    }
    let flags = pes[7] >> 6;
    let read = |at: &[u8]| {
        (((at[0] & 0x0E) as i64) << 29)
            | ((at[1] as i64) << 22)
            | (((at[2] & 0xFE) as i64) << 14)
            | ((at[3] as i64) << 7)
            | ((at[4] & 0xFE) as i64 >> 1)
    };
    if flags & 0x2 == 0 || pes[8] < 5 {
        return None;
    }
    let pts = read(&pes[9..14]);
    let dts = (flags == 0x3 && pes[8] >= 10 && pes.len() >= 19).then(|| read(&pes[14..19]));
    let kind = match pes[3] {
        0xE0..=0xEF => Kind::Picture,
        0xC0..=0xDF => Kind::Sound,
        _ => Kind::Other,
    };
    Some((pts, dts.unwrap_or(pts), kind))
}

/// The file being read, and how its packets are laid out.
struct Stream {
    file: std::fs::File,
    len: u64,
    /// 188, or 192 with the four arrival-time bytes in front.
    packet: u64,
}

impl Stream {
    /// Up to `n` bytes from `at`, fewer at the end of the file.
    fn read(&mut self, at: u64, n: usize) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(at))?;
        let mut buf = Vec::with_capacity(n);
        (&mut self.file).take(n as u64).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// The transport packets of `buf`, which begins on a packet, with the
    /// byte each begins at. `None` for one that is not where a packet
    /// should be: the file is not laid out on one grid from its start.
    fn packets<'a>(&self, at: u64, buf: &'a [u8]) -> impl Iterator<Item = Option<(u64, &'a [u8])>> {
        let size = self.packet as usize;
        let lead = size - 188;
        buf.chunks_exact(size).enumerate().map(move |(i, p)| {
            let ts = &p[lead..];
            (ts[0] == 0x47).then_some((at + (i * size) as u64, ts))
        })
    }

    /// Where the packet that holds byte `at` begins.
    fn grid(&self, at: u64) -> u64 {
        at / self.packet * self.packet
    }
}

/// What one place in a file says about its clock.
enum Sample {
    /// A clock reference that the next one agrees with.
    Clock(i64),
    /// No reference to be believed here.
    Nothing,
    /// The packets are not on the grid from the file's start, or several
    /// programs' clocks are mixed (a whole multiplex): nothing here can say.
    Unreadable,
}

/// The clock where `at` lands.
fn sample(s: &mut Stream, at: u64) -> Sample {
    let mut read = CAT_WINDOW;
    loop {
        let Ok(buf) = s.read(at, read) else { return Sample::Unreadable };
        let mut clocks: Vec<(u16, i64)> = Vec::new();
        for p in s.packets(at, &buf) {
            let Some((_, ts)) = p else { return Sample::Unreadable };
            if let Some(pcr) = pcr_in(ts) {
                clocks.push(((((ts[1] & 0x1F) as u16) << 8) | ts[2] as u16, pcr));
            }
        }
        // Two programs' references taking turns is a whole multiplex, whose
        // clocks have nothing to do with one another.
        if clocks.windows(2).filter(|w| w[0].0 != w[1].0).count() > 1 {
            return Sample::Unreadable;
        }
        let first = clocks.first().map(|c| c.0);
        let mine: Vec<i64> = clocks.iter().filter(|c| Some(c.0) == first).map(|c| c.1).collect();
        if let Some(w) = mine.windows(2).find(|w| (0..=45_000).contains(&apart(w[1], w[0]))) {
            return Sample::Clock(w[0]);
        }
        if read >= CAT_WINDOW_MAX || (buf.len() as u64) < read as u64 {
            return Sample::Nothing;
        }
        read *= 4;
    }
}

/// Where the pictures and the sound of one stretch begin and end, on its own
/// clock.
#[derive(Default)]
struct Ends {
    tracks: Vec<Track>,
}

/// One stream's stamps in a stretch.
struct Track {
    pid: u16,
    video: bool,
    /// MPEG audio or AAC by its PES stream id: sound for certain, where
    /// `0xBD` can be sound or captions.
    heard: bool,
    lo: i64,
    hi: i64,
    /// The last few stamps, for the step between two: where each PES began,
    /// its time, and how many bytes it carried once that is known.
    recent: Vec<(u64, i64, Option<u64>)>,
}

impl Track {
    /// How long one unit of it lasts: the shortest step between two
    /// pictures (a field, where pictures are sent a field at a time), the
    /// usual one between two units of sound. `None` for a stream with too
    /// few stamps or too uneven a step to be sound (captions, subtitles).
    fn step(&self) -> Option<i64> {
        let mut sorted: Vec<i64> = self.recent.iter().map(|r| r.1).collect();
        sorted.sort_unstable();
        let mut steps: Vec<i64> = sorted.windows(2).map(|w| w[1] - w[0]).filter(|d| *d >= 300).collect();
        if self.video {
            return Some(steps.iter().copied().min().unwrap_or(3_003).min(45_000));
        }
        if steps.len() < 8 {
            return None;
        }
        steps.sort_unstable();
        let usual = steps[steps.len() / 2];
        let even = steps.iter().filter(|d| (usual / 2..=usual * 2).contains(*d)).count();
        (usual <= 90_000 && even * 4 >= steps.len() * 3).then_some(usual)
    }

    /// Where the last unit ends. For sound, whose PES headers can be many
    /// frames apart (a muxer gathers frames into one), the last PES lasts as
    /// long as its bytes do at the rate the PES before it were sent at.
    fn end(&self) -> Option<i64> {
        let step = self.step()?;
        if self.video {
            return Some(self.hi + step);
        }
        let last = self.recent.iter().max_by_key(|r| r.1)?;
        let (mut time, mut bytes) = (0i64, 0u64);
        for w in self.recent.windows(2) {
            if let (true, Some(b)) = (w[1].1 > w[0].1, w[0].2) {
                time += w[1].1 - w[0].1;
                bytes += b;
            }
        }
        let lasts = match last.2 {
            Some(b) if bytes > 0 && time > 0 => ((b as f64 * time as f64 / bytes as f64) as i64).min(2 * step),
            _ => step,
        };
        Some(self.hi.max(last.1 + lasts))
    }
}

impl Ends {
    /// How many bytes the PES that began at `at` carried.
    fn size(&mut self, at: u64, pid: u16, bytes: u64) {
        if let Some(k) = self.tracks.iter_mut().find(|k| k.pid == pid) {
            if let Some(r) = k.recent.iter_mut().rev().find(|r| r.0 == at) {
                r.2 = Some(bytes);
            }
        }
    }

    fn feed(&mut self, at: u64, pid: u16, t: i64, video: bool, heard: bool) {
        let track = match self.tracks.iter().position(|k| k.pid == pid) {
            Some(i) => &mut self.tracks[i],
            None => {
                self.tracks.push(Track {
                    pid,
                    video,
                    heard,
                    lo: t,
                    hi: t,
                    recent: Vec::new(),
                });
                self.tracks.last_mut().unwrap()
            }
        };
        track.lo = track.lo.min(t);
        track.hi = track.hi.max(t);
        track.recent.push((at, t, None));
        if track.recent.len() > 48 {
            track.recent.remove(0);
        }
    }

    /// Where the stretch begins and where it ends, in ticks: where its
    /// pictures do, unless its sound runs on (or starts) more than two
    /// pictures away from them -- a recording played on its own goes on
    /// sounding over its last picture, and the next is not to be laid over
    /// that.
    fn span(&self) -> Option<(i64, i64)> {
        let ends = |video: bool| {
            self.tracks
                .iter()
                .filter(|k| k.video == video)
                .filter_map(|k| Some((k.lo, k.end()?)))
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
        };
        let step = self
            .tracks
            .iter()
            .filter(|k| k.video)
            .filter_map(Track::step)
            .min()
            .unwrap_or(3_003);
        match (ends(true), ends(false)) {
            (Some((lo, hi)), None) => Some((lo, hi)),
            (Some((lo, hi)), Some((slo, shi))) => Some((
                if slo < lo - 2 * step { slo } else { lo },
                if shi > hi + 2 * step { shi } else { hi },
            )),
            (None, sound) => sound,
        }
    }
}

/// One stretch as the whole read found it.
struct Stretch {
    at: u64,
    /// Its first clock reference, which everything in it is measured from.
    origin: i64,
    ends: Ends,
}

/// A clock reference that disagreed with the clock so far, waiting to be
/// believed or not.
struct Doubt {
    at: u64,
    first: i64,
    last: i64,
    /// The references it is made of, with where each was met.
    clocks: Vec<(u64, i64)>,
}

/// What the whole read met, in the order of the bytes, and not yet put in a
/// stretch.
#[derive(Clone, Copy)]
enum Met {
    /// A clock reference.
    Clock(u64, i64),
    /// A PES header's presentation and decode times, whether a picture's,
    /// and whether its decode time agreed with the clock reference sent last
    /// (a stamp hit by damage does not, and says nothing about where a
    /// stretch ends). The decode time is what is judged: a unit is decoded
    /// soon after it arrives, while a picture can be shown long after -- a
    /// still in a variable-rate recording with B pictures is shown as many
    /// seconds after it is sent as the still lasts.
    Stamp(u64, u16, i64, i64, Kind, bool),
    /// How many bytes the PES that began at the byte given carried, known
    /// when the next one on its PID begins.
    Size(u64, u16, u64),
}

impl Met {
    fn at(&self) -> u64 {
        match *self {
            Met::Clock(at, _) | Met::Stamp(at, ..) | Met::Size(at, ..) => at,
        }
    }
}

/// Put what was met into `stretch`, on the clock `clock` (its raw value, and
/// how far that is from the stretch's origin). Answers the clock afterwards.
fn settle(stretch: &mut Stretch, mut clock: (i64, i64), met: &mut Vec<Met>) -> (i64, i64) {
    for m in met.drain(..) {
        match m {
            Met::Clock(_, v) => {
                let d = apart(v, clock.0);
                if d >= 0 {
                    clock = (v, clock.1 + d);
                }
            }
            Met::Stamp(at, pid, t, _, kind, sane) => {
                if sane {
                    let (video, heard) = (kind == Kind::Picture, kind == Kind::Sound);
                    stretch.ends.feed(at, pid, clock.1 + apart(t, clock.0), video, heard);
                }
            }
            Met::Size(at, pid, bytes) => stretch.ends.size(at, pid, bytes),
        }
    }
    clock
}

/// Read the whole of the stream for where its clock goes back.
fn whole(s: &mut Stream) -> Cat {
    const BLOCK: usize = 4 << 20;
    let mut stretches: Vec<Stretch> = Vec::new();
    // The clock as last believed.
    let mut clock: Option<(i64, i64)> = None;
    let mut doubt: Option<Doubt> = None;
    // The reference met last, believed or not: what a stamp is judged by.
    let mut near: Option<i64> = None;
    let mut met: Vec<Met> = Vec::new();
    // Each PID's PES in progress: where it began and the bytes so far.
    let mut open: Vec<(u16, u64, u64)> = Vec::new();
    // Where the program maps are, and every (PID, stream type) they named.
    let (mut maps, mut types): (Vec<u16>, Vec<(u16, u8)>) = (Vec::new(), Vec::new());
    let mut pos = 0u64;
    while pos < s.len {
        let Ok(buf) = s.read(pos, BLOCK) else { return Cat::One };
        if buf.len() < s.packet as usize {
            break;
        }
        for p in s.packets(pos, &buf) {
            let Some((at, ts)) = p else { return Cat::One };
            let pid = (((ts[1] & 0x1F) as u16) << 8) | ts[2] as u16;
            if pid == 0 || maps.contains(&pid) {
                tables(ts, &mut maps, &mut types);
            }
            let control = (ts[3] >> 4) & 0x3;
            let carried = match control {
                1 => 184,
                3 => 183u64.saturating_sub(ts[4] as u64),
                _ => 0,
            };
            let unit = ts[1] & 0x40 != 0 && carried > 0;
            match open.iter().position(|o| o.0 == pid) {
                Some(i) if unit => {
                    let (_, began, bytes) = open[i];
                    met.push(Met::Size(began, pid, bytes));
                    open[i] = (pid, at, carried);
                }
                Some(i) => open[i].2 += carried,
                None if unit => open.push((pid, at, carried)),
                None => {}
            }
            let pcr = pcr_in(ts);
            if let Some(v) = pcr {
                near = Some(v);
            }
            if let Some((pts, dts, kind)) = stamp_in(ts) {
                let sane =
                    near.is_none_or(|n| (CAT_SANE.0..=CAT_SANE.1).contains(&apart(dts, n)));
                met.push(Met::Stamp(at, pid, pts, dts, kind, sane));
                if met.len() > 1 << 20 {
                    // A million stamps with no reference believed between
                    // them: not a stream this can read for its clock.
                    return Cat::One;
                }
            }
            let Some(pcr) = pcr else { continue };
            let Some(now) = clock else {
                stretches.push(Stretch {
                    at: 0,
                    origin: pcr,
                    ends: Ends::default(),
                });
                met.push(Met::Clock(at, pcr));
                clock = Some(settle(stretches.last_mut().unwrap(), (pcr, 0), &mut met));
                continue;
            };
            let d = apart(pcr, now.0);
            // On the clock so far: forwards, by no more than a reference can
            // be late, or back by no more than it can be early.
            if (-CAT_BACK..=90_000).contains(&d) {
                doubt = None;
                met.push(Met::Clock(at, pcr));
                clock = Some(settle(stretches.last_mut().unwrap(), now, &mut met));
                continue;
            }
            match &mut doubt {
                Some(o) if (0..=60 * 90_000).contains(&apart(pcr, o.last)) => {
                    o.last = pcr;
                    o.clocks.push((at, pcr));
                }
                _ => {
                    doubt = Some(Doubt {
                        at,
                        first: pcr,
                        last: pcr,
                        clocks: vec![(at, pcr)],
                    })
                }
            }
            if doubt.as_ref().is_some_and(|o| o.clocks.len() < CAT_HOLD) {
                continue;
            }
            let o = doubt.take().unwrap();
            let step = apart(o.first, now.0);
            // The references go back in where they were met.
            let with = |met: &mut Vec<Met>| {
                met.extend(o.clocks.iter().map(|&(at, v)| Met::Clock(at, v)));
                met.sort_by_key(Met::at);
            };
            if step > 0 {
                // A clock that leapt forwards and stayed there: a pause, a
                // still with nothing sent, packets lost. One recording still,
                // read as it always was.
                with(&mut met);
                clock = Some(settle(stretches.last_mut().unwrap(), now, &mut met));
                continue;
            }
            if -step < CAT_APART {
                // Too little to tell the two sides apart by. Read as it
                // always was, one clock: two recordings of one channel that
                // overlap by a second or two -- the margins of two
                // reservations, joined -- lose exactly the overlap there,
                // which is the right answer for them, and refusing them
                // took a cut 0.9.1 made away.
                return Cat::One;
            }
            // Where the next recording begins: its first stamp, which is
            // more than half the step below the clock it leaves. Nothing of
            // the recording before it may come after that.
            let theirs = |t: i64| apart(t, now.0) < step / 2;
            let first =
                met.iter().position(|m| matches!(*m, Met::Stamp(at, _, _, t, ..) if at < o.at && theirs(t)));
            let begin = first.map_or(o.at, |i| met[i].at().min(o.at));
            if let Some(i) = first {
                if met[i..]
                    .iter()
                    .any(|m| matches!(*m, Met::Stamp(at, _, _, t, _, true) if at < o.at && !theirs(t)))
                {
                    return Cat::Tangled(format!(
                        "the clock of the pictures goes back by {:.3}s at byte {}, and packets of \
                         the two clocks are mixed there",
                        -step as f64 / TICK,
                        begin
                    ));
                }
            }
            // What the next recording sent before its first reference was
            // judged by the clock it leaves, and is judged again by its own.
            // Each PES of the recording before that is still open ends
            // here: the next recording's first unit on its PID begins one
            // of its own.
            met.extend(
                open.iter()
                    .filter(|o| o.1 < begin)
                    .map(|&(pid, began, bytes)| Met::Size(began, pid, bytes)),
            );
            open.retain(|o| o.1 >= begin);
            let mut after: Vec<Met> = met
                .iter()
                .filter(|m| m.at() >= begin)
                .map(|m| match *m {
                    Met::Stamp(at, pid, t, d, kind, _) if at < o.at => Met::Stamp(
                        at,
                        pid,
                        t,
                        d,
                        kind,
                        (CAT_SANE.0..=CAT_SANE.1).contains(&apart(d, o.first)),
                    ),
                    other => other,
                })
                .collect();
            met.retain(|m| m.at() < begin);
            settle(stretches.last_mut().unwrap(), now, &mut met);
            with(&mut after);
            stretches.push(Stretch {
                at: begin,
                origin: o.first,
                ends: Ends::default(),
            });
            clock = Some(settle(stretches.last_mut().unwrap(), (o.first, 0), &mut after));
        }
        pos += (buf.len() as u64 / s.packet) * s.packet;
    }
    met.extend(open.iter().map(|&(pid, began, bytes)| Met::Size(began, pid, bytes)));
    if let (Some(stretch), Some(c)) = (stretches.last_mut(), clock) {
        settle(stretch, c, &mut met);
    }
    if stretches.len() < 2 {
        return Cat::One;
    }
    // **Each recording carries its own streams**, and a cut carries the ones
    // the first names: two stations' recordings joined, or a camera's with a
    // camcorder's, hold their pictures or their sound under other PIDs, and
    // the second recording would be left out of every cut without a word.
    let carried = |st: &Stretch, which: fn(&Track) -> bool| {
        let mut pids: Vec<u16> = st.ends.tracks.iter().filter(|k| which(k)).map(|k| k.pid).collect();
        pids.sort_unstable();
        pids
    };
    type Which = fn(&Track) -> bool;
    let kinds: [(&str, Which); 2] = [("pictures", |k| k.video), ("sound", |k| k.heard)];
    for (what, which) in kinds {
        let first = carried(&stretches[0], which);
        if let Some(st) = stretches.iter().find(|st| carried(st, which) != first) {
            let name = |p: Vec<u16>| p.iter().map(|p| format!("0x{p:04x}")).collect::<Vec<_>>().join(", ");
            return Cat::Tangled(format!(
                "the clock of the pictures goes back at byte {}, where a recording begins that \
                 carries its {} on other streams ({}) than the one before it ({}), so a cut would \
                 keep only one of them",
                st.at,
                what,
                name(carried(st, which)),
                name(first),
            ));
        }
    }
    // **Or carries them coded another way under the same PID**: `cat` of an
    // H.264 file and an MPEG-2 one, both written with ffmpeg's PIDs. The
    // pictures after the join would be read with the first one's decoder
    // and the second recording left out of the cut without a word.
    for k in stretches[0].ends.tracks.iter().filter(|k| k.video || k.heard) {
        let mut named: Vec<u8> = types.iter().filter(|t| t.0 == k.pid).map(|t| t.1).collect();
        named.dedup();
        if named.len() > 1 {
            return Cat::Tangled(format!(
                "the clock of the pictures goes back where a recording begins whose program map \
                 names stream 0x{:04x} as another kind (stream types {}), so a cut would keep only \
                 one of them",
                k.pid,
                named.iter().map(|t| format!("0x{t:02x}")).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    let mut pieces = Vec::with_capacity(stretches.len());
    let mut end = 0i64;
    for (i, st) in stretches.iter().enumerate() {
        let Some((lo, hi)) = st.ends.span() else {
            return Cat::Tangled(format!(
                "the clock of the pictures goes back at byte {}, and the recording after it holds \
                 no stamp to place it by",
                st.at
            ));
        };
        let (lo, hi) = (st.origin + lo, st.origin + hi);
        if i == 0 {
            pieces.push(Piece { at: 0, shift: 0, from: lo });
            end = hi;
        } else {
            pieces.push(Piece {
                at: st.at,
                shift: end - lo,
                from: end,
            });
            end += hi - lo;
        }
    }
    // **On the demuxer's clock.** libavformat takes 2^33 off every time of a
    // recording whose first stamp falls in the last minute before the wrap
    // (its AV_PTS_WRAP_SUB_OFFSET), and the seams are timed against what it
    // reports. Left on the unwrapped clock, a join after a recording that ran
    // across the wrap was planned 26.5 hours on, and a cut of the whole file
    // wrote the first recording's pictures and none of the second's.
    let first = pieces[0].from.rem_euclid(WRAP);
    let theirs = if first >= WRAP - 60 * 90_000 { first - WRAP } else { first };
    let by = theirs - pieces[0].from;
    for p in pieces.iter_mut() {
        p.from += by;
    }
    Cat::Joined(Restamp {
        pieces,
        packet: s.packet as usize,
        found: true,
    })
}

/// Read a program association or program map section that begins in this
/// packet and ends in it (every one a recording opens with does): the PIDs
/// of the maps into `maps`, and each (PID, stream type) a map names into
/// `types`, in the order first met. Anything longer or malformed is passed
/// over: this only ever adds a reason to refuse a file already found to be
/// recordings joined end to end.
fn tables(ts: &[u8], maps: &mut Vec<u16>, types: &mut Vec<(u16, u8)>) {
    if ts[1] & 0x40 == 0 || (ts[3] >> 4) & 0x1 == 0 {
        return;
    }
    let payload = if (ts[3] >> 4) & 0x2 != 0 { 5 + ts[4] as usize } else { 4 };
    let Some(&pointer) = ts.get(payload) else { return };
    let Some(sec) = ts.get(payload + 1 + pointer as usize..) else { return };
    if sec.len() < 12 {
        return;
    }
    let len = (((sec[1] & 0x0F) as usize) << 8) | sec[2] as usize;
    let Some(body) = sec.get(..3 + len) else { return };
    if body.len() < 16 {
        return;
    }
    let entries = &body[..body.len() - 4];
    match body[0] {
        0x00 => {
            for e in entries[8..].as_chunks::<4>().0 {
                let program = ((e[0] as u16) << 8) | e[1] as u16;
                let pid = (((e[2] & 0x1F) as u16) << 8) | e[3] as u16;
                if program != 0 && !maps.contains(&pid) && maps.len() < 64 {
                    maps.push(pid);
                }
            }
        }
        0x02 => {
            let info = (((body[10] & 0x0F) as usize) << 8) | body[11] as usize;
            let mut i = 12 + info;
            while i + 5 <= entries.len() {
                let kind = entries[i];
                let pid = (((entries[i + 1] & 0x1F) as u16) << 8) | entries[i + 2] as u16;
                if !types.contains(&(pid, kind)) && types.len() < 256 {
                    types.push((pid, kind));
                }
                i += 5 + ((((entries[i + 3] & 0x0F) as usize) << 8) | entries[i + 4] as usize);
            }
        }
        _ => {}
    }
}

/// Whether the transport stream in `path` holds recordings joined end to
/// end, and if it does, how to read them on one clock.
///
/// **Paid by every transport stream, so it costs next to nothing**: a large
/// file is looked at in its head and at its end, which the demuxer reads
/// anyway as it opens the recording. A clock that goes back between them
/// means the file is read whole for where it does. Recordings of one length
/// joined end to end, each from the same clock, end on a clock far short of
/// where the head's pace says the bytes end ([`CAT_SHORT`]); such a file is
/// looked at in [`CAT_SAMPLES`] places more, several at a time, and read
/// whole if one is behind the place before it. A file of [`CAT_WHOLE`] or
/// less is read whole straight away. What none of this catches the walk
/// does, as a picture's clock going back, and asks [`confirm`] about it:
/// then the recording is read again on one clock (see
/// [`crate::scan_stoppable`]).
///
/// What is found is kept for the life of the process, against the file's
/// size and time.
pub fn cat_of(path: &std::path::Path) -> Cat {
    let Ok(meta) = std::fs::metadata(path) else { return Cat::One };
    if !meta.is_file() {
        return Cat::One;
    }
    let stamp = (meta.len(), meta.modified().ok());
    {
        let kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(k) = kept.iter().find(|k| k.0 == path && (k.1, k.2) == stamp) {
            return k.3.clone();
        }
    }
    let found = read_for_joins(path, meta.len());
    keep(path, &meta, found.clone());
    found
}

/// What [`cat_of`] keeps: what was found for a file, against its size and
/// time.
type Kept = (std::path::PathBuf, u64, Option<std::time::SystemTime>, Cat);
static KEPT: std::sync::Mutex<Vec<Kept>> = std::sync::Mutex::new(Vec::new());

fn keep(path: &std::path::Path, meta: &std::fs::Metadata, found: Cat) {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|k| k.0 != path);
    if kept.len() >= 64 {
        // A join found by the walk ([`confirm`]) or taken from an index
        // ([`adopt`]) is not found again by [`cat_of`]'s look: forgotten, a
        // later parse of the file read it on another clock than the open it
        // belongs to. Ordinary files go first; they cost a look to find again.
        let i = kept.iter().position(|k| k.3 == Cat::One).unwrap_or(0);
        kept.remove(i);
    }
    kept.push((path.to_path_buf(), meta.len(), meta.modified().ok(), found));
}

/// Open `path` as a transport stream, settling how its packets are laid out.
fn stream(path: &std::path::Path, len: u64) -> Option<Stream> {
    let file = std::fs::File::open(path).ok()?;
    let mut s = Stream { file, len, packet: 188 };
    let head = s.read(0, 4 * 192).ok()?;
    let on = |lead: usize, size: usize| (0..4).all(|k| head.get(lead + k * size) == Some(&0x47));
    s.packet = if on(0, 188) {
        188
    } else if on(4, 192) {
        192
    } else {
        return None;
    };
    (len >= 64 * s.packet).then_some(s)
}

/// How much of the head is read for its clock and its pace: no more than the
/// demuxer reads of it anyway when it opens the recording.
const CAT_HEAD: usize = 2 << 20;

/// A clock that covers less than this share of what the bytes would take at
/// the head's pace is looked at more closely ([`CAT_SAMPLES`] places): `cat`
/// of N recordings of one length, each from the same clock, covers 1/N.
const CAT_SHORT: f64 = 0.6;

/// The clock references of the first program in `buf`, with the byte each
/// was met at. `None` for a whole multiplex, or bytes off the grid.
fn clocks(s: &Stream, at: u64, buf: &[u8]) -> Option<Vec<(u64, i64)>> {
    let mut all: Vec<(u64, u16, i64)> = Vec::new();
    for p in s.packets(at, buf) {
        let (at, ts) = p?;
        if let Some(pcr) = pcr_in(ts) {
            all.push((at, (((ts[1] & 0x1F) as u16) << 8) | ts[2] as u16, pcr));
        }
    }
    if all.windows(2).filter(|w| w[0].1 != w[1].1).count() > 1 {
        return None;
    }
    let first = all.first().map(|c| c.1);
    Some(all.into_iter().filter(|c| Some(c.1) == first).map(|c| (c.0, c.2)).collect())
}

/// Whether a run of clock references goes back anywhere, by the rule the
/// whole read uses: a reference behind the one before, that the next
/// [`CAT_HOLD`] - 1 agree with.
fn goes_back(clocks: &[(u64, i64)]) -> bool {
    clocks.windows(CAT_HOLD + 1).any(|w| {
        apart(w[1].1, w[0].1) < -CAT_BACK
            && w[1..].windows(2).all(|v| (0..=45_000).contains(&apart(v[1].1, v[0].1)))
    })
}

/// The references of `clocks` that a neighbour agrees with (the next one
/// sent within half a second after it, or the one before within half a
/// second ahead of it). A lone reference off by a power of two -- reception
/// damage -- is left out, and a real step back keeps both its sides.
fn steady(clocks: &[(u64, i64)]) -> Vec<(u64, i64)> {
    let agree = |a: i64, b: i64| (0..=45_000).contains(&apart(b, a));
    (0..clocks.len())
        .filter(|&i| {
            (i > 0 && agree(clocks[i - 1].1, clocks[i].1))
                || (i + 1 < clocks.len() && agree(clocks[i].1, clocks[i + 1].1))
        })
        .map(|i| clocks[i])
        .collect()
}

fn read_for_joins(path: &std::path::Path, len: u64) -> Cat {
    if crate::index::off("SMARTCUT_CAT_JOIN") {
        return Cat::One;
    }
    let Some(mut s) = stream(path, len) else { return Cat::One };
    if len <= CAT_WHOLE {
        return whole(&mut s);
    }
    // The head: its clock, whether it goes back inside it, and its pace.
    let Ok(head) = s.read(0, CAT_HEAD) else { return Cat::One };
    let Some(first) = clocks(&s, 0, &head) else { return Cat::One };
    // Without a reference a neighbour disagrees with: one hit by reception
    // damage made every parse read the whole file.
    let first = steady(&first);
    if goes_back(&first) {
        return whole(&mut s);
    }
    let Some(&(at0, p0)) = first.first() else { return Cat::One };
    let pace = first.last().and_then(|&(at1, p1)| {
        let secs = apart(p1, p0) as f64 / TICK;
        (secs >= 0.5 && at1 > at0).then(|| (at1 - at0) as f64 / secs)
    });
    // The end, which the demuxer reads too, to say how long the recording is.
    let tail = s.grid(len.saturating_sub(CAT_WINDOW as u64));
    let end = match sample(&mut s, tail) {
        Sample::Unreadable => return Cat::One,
        Sample::Nothing => None,
        Sample::Clock(v) => {
            if first.last().is_some_and(|l| apart(v, l.1) < -CAT_BACK) {
                return whole(&mut s);
            }
            Some((tail, v))
        }
    };
    // A clock that ends far short of where the bytes say it should is
    // several clocks over the same stretch of time: looked at in many more
    // places for where one goes back.
    let short = match (pace, end) {
        (Some(pace), Some((at, v))) if at > at0 => {
            apart(v, p0) as f64 / TICK < CAT_SHORT * (at - at0) as f64 / pace
        }
        _ => false,
    };
    if !short {
        return Cat::One;
    }
    let mut last: Option<i64> = None;
    let places: Vec<u64> = (0..CAT_SAMPLES)
        .map(|i| {
            let jitter = ((i as f64 * 0.618_034).fract() - 0.5) * 0.5;
            s.grid(((i as f64 + jitter).max(0.0) / CAT_SAMPLES as f64 * len as f64) as u64)
        })
        .collect();
    for seen in samples(path, &s, &places) {
        match seen {
            Sample::Unreadable => return Cat::One,
            Sample::Nothing => {}
            Sample::Clock(v) => {
                if last.is_some_and(|l| apart(v, l) < -CAT_BACK) {
                    return whole(&mut s);
                }
                last = Some(v);
            }
        }
    }
    Cat::One
}

/// Take the table an index made of `path` was read through, where this
/// process has not found the join itself (see
/// [`crate::seek_index::SeekIndex`]): every open of it after this one reads
/// through the table. False where `path` is not a file, or the text is not a
/// table found by reading a stream.
pub fn adopt(path: &std::path::Path, table: &str) -> bool {
    let (Ok(meta), Some(table)) = (std::fs::metadata(path), Restamp::decode(table)) else {
        return false;
    };
    if !table.found() || !table.wanted() || !meta.is_file() {
        return false;
    }
    keep(path, &meta, Cat::Joined(table));
    true
}

/// [`sample`] at each of `places`, several at once: on a share each one is a
/// round trip, and they do not wait on one another.
fn samples(path: &std::path::Path, s: &Stream, places: &[u64]) -> Vec<Sample> {
    const AT_ONCE: usize = 8;
    let mut out: Vec<Option<Sample>> = places.iter().map(|_| None).collect();
    let each = places.len().div_ceil(AT_ONCE).max(1);
    std::thread::scope(|scope| {
        for (chunk, slots) in places.chunks(each).zip(out.chunks_mut(each)) {
            scope.spawn(move || {
                let Ok(file) = std::fs::File::open(path) else { return };
                let mut own = Stream {
                    file,
                    len: s.len,
                    packet: s.packet,
                };
                for (at, slot) in chunk.iter().zip(slots.iter_mut()) {
                    *slot = Some(sample(&mut own, *at));
                }
            });
        }
    });
    out.into_iter().map(|o| o.unwrap_or(Sample::Unreadable)).collect()
}

/// Read `path` whole for its joins after all, where reading it found a
/// picture's clock going back at byte `at` that the looks [`cat_of`] takes
/// missed, and keep what it finds for every open after this one. Answers
/// what was found when it is not [`Cat::One`].
///
/// Only where the clock references go back there too: a picture's time put
/// back by reception damage leaves them alone, and a recording with one is
/// read as it always was.
pub fn confirm(path: &std::path::Path, at: u64) -> Option<Cat> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (path.to_path_buf(), meta.len(), meta.modified().ok());
    // Read whole once per process: a file whose clock goes back too little
    // to be split comes here again at every open.
    if READ_WHOLE.lock().unwrap_or_else(|e| e.into_inner()).contains(&stamp) {
        return None;
    }
    let mut s = stream(path, meta.len())?;
    let (before, after) = (s.grid(at.saturating_sub(2 << 20)), s.grid(at));
    let (Sample::Clock(a), Sample::Clock(b)) = (sample(&mut s, before), sample(&mut s, after)) else {
        return None;
    };
    if apart(b, a) >= -CAT_BACK {
        return None;
    }
    let found = whole(&mut s);
    if found == Cat::One {
        let mut read = READ_WHOLE.lock().unwrap_or_else(|e| e.into_inner());
        if read.len() >= 64 {
            read.remove(0);
        }
        read.push(stamp);
        return None;
    }
    keep(path, &meta, found.clone());
    Some(found)
}

/// The files [`confirm`] read whole and found to be one recording after all.
type Stamp = (std::path::PathBuf, u64, Option<std::time::SystemTime>);
static READ_WHOLE: std::sync::Mutex<Vec<Stamp>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport stream of `pictures` at 30 a second from `from` (90 kHz
    /// ticks): each picture one packet on PID 0x100 with a clock reference,
    /// and a sound frame on PID 0x101 every 1920 ticks, each its own PES.
    fn recording(from: i64, pictures: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut sound = from;
        for n in 0..pictures {
            let t = from + n as i64 * 3_000;
            let mut ts = pes_packet(t + 9_000, t + 6_000);
            ts[1] = 0x41; // unit starts here, pid 0x0100
            // An adaptation field with the clock reference, in front of the PES.
            let pes = ts[4..4 + 19].to_vec();
            let mut p = vec![0xFFu8; 188];
            p[0] = 0x47;
            p[1] = 0x41;
            p[2] = 0x00;
            p[3] = 0x30;
            p[4] = 7;
            p[5] = 0x10;
            let base = t;
            p[6] = (base >> 25) as u8;
            p[7] = (base >> 17) as u8;
            p[8] = (base >> 9) as u8;
            p[9] = (base >> 1) as u8;
            p[10] = ((base as u8) << 7) | 0x7E;
            p[11] = 0;
            p[12..12 + pes.len()].copy_from_slice(&pes);
            out.extend_from_slice(&p);
            while sound < t + 3_000 {
                let mut a = pes_packet(sound, sound);
                a[1] = 0x41;
                a[2] = 0x01;
                a[4 + 3] = 0xC0;
                a[4 + 7] = 0x80; // a presentation time only
                a[4 + 8] = 5;
                out.extend_from_slice(&a);
                sound += 1_920;
            }
        }
        out
    }

    fn cat_of_bytes(name: &str, bytes: &[u8]) -> Cat {
        let at = std::env::temp_dir().join(format!("smartcut-cat-{name}-{}.ts", std::process::id()));
        std::fs::write(&at, bytes).unwrap();
        let found = read_for_joins(&at, bytes.len() as u64);
        let _ = std::fs::remove_file(&at);
        found
    }

    /// One recording, however its clock runs, is read as it always was.
    #[test]
    fn one_clock_is_one_recording() {
        assert_eq!(cat_of_bytes("one", &recording(126_000, 600)), Cat::One);
        // Across the 33-bit wrap.
        assert_eq!(cat_of_bytes("wrap", &recording(WRAP - 90_000, 600)), Cat::One);
        // A clock that leaps forward and stays there: a pause.
        let mut paused = recording(126_000, 300);
        paused.extend(recording(126_000 + 300 * 3_000 + 40 * 90_000, 300));
        assert_eq!(cat_of_bytes("pause", &paused), Cat::One);
    }

    /// Two recordings joined end to end, each from the same clock: the second
    /// is placed where the first ends, its sound and pictures together.
    #[test]
    fn recordings_joined_end_to_end_are_put_on_one_clock() {
        let a = recording(126_000, 300);
        let mut both = a.clone();
        both.extend(recording(126_000, 200));
        let Cat::Joined(table) = cat_of_bytes("two", &both) else { panic!("not found") };
        assert_eq!(table.packet(), 188);
        assert!(table.found());
        let p = table.pieces();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].at, a.len() as u64);
        // The first's last picture is shown at 126000 + 299 * 3000 + 9000
        // and lasts a picture. The second's sound begins three pictures
        // before its first picture, at 126000, so that is where it is laid.
        assert_eq!(p[0].from, 126_000);
        assert_eq!(p[1].from, 126_000 + 299 * 3_000 + 9_000 + 3_000);
        assert_eq!(p[1].shift, p[1].from - 126_000);
        // And the table goes through a name and back.
        assert_eq!(Restamp::decode(&table.encode()), Some(table.clone()));
        // Three of them.
        let mut three = both.clone();
        three.extend(recording(126_000, 100));
        let Cat::Joined(table) = cat_of_bytes("three", &three) else { panic!("not found") };
        assert_eq!(table.pieces().len(), 3);
    }

    /// A step back too small to tell the two sides apart is read as one
    /// clock, as before: two overlapping recordings of one channel lose the
    /// overlap and nothing else.
    #[test]
    fn a_small_step_back_is_one_recording() {
        let mut both = recording(126_000, 300);
        both.extend(recording(126_000 + 200 * 3_000, 200));
        assert_eq!(cat_of_bytes("small", &both), Cat::One);
    }

    /// One clock reference knocked forward by damage in the head of a large
    /// file is not a clock going back: no whole read.
    #[test]
    fn a_damaged_reference_in_the_head_is_steady() {
        let c: Vec<(u64, i64)> = (0..20).map(|k| (k * 1000, 126_000 + k as i64 * 3_000)).collect();
        assert!(!goes_back(&steady(&c)));
        let mut hit = c.clone();
        hit[5].1 += 1 << 31;
        assert!(goes_back(&hit));
        assert!(!goes_back(&steady(&hit)));
        // A real step back stays one.
        let mut back = c.clone();
        for p in back.iter_mut().skip(10) {
            p.1 -= 20 * 90_000;
        }
        assert!(goes_back(&steady(&back)));
    }

    /// Seams are timed on the clock libavformat reports, which takes 2^33
    /// off a recording that starts within its last minute before the wrap.
    #[test]
    fn seams_follow_the_demuxer_across_the_wrap() {
        let a = recording(WRAP - 5 * 90_000, 360);
        let mut both = a.clone();
        both.extend(recording(126_000, 200));
        let Cat::Joined(table) = cat_of_bytes("wrapjoin", &both) else { panic!("not found") };
        let p = table.pieces();
        assert_eq!(p[0].from, -5 * 90_000);
        assert_eq!(p[1].from, -5 * 90_000 + 359 * 3_000 + 9_000 + 3_000);
        assert_eq!(p[1].shift.rem_euclid(WRAP), (p[1].from - 126_000).rem_euclid(WRAP));
    }

    /// A clock reference hit by reception damage, alone, is damage.
    #[test]
    fn one_damaged_clock_reference_is_not_a_join() {
        let mut r = recording(1_260_000, 600);
        // Picture 300's clock reference, its 2^25 bit (373 s) knocked over.
        let at = (0..r.len() / 188)
            .map(|k| k * 188)
            .filter(|k| r[*k + 1] == 0x41 && r[*k + 2] == 0x00)
            .nth(300)
            .unwrap();
        r[at + 6] ^= 0x01;
        assert_eq!(cat_of_bytes("damaged", &r), Cat::One);
    }

    /// Recordings whose pictures are on other PIDs cannot be carried as one.
    #[test]
    fn a_recording_on_other_streams_is_refused() {
        let mut both = recording(126_000, 300);
        let mut b = recording(126_000, 200);
        for k in (0..b.len()).step_by(188) {
            if b[k + 2] == 0x00 {
                b[k + 2] = 0x11;
            }
        }
        both.extend(b);
        assert!(matches!(cat_of_bytes("pids", &both), Cat::Tangled(w) if w.contains("pictures")));
    }

    /// A packet with a PES header in it: a payload unit starting, with a
    /// presentation time and a decode time.
    fn pes_packet(pts: i64, dts: i64) -> Vec<u8> {
        let mut ts = vec![0xFFu8; 188];
        ts[0] = 0x47;
        ts[1] = 0x40 | 0x01; // unit starts here, pid 0x0100
        ts[2] = 0x00;
        ts[3] = 0x10; // payload only
        let pes = &mut ts[4..];
        pes[0] = 0;
        pes[1] = 0;
        pes[2] = 1;
        pes[3] = 0xE0; // video
        pes[4] = 0;
        pes[5] = 0;
        pes[6] = 0x80;
        pes[7] = 0xC0; // both times present
        pes[8] = 10;
        // Zeroed and then moved, which exercises the encoder above on the
        // way in as well as on the way out.
        for b in pes[9..19].iter_mut() {
            *b = 0;
        }
        shift_time(&mut pes[9..14], pts);
        shift_time(&mut pes[14..19], dts);
        ts
    }

    fn read_time(at: &[u8]) -> i64 {
        (((at[0] & 0x0E) as i64) << 29)
            | ((at[1] as i64) << 22)
            | (((at[2] & 0xFE) as i64) << 14)
            | ((at[3] as i64) << 7)
            | ((at[4] & 0xFE) as i64 >> 1)
    }

    /// The times move and nothing else does.
    #[test]
    fn a_presentation_time_moves_by_the_shift() {
        let mut ts = pes_packet(90_000, 87_000);
        let before = ts.clone();
        stamp(&mut ts, 450_000);
        assert_eq!(read_time(&ts[13..18]), 540_000);
        assert_eq!(read_time(&ts[18..23]), 537_000);
        assert_eq!(ts.len(), before.len());
        // The header around them is untouched, marker bits and all.
        assert_eq!(ts[..13], before[..13]);
        assert_eq!(ts[23..], before[23..]);
    }

    /// A clock reference is on the same clock and moves with them.
    #[test]
    fn a_clock_reference_moves_too() {
        let mut ts = vec![0xFFu8; 188];
        ts[0] = 0x47;
        ts[1] = 0x01;
        ts[2] = 0x00;
        ts[3] = 0x20; // adaptation field only
        ts[4] = 183;
        ts[5] = 0x10; // a clock reference follows
        for b in ts[6..12].iter_mut() {
            *b = 0;
        }
        ts[10] = 0x7E; // reserved bits set, extension zero
        stamp(&mut ts, 90_000);
        let base = ((ts[6] as i64) << 25)
            | ((ts[7] as i64) << 17)
            | ((ts[8] as i64) << 9)
            | ((ts[9] as i64) << 1)
            | ((ts[10] as i64) >> 7);
        assert_eq!(base, 90_000);
        assert_eq!(ts[10] & 0x7F, 0x7E); // the extension and its padding
    }

    /// Which piece a byte falls in is decided by position and nothing else,
    /// which is what lets a demuxer seek.
    #[test]
    fn the_shift_is_read_off_the_position() {
        let table = Restamp::new(vec![
            Piece { at: 0, shift: 0, from: 0 },
            Piece {
                at: 1_000,
                shift: 500,
                from: 0,
            },
            Piece {
                at: 2_000,
                shift: -300,
                from: 0,
            },
        ]);
        assert_eq!(table.shift_at(0), 0);
        assert_eq!(table.shift_at(999), 0);
        assert_eq!(table.shift_at(1_000), 500);
        assert_eq!(table.shift_at(1_999), 500);
        assert_eq!(table.shift_at(2_000), -300);
        assert_eq!(table.shift_at(9_999_999), -300);
    }

    /// A whole buffer is corrected packet by packet, and a packet that
    /// straddles a boundary takes the correction of where it starts.
    #[test]
    fn a_buffer_is_corrected_a_packet_at_a_time() {
        let table = Restamp::new(vec![
            Piece { at: 0, shift: 0, from: 0 },
            Piece {
                at: SOURCE_PACKET as u64,
                shift: 90_000,
                from: 0,
            },
        ]);
        let mut buf = Vec::new();
        for _ in 0..2 {
            buf.extend_from_slice(&[0u8; 4]);
            buf.extend_from_slice(&pes_packet(45_000, 45_000));
        }
        table.apply(0, &mut buf);
        assert_eq!(read_time(&buf[17..22]), 45_000);
        assert_eq!(read_time(&buf[SOURCE_PACKET + 17..SOURCE_PACKET + 22]), 135_000);
    }

    /// The table survives the trip through a URL.
    #[test]
    fn a_table_is_written_down_and_read_back() {
        let table = Restamp::new(vec![
            Piece { at: 0, shift: 0, from: 0 },
            Piece {
                at: 1_720_320,
                shift: -1_234,
                from: 0,
            },
        ]);
        assert_eq!(Restamp::decode(&table.encode()), Some(table));
        assert_eq!(Restamp::decode("nonsense"), None);
        // Out of order is not a table.
        assert_eq!(Restamp::decode("100:0,50:0"), None);
    }

    /// A clip that already reads straight through is left alone, and one
    /// written in stretches is a table even where nothing has to move.
    #[test]
    fn one_piece_corrects_nothing() {
        assert!(!Restamp::new(vec![Piece { at: 0, shift: 0, from: 0 }]).wanted());
        assert!(Restamp::new(vec![
            Piece { at: 0, shift: 0, from: 0 },
            Piece { at: 100, shift: 0, from: 0 }
        ])
        .wanted());
        // And a correction of nothing leaves the packet exactly as it was.
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&pes_packet(45_000, 45_000));
        let before = buf.clone();
        Restamp::new(vec![Piece { at: 0, shift: 0, from: 0 }]).apply(0, &mut buf);
        assert_eq!(buf, before);
    }

    /// A time pushed past the end of the clock starts again at the
    /// beginning, and one pulled back before it does the same.
    #[test]
    fn the_clock_wraps_rather_than_overflowing() {
        assert_eq!(wrap(WRAP + 7), 7);
        assert_eq!(wrap(-1), WRAP - 1);
        let mut ts = pes_packet(WRAP - 90_000, WRAP - 90_000);
        stamp(&mut ts, 180_000);
        assert_eq!(read_time(&ts[13..18]), 90_000);
    }
}
