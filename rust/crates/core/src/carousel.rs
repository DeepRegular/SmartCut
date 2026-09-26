//! The data broadcast, carried across a cut.
//!
//! A Japanese broadcast sends more than pictures, sound and captions. Behind
//! the d button there is a small application -- the local forecast, the
//! programme's own pages, the traffic on the roads -- sent as a *carousel*:
//! a set of modules repeated end to end for as long as the programme runs,
//! so that a receiver switching on at any moment has the whole of it within
//! a few seconds. Of forty recordings sampled at random from the corpus,
//! **thirty carry one**, and every one of those is stream type 0x0D with a
//! component tag from 0x40 up. What it costs is real: 0.9% of the packets on
//! a channel that sends a title card, and 22% on one that sends a full set
//! of pages.
//!
//! **None of it can travel through the muxer.** A carousel is sent as
//! sections rather than as a stream of PES packets, and libavformat's
//! demuxer delivers sections only for the tables it reads itself. The stream
//! is listed -- `Unknown: none ([13][0][0][0] / 0x000D)` -- and nothing ever
//! comes out of it: asked to copy one with `-copy_unknown`, ffmpeg writes an
//! entry into its map and **not one packet** behind it. So there is no
//! arrangement of the cut that carries this. The packets have to be taken
//! out of the recording as bytes and put into the finished file as bytes,
//! which is what this module does, and why it hangs off the pass that puts
//! the tables back ([`crate::si::graft`]).
//!
//! That pass is already reading the cut packet by packet and already knows
//! what time each one is at, so carrying the data broadcast is a matter of
//! reading the recording alongside it and dealing the packets back in at the
//! moment they were sent. Nothing in them is timed -- a section carries no
//! stamp, and a carousel means nothing but itself -- so nothing has to be
//! spliced. They are placed, renumbered, and otherwise left exactly as they
//! arrived.
//!
//! **A cut of a carousel is not a broken carousel; it is a shorter one.** The
//! modules go round every 3 to 6.5 seconds -- measured over the 22 modules of
//! one satellite service -- so any kept range longer than about seven seconds
//! holds a whole turn of it and a receiver has everything it needs. What a
//! cut point does is interrupt a module part way through, and a receiver that
//! misses a block waits for the next time round, which is what it does off
//! the air whenever reception drops.

use anyhow::{anyhow, Context, Result};
use std::io::{Seek, SeekFrom};

use crate::si::{framing, pcr_of, pid_of, read_fully, PACKET};

/// The stream type a data carousel is sent under: DSM-CC sections.
///
/// ARIB uses this one and nothing else for the data broadcast -- checked
/// across the sample, where every data stream of every station was 0x0D. The
/// neighbouring types exist (0x0B carries DSM-CC messages, 0x0C a stream of
/// descriptors) and nothing in the corpus uses either, so what is carried is
/// what is actually sent rather than everything the standard allows.
pub const DSMCC_SECTIONS: u8 = 0x0D;

/// Where one kept range sits in the recording it came from.
///
/// The cut is planned in seconds and the recording is read in bytes, and this
/// is the join between them: `pos` is the byte the range's opening picture
/// arrives at -- which the access-point index knows, and which is what makes
/// this cheap on a file of several gigabytes -- `skip` is how far past that
/// picture the range actually begins, and `length` is how long it runs.
///
/// The two times are what keep the reading honest without either clock having
/// to agree with the other. The recording's clock is read from its own
/// packets and the cut's from the file being written; only the *difference*
/// from the start of the range is ever used, so a recording whose clock
/// started at four in the afternoon and a cut whose clock starts at zero
/// describe the same moments.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    /// Byte offset of the packet the range's opening picture arrives in.
    pub pos: i64,
    /// Seconds from that picture to where the range actually begins. Up to
    /// one group of pictures: a range begins where the editor put it, and the
    /// picture it opens on is the access point at or before that.
    pub skip: f64,
    /// How long the range runs, in seconds.
    pub length: f64,
}

/// How far into a recording to look for the packet boundary before giving up.
///
/// Two packets of either size settle it. This is generous because the seek
/// lands on a byte an index recorded, and an index can have been written by
/// an older version of this program.
const SYNC_WINDOW: usize = 1 << 16;

/// The clock wraps every 2^33 ticks, which at 90 kHz is a little over 26
/// hours. A step backwards is that and not a recording read out of order.
const PCR_WRAP: i64 = 1 << 33;

/// How far a range is read for its first clock reference before it is given
/// up on. Many seconds of any broadcast, which sends one every tenth.
const NO_CLOCK: usize = 32 << 20;

/// The recording's data-broadcast packets, in the order they were sent, each
/// with the moment in the finished file it belongs at.
///
/// One of these is opened per kept range, at the byte that range begins on.
/// It reads forward holding one packet at a time: the caller asks what is
/// due, takes it or does not, and the file behind it only moves when the held
/// packet is taken. A range that is finished with -- the clock has run past
/// its end, or the recording has -- answers nothing from then on.
pub struct Reader {
    file: std::io::BufReader<crate::input::Reader>,
    /// 188 or 192. See [`framing`].
    stride: usize,
    /// The PIDs being carried.
    pids: Vec<u16>,
    /// The PID the recording's clock is on, which is what the packets between
    /// references are timed by. Not always the pictures': a satellite service
    /// in the sample keeps its clock on a PID of its own.
    pcr_pid: u16,
    /// The recording's clock in seconds as of the last packet read, with any
    /// wrap unwound into it so that it only ever goes forwards.
    clock: Option<f64>,
    /// The last reading taken, to see a wrap coming.
    last: Option<i64>,
    /// How many wraps have gone by.
    wraps: i64,
    /// What the recording's clock said where this range begins. Not known
    /// until the first reference arrives; [`Span::skip`] is what is added to
    /// it then.
    origin: Option<f64>,
    skip: f64,
    /// What the cut's own clock says at that same moment.
    start: f64,
    /// How long the range runs. The reading stops there.
    length: f64,
    /// The packet read and not yet handed over, and when it is due.
    held: Option<(f64, [u8; PACKET])>,
    /// Set once there is nothing more to hand over.
    done: bool,
}

impl Reader {
    /// Open the recording at the byte one kept range begins on.
    ///
    /// `start` is where that range begins on the cut's own clock, which is
    /// what everything coming out of here is timed against.
    pub fn open(
        input: &crate::input::Input,
        pids: &[u16],
        pcr_pid: u16,
        span: Span,
        start: f64,
    ) -> Result<Self> {
        let mut file = input.open()?;
        let at = span.pos.max(0) as u64;
        file.seek(SeekFrom::Start(at))
            .with_context(|| format!("seeking to {at} for the data broadcast"))?;
        // Where the packets start and how far apart they are, read off the
        // recording at this point rather than assumed: a clip inside a disc
        // image is 192 bytes to the packet and a broadcast recording is 188.
        let mut head = vec![0u8; SYNC_WINDOW];
        let n = read_fully(&mut file, &mut head)?;
        head.truncate(n);
        let (base, stride) =
            framing(&head).ok_or_else(|| anyhow!("no packet boundary at byte {at}"))?;
        file.seek(SeekFrom::Start(at + base as u64))?;
        Ok(Self {
            file: std::io::BufReader::with_capacity(1 << 20, file),
            stride,
            pids: pids.to_vec(),
            pcr_pid,
            clock: None,
            last: None,
            wraps: 0,
            origin: None,
            skip: span.skip,
            start,
            length: span.length,
            held: None,
            done: false,
        })
    }

    /// The next packet due by `now` on the cut's clock, if there is one.
    ///
    /// `None` means nothing is due *yet* as often as it means nothing is left;
    /// either way the caller carries on and asks again at the next packet it
    /// writes.
    pub fn due(&mut self, now: f64) -> Result<Option<[u8; PACKET]>> {
        loop {
            if let Some((at, packet)) = self.held.take() {
                if at > now {
                    self.held = Some((at, packet));
                    return Ok(None);
                }
                return Ok(Some(packet));
            }
            if self.done {
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// Read forward until a carried packet is in hand, or the range ends.
    fn fill(&mut self) -> Result<()> {
        let mut frame = [0u8; crate::si::M2TS_PACKET];
        // How far this has read without the clock arriving. A reference goes
        // out ten times a second; a recording whose map names a clock PID
        // that carries none would otherwise be read to its end, gigabytes,
        // once for every kept range, before anything else is written.
        let mut unclocked = 0usize;
        loop {
            if self.clock.is_none() {
                unclocked += self.stride;
                if unclocked > NO_CLOCK {
                    self.done = true;
                    return Ok(());
                }
            }
            let frame = &mut frame[..self.stride];
            match read_fully(&mut self.file, frame)? {
                n if n == frame.len() => {}
                // The recording ran out mid packet, or ran out. Either way
                // there is no more of it to carry.
                _ => {
                    self.done = true;
                    return Ok(());
                }
            }
            // The seek landed on a sync byte, so the packet is at the front
            // of the frame and a disc's four bytes of arrival time are at
            // the back of it, in front of the next one.
            let packet = &frame[..PACKET];
            // Alignment is settled once, at the seek. A packet that does not
            // begin with a sync byte after that means the recording is not
            // what it was read as, and guessing a new boundary here would
            // deal random bytes into the output on a PID a player reads.
            if packet[0] != 0x47 {
                self.done = true;
                return Ok(());
            }
            let pid = pid_of(packet);
            if pid == self.pcr_pid {
                if let Some(ticks) = pcr_of(packet) {
                    if self.last.is_some_and(|was| ticks < was) {
                        self.wraps += 1;
                    }
                    self.last = Some(ticks);
                    let seconds = (ticks + self.wraps * PCR_WRAP) as f64 / 90_000.0;
                    self.clock = Some(seconds);
                    // The first reference is what the range's own start is
                    // measured from: the picture it opens on arrived about
                    // here, and the range itself begins `skip` later.
                    self.origin.get_or_insert(seconds + self.skip);
                }
            }
            let (Some(clock), Some(origin)) = (self.clock, self.origin) else {
                continue;
            };
            // A packet sent at the far cut point belongs to what comes
            // after it, which for a cut of several ranges is the next range
            // and not this one.
            if clock - origin >= self.length {
                self.done = true;
                return Ok(());
            }
            if !self.pids.contains(&pid) {
                continue;
            }
            // Timed by the last reference seen rather than by anything in
            // the packet, because there is nothing in the packet: a section
            // carries no stamp. So a run of them between two references all
            // land on the earlier one, which is a tenth of a second's worth
            // of slack in where a packet nothing times is placed.
            let at = self.start + (clock - origin);
            // Everything between the opening picture and the cut point
            // itself belongs to what was cut away.
            if at < self.start {
                continue;
            }
            let mut held = [0u8; PACKET];
            held.copy_from_slice(packet);
            self.held = Some((at, held));
            return Ok(());
        }
    }
}

/// What the carried PIDs occupy, in bits per second.
///
/// Read off the recording rather than reasoned about, because the answer
/// ranges over a factor of twenty between stations. The one thing that wants
/// it is the partial stream descriptor a `.ts` written in a recorder's shape
/// carries, which states the peak rate of the whole stream: the cut is
/// measured after the muxer wrote it, and what this pass is about to add to
/// it has to be measured before it is added.
///
/// A window rather than the whole file, for the reason every other window
/// here is one. Zero where the recording has no clock to measure against,
/// which is the honest answer to "how fast" when nothing says how long.
pub fn rate(input: &crate::input::Input, pids: &[u16], pcr_pid: u16, pos: i64) -> f64 {
    const WINDOW: usize = 8 << 20;
    let Ok(mut file) = input.open() else {
        return 0.0;
    };
    if file.seek(SeekFrom::Start(pos.max(0) as u64)).is_err() {
        return 0.0;
    }
    let mut buf = vec![0u8; WINDOW];
    let Ok(n) = read_fully(&mut file, &mut buf) else {
        return 0.0;
    };
    buf.truncate(n);
    let Some((base, stride)) = framing(&buf) else {
        return 0.0;
    };
    let (mut first, mut last) = (None, None);
    let mut carried = 0usize;
    let mut at = base;
    while at + PACKET <= buf.len() {
        let p = &buf[at..at + PACKET];
        at += stride;
        if p[0] != 0x47 {
            break;
        }
        let pid = pid_of(p);
        if pid == pcr_pid {
            if let Some(ticks) = pcr_of(p) {
                first.get_or_insert(ticks);
                last = Some(ticks);
            }
        }
        if pids.contains(&pid) {
            carried += 1;
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return 0.0;
    };
    let seconds = (last - first) as f64 / 90_000.0;
    if seconds <= 0.0 {
        return 0.0;
    }
    (carried * PACKET * 8) as f64 / seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PID the sample recording's clock is on, and the one its carousel
    /// is on. Deliberately not the same PID and deliberately not the
    /// pictures': a satellite service in the corpus keeps its clock apart,
    /// and reading one that does is the case worth writing down.
    const CLOCK: u16 = 0x01FF;
    const DATA: u16 = 0x0140;

    /// Ten clock references a second, and twenty carousel packets between
    /// them, which is the shape of the thing at broadcast rates.
    const PER_SECOND: usize = 10;
    const BETWEEN: usize = 2;

    /// A packet carrying the clock and nothing else, as a recorder writes one
    /// on a PID of its own.
    fn clock_packet(ticks: i64) -> [u8; PACKET] {
        let mut p = [0xFFu8; PACKET];
        p[0] = 0x47;
        p[1] = ((CLOCK >> 8) as u8) & 0x1F;
        p[2] = CLOCK as u8;
        p[3] = 0x20;
        p[4] = (PACKET - 5) as u8;
        p[5] = 0x10;
        p[6] = (ticks >> 25) as u8;
        p[7] = (ticks >> 17) as u8;
        p[8] = (ticks >> 9) as u8;
        p[9] = (ticks >> 1) as u8;
        p[10] = ((ticks as u8) << 7) | 0x7E;
        p
    }

    /// One carousel packet, numbered in its payload so that which of them
    /// came out can be told from what came out.
    fn data_packet(n: u16) -> [u8; PACKET] {
        let mut p = [0u8; PACKET];
        p[0] = 0x47;
        p[1] = 0x40 | (((DATA >> 8) as u8) & 0x1F);
        p[2] = DATA as u8;
        p[3] = 0x10 | (n as u8 & 0x0F);
        p[4] = (n >> 8) as u8;
        p[5] = n as u8;
        p
    }

    fn numbered(p: &[u8; PACKET]) -> u16 {
        ((p[4] as u16) << 8) | p[5] as u16
    }

    /// A recording that runs for `seconds`, with a carousel all through it.
    fn a_recording(name: &str, seconds: usize) -> std::path::PathBuf {
        let mut out = Vec::new();
        let mut n = 0u16;
        for tick in 0..seconds * PER_SECOND {
            out.extend_from_slice(&clock_packet(tick as i64 * 90_000 / PER_SECOND as i64));
            for _ in 0..BETWEEN {
                out.extend_from_slice(&data_packet(n));
                n += 1;
            }
        }
        let at = std::env::temp_dir().join(name);
        std::fs::write(&at, &out).expect("write the sample");
        at
    }

    /// Everything the reader hands over, as the cut's clock runs past it.
    fn drain(reader: &mut Reader, from: f64, to: f64) -> Vec<[u8; PACKET]> {
        let mut out = Vec::new();
        let mut now = from;
        while now <= to {
            while let Some(one) = reader.due(now).expect("reading the sample") {
                out.push(one);
            }
            now += 0.01;
        }
        out
    }

    /// The range is a stretch of the recording, and only that stretch: what
    /// was sent before the cut point and after the one at the far end stays
    /// where it was.
    #[test]
    fn only_what_the_range_holds_is_carried() {
        let at = a_recording("smartcut-carousel-range.ts", 10);
        let input = crate::input::Input::plain(&at.to_string_lossy());
        let span = Span {
            pos: 0,
            skip: 2.0,
            length: 3.0,
        };
        // A cut whose own clock starts at a hundred seconds, which the
        // recording's never said: only the distance from the start of the
        // range is used, so the two never have to agree.
        let mut reader = Reader::open(&input, &[DATA], CLOCK, span, 100.0).expect("open");
        let got = drain(&mut reader, 100.0, 106.0);

        let per_second = (PER_SECOND * BETWEEN) as u16;
        assert_eq!(
            numbered(&got[0]),
            2 * per_second,
            "the first packet carried is the first one sent after the cut point"
        );
        assert_eq!(
            numbered(got.last().expect("something was carried")),
            5 * per_second - 1,
            "and the last is the one before the far cut point"
        );
        assert_eq!(got.len(), 3 * per_second as usize, "three seconds of it");
        let _ = std::fs::remove_file(&at);
    }

    /// Nothing comes out before it was sent. The packets are dealt into the
    /// output where they arrived in the recording, and a caller whose clock
    /// has not got there yet is told there is nothing due.
    #[test]
    fn nothing_arrives_before_its_moment() {
        let at = a_recording("smartcut-carousel-timing.ts", 10);
        let input = crate::input::Input::plain(&at.to_string_lossy());
        let span = Span {
            pos: 0,
            skip: 0.0,
            length: 5.0,
        };
        let mut reader = Reader::open(&input, &[DATA], CLOCK, span, 0.0).expect("open");
        // A second in, a second's worth has come due and no more.
        let first = drain(&mut reader, 0.0, 1.0);
        let per_second = (PER_SECOND * BETWEEN) as usize;
        assert!(
            (first.len() as i64 - per_second as i64).abs() <= BETWEEN as i64,
            "about a second's worth by a second in, got {}",
            first.len()
        );
        // And the rest of it follows as the clock does.
        let rest = drain(&mut reader, 1.0, 5.0);
        assert!(rest.len() > first.len() * 3, "the rest follows the clock");
        let _ = std::fs::remove_file(&at);
    }

    /// What it costs, which is what the partial stream descriptor has to
    /// state before any of it has been written.
    #[test]
    fn the_rate_is_what_the_recording_spends_on_it() {
        let at = a_recording("smartcut-carousel-rate.ts", 10);
        let input = crate::input::Input::plain(&at.to_string_lossy());
        let want = (PER_SECOND * BETWEEN * PACKET * 8) as f64;
        let got = rate(&input, &[DATA], CLOCK, 0);
        assert!(
            (got - want).abs() < want * 0.05,
            "{got} bits per second against {want}"
        );
        let _ = std::fs::remove_file(&at);
    }
}
