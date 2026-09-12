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

/// The correction to apply to a clip, piece by piece.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restamp {
    /// In order, first at zero. A table with fewer than two pieces corrects
    /// nothing and is not built: see [`Restamp::wanted`].
    pieces: Vec<Piece>,
}

impl Restamp {
    /// Build one from pieces already in order.
    pub fn new(pieces: Vec<Piece>) -> Restamp {
        Restamp { pieces }
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

    /// The seams, in seconds on the joined clock: where each stretch after
    /// the first begins. Empty where there is only one stretch.
    pub fn joins(&self) -> Vec<f64> {
        self.pieces
            .iter()
            .skip(1)
            .map(|p| p.from as f64 / TICK)
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
        for (i, packet) in buf.chunks_exact_mut(SOURCE_PACKET).enumerate() {
            let shift = self.shift_at(at + (i * SOURCE_PACKET) as u64);
            if shift != 0 {
                // The four bytes in front are the arrival time, which is on
                // the recorder's own clock rather than the stream's and is
                // read by nothing here. Left as the recorder wrote it.
                stamp(&mut packet[4..], shift);
            }
        }
    }

    /// Encode as text, for the name a demuxer is opened by.
    ///
    /// The table has to travel with the recording, and everything in this
    /// program travels as one string -- so it is written into the URL rather
    /// than read again at the other end. See [`crate::input::demux`].
    pub fn encode(&self) -> String {
        self.pieces
            .iter()
            .map(|p| format!("{}:{}:{}", p.at, p.shift, p.from))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Read back what [`Restamp::encode`] wrote. `None` for anything that is
    /// not a table, which is a URL this module did not write.
    pub fn decode(text: &str) -> Option<Restamp> {
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
        (!pieces.is_empty() && pieces.windows(2).all(|w| w[0].at < w[1].at))
            .then_some(Restamp { pieces })
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
            let moved = wrap(base + shift);
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
    let moved = wrap(v + shift);
    at[0] = (at[0] & 0xF0) | (((moved >> 30) as u8) << 1) | 0x01;
    at[1] = (moved >> 22) as u8;
    at[2] = (((moved >> 15) as u8) << 1) | 0x01;
    at[3] = (moved >> 7) as u8;
    at[4] = ((moved as u8) << 1) | 0x01;
}

/// A time that ran off either end of the clock comes back on at the other,
/// which is what the clock itself does.
fn wrap(v: i64) -> i64 {
    v.rem_euclid(WRAP)
}

#[cfg(test)]
mod tests {
    use super::*;

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
