//! Writing a disc of recordings.
//!
//! The other side of [`crate::disc`]. That module reads the small index
//! files a Blu-ray carries so that a list of recordings says what the
//! programmes were called; this one writes them, so that a night's cuts
//! leave here as a disc a recorder or a player can be handed rather than as
//! three files called `cut_*.m2ts`.
//!
//! ```text
//! BDAV/
//!   info.bdav              which playlists there are, and what the disc is called
//!   PLAYLIST/00001.rpls    one recording: which clip, from when to when, and its name
//!   CLIPINF/00001.clpi     that clip's own index: what it carries, and every entry point in it
//!   STREAM/00001.m2ts      the transport stream itself
//! ```
//!
//! **The stream is written by the cut and not by this.** Asked for a
//! `.m2ts`, libavformat writes Blu-ray's own framing -- the same packets
//! with four bytes of arrival time in front of each -- and the recording's
//! own tables go back into it exactly as they go into a `.ts`
//! ([`crate::si`]). What is left is the index around it, which is what a
//! directory of `00001.m2ts` says nothing about: which programme is which,
//! when it was recorded, where its chapter points are, and where in the file
//! each picture a player can start at begins.
//!
//! ## What is written from what
//!
//! Every number in the index is read back off the stream that was written,
//! rather than carried forward from the recording it was cut from. A cut is
//! not its source: it is shorter, its timestamps start elsewhere, and where
//! its pictures sit in the file is the muxer's business. So the pass here
//! opens the finished `.m2ts` and asks it.
//!
//! The one thing that cannot be read off the stream is what the programme
//! was called, and that is the whole reason for the disc's index existing.
//! It comes from wherever the recording knew it: the event information a
//! broadcast carries, the table a cut this program made carries instead, or
//! the playlist of the disc the recording was read off in the first place.
//! See [`crate::si::programme`].
//!
//! ## The arrival times
//!
//! A Blu-ray's packets each carry the moment they arrived, in the 27 MHz the
//! clock counts in, and a player uses them to feed its decoder at the rate
//! the recording was made at. libavformat writes that field and, asked for a
//! stream whose rate it was not told, writes nonsense in it -- a counter
//! that steps *backwards* by a fixed amount every packet. So [`stamp`]
//! writes them again from the stream's own clock, which is where the answer
//! actually is.
//!
//! ## What is copied rather than understood
//!
//! Two of these files carry fields this program does not know the meaning
//! of. Where that is so, what is written is what a disc written by a real
//! authoring tool has in that field, and it is marked below. Copying a
//! constant that a player has demonstrably accepted is worth more than
//! writing a zero into a field whose name is unknown.

use anyhow::{bail, Context, Result};
use ffmpeg_next as ff;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::si::Began;

/// The clock every time in a playlist and a clip index is counted in.
const TICK: f64 = 45_000.0;

/// A source packet: a transport stream packet behind four bytes saying when
/// it arrived.
const SOURCE_PACKET: usize = 192;
const PACKET: usize = 188;

/// The 27 MHz the arrival times count in.
const ATS_CLOCK: f64 = 27_000_000.0;

/// The arrival time is thirty bits, so it wraps every forty seconds or so.
/// The two bits above it are a copy permission, and zero is what a recording
/// carries.
const ATS_MASK: u32 = 0x3FFF_FFFF;

/// The rate a BDAV disc is written at, in bytes of transport stream per
/// second.
///
/// A fixed number, and the same one the disc this was written against
/// carries. It is what the format is built around rather than a measurement
/// of any one recording: a player reads it to size the buffer it feeds, and
/// a figure below the rate the stream actually arrives at is the one mistake
/// that matters.
///
/// It is also the rate the arrival times below are spaced at where there is
/// no clock to space them by -- 27 MHz divided by this over a packet is
/// 1571 ticks, which is exactly the step a real disc's arrival times run at.
const RECORDING_RATE: u32 = 3_231_064;

/// Where each thing a playlist says about its recording sits, and how much
/// room it has. The same offsets [`crate::disc`] reads, and the same on both
/// of the discs this was written against.
///
/// ```text
///  50  when it was recorded    7 bytes, binary coded decimal, century first
///  64  the channel's number    2 bytes  -- the three digits a viewer knows
///  67  the channel's name      1 + 20   -- ARIB text
///  88  the programme's name    1 + 255  -- ARIB text
/// 344  what it was about       2 + …    -- ARIB text, lines and all
/// ```
const MADE_AT: usize = 50;
const CHANNEL_AT: usize = 64;
const CHANNEL_NAME_AT: usize = 67;
const CHANNEL_NAME_MAX: usize = 20;
const NAME_LEN_AT: usize = 88;
const NAME_MAX: usize = 255;
const DESCRIPTION_AT: usize = 344;

/// How long the description a playlist opens with is, header and all.
///
/// A fixed 1502 bytes on both of the discs this was written against,
/// whatever they had to put in it -- so the play items always begin at 1546.
/// Which is also what leaves room for a description of nine hundred bytes
/// without the file having to be laid out twice.
const APP_INFO: usize = 1502;
const LIST_AT: usize = 44 + APP_INFO;
/// Where the list of playlists starts in `info.bdav`, and where the disc's
/// own name sits before it.
const TABLE_AT: usize = 320;
const DISC_NAME_AT: usize = 64;

/// One recording, as it is to appear on the disc.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The five digits the three files share: `00001`.
    pub clip: String,
    /// What the programme was called. Written into the playlist as ARIB
    /// text, which is where a recorder looks for it.
    pub name: String,
    /// When it was made. A recorder writes the moment the recording started;
    /// what goes here is the moment the programme went out, where the
    /// recording still said so, and nothing where it did not.
    pub made: Option<Began>,
    /// What the broadcaster said the programme was: the sentence a listing
    /// carries, and the cast and staff under it. What a recorder's own
    /// remote shows when the programme is selected in its list.
    pub description: Option<String>,
    /// The channel it came off: what it calls itself, and the three digits a
    /// viewer knows it by (0 where the recording does not say).
    pub channel: Option<String>,
    pub channel_number: u16,
    /// Chapter points, in seconds from the start of the stream that was
    /// written -- the cut's own timeline, not the recording's.
    pub marks: Vec<f64>,
}

/// Where a disc's files live under the folder it is written into.
pub fn root(at: &Path) -> PathBuf {
    at.join("BDAV")
}

/// Where one recording's stream goes.
pub fn stream_of(at: &Path, clip: &str) -> PathBuf {
    root(at).join("STREAM").join(format!("{clip}.m2ts"))
}

/// Make the disc's directories, and say what to call the next `n`
/// recordings.
///
/// Numbering continues from what is already there. A recorder's disc is
/// added to rather than replaced -- an evening's second batch belongs beside
/// the first -- and nothing this writes ever removes a recording somebody
/// else put on the disc.
pub fn prepare(at: &Path, n: usize) -> Result<Vec<String>> {
    let root = root(at);
    for dir in ["STREAM", "PLAYLIST", "CLIPINF"] {
        std::fs::create_dir_all(root.join(dir))
            .with_context(|| format!("making {}", root.join(dir).display()))?;
    }
    let mut next = 1u32;
    for name in numbered(&root.join("STREAM"), "m2ts")? {
        if let Some(taken) = name.parse::<u32>().ok().filter(|t| *t >= next) {
            next = taken + 1;
        }
    }
    Ok((0..n as u32).map(|i| format!("{:05}", next + i)).collect())
}

/// The five-digit stems of the files of one kind a disc directory holds.
fn numbered(dir: &Path, ext: &str) -> Result<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((stem, found)) = name.rsplit_once('.') else { continue };
        if !found.eq_ignore_ascii_case(ext) || stem.len() != 5 || !stem.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        out.push(stem.to_string());
    }
    out.sort();
    Ok(out)
}

/// Write the index around streams already written into `BDAV/STREAM`.
///
/// Each recording is read back once: the arrival times are written from its
/// own clock, and then the pass that indexes it finds every entry point in
/// it. `on` is told which recording is being read and how far through it is,
/// because on a disc's worth of material this is minutes rather than
/// seconds.
pub fn write(
    at: &Path,
    title: &str,
    recordings: &[Recording],
    on: Option<&(dyn Fn(&str, f64) + Sync)>,
) -> Result<()> {
    let root = root(at);
    for rec in recordings {
        let stream = stream_of(at, &rec.clip);
        if !stream.exists() {
            bail!("{}: the stream this playlist is about is not there", stream.display());
        }
        stamp(&stream)?;
        let clip = read_clip(&stream, on.map(|f| (f, rec.clip.as_str())))?;
        std::fs::write(root.join("CLIPINF").join(format!("{}.clpi", rec.clip)), clpi(&clip))
            .with_context(|| format!("writing the index of {}", rec.clip))?;
        std::fs::write(
            root.join("PLAYLIST").join(format!("{}.rpls", rec.clip)),
            rpls(&rec.clip, &clip, rec),
        )
        .with_context(|| format!("writing the playlist of {}", rec.clip))?;
    }

    // Every playlist on the disc and not only the ones just written: the
    // table is what a recorder reads the disc through, and one that named
    // three of five recordings would have lost two of them.
    let playlists: Vec<String> =
        numbered(&root.join("PLAYLIST"), "rpls")?.iter().map(|n| format!("{n}.rpls")).collect();
    std::fs::write(root.join("info.bdav"), info(&playlists, title))
        .with_context(|| format!("writing {}", root.join("info.bdav").display()))?;
    Ok(())
}

// --- the arrival times ---------------------------------------------------

/// Write each packet's arrival time again, from the clock inside the stream.
///
/// A source packet is a transport stream packet with the moment it arrived
/// in front of it, and what a player does with that is meter the stream into
/// its decoder. The stream itself already says when everything arrives --
/// that is what the programme clock reference is -- so the arrival time of
/// every packet between two clock references is the two of them interpolated
/// by how far through it sits. Which is the same model a recorder uses, and
/// why a recording's arrival times step evenly through a burst and then jump:
/// the jump is where the recorder had nothing to write.
///
/// The file is rewritten beside itself and renamed over, so a failure leaves
/// the stream as it was rather than half stamped.
pub fn stamp(path: &Path) -> Result<u64> {
    let temp = path.with_extension("m2ts.ats");
    let done = (|| -> Result<u64> {
        let mut src = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut dst = BufWriter::with_capacity(1 << 20, std::fs::File::create(&temp)?);
        let mut frame = [0u8; SOURCE_PACKET];
        // Frames whose arrival time is not settled yet, because the clock
        // reference that ends their interval has not arrived. A clock goes
        // out at least every tenth of a second, which is a few hundred
        // packets.
        let mut pending: Vec<[u8; SOURCE_PACKET]> = Vec::new();
        // The last packet whose time is settled: when it arrived, and what
        // the clock said there.
        let mut anchor: Option<(f64, f64)> = None;
        // How far apart two packets are before anything is known: the rate a
        // disc is written at. What the packets in front of the first clock
        // reference are spaced by, there being nothing else to go on for
        // them. Past that the file's own average takes over -- see below.
        let opening = ATS_CLOCK / (RECORDING_RATE as f64 / PACKET as f64);
        let mut written = 0u64;
        // How many packets have been given a time, so that the ones after
        // the last clock reference can be spaced by the average of the file
        // rather than by the interval that happened to come last. The final
        // interval is often a handful of packets the muxer flushed together,
        // whose spacing is nothing like the recording's.
        let mut settled = 0u64;

        let put = |dst: &mut BufWriter<std::fs::File>,
                       frame: &mut [u8; SOURCE_PACKET],
                       at: f64|
         -> Result<()> {
            let ats = (at.max(0.0) as u64 as u32) & ATS_MASK;
            frame[..4].copy_from_slice(&ats.to_be_bytes());
            dst.write_all(frame)?;
            Ok(())
        };

        loop {
            match src.read_exact(&mut frame) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("reading the stream back"),
            }
            if frame[4] != 0x47 {
                bail!("{} is not a Blu-ray transport stream", path.display());
            }
            written += 1;
            pending.push(frame);
            let Some(pcr) = pcr_of(&frame[4..]) else { continue };
            match anchor {
                // The first clock reference. Everything in front of it
                // arrived before it, evenly spaced -- and the file's first
                // packet arrives at zero, which is where a recording starts
                // counting.
                None => {
                    let first = (pending.len() - 1) as f64 * opening;
                    settled += pending.len() as u64;
                    for (i, mut held) in pending.drain(..).enumerate() {
                        put(&mut dst, &mut held, i as f64 * opening)?;
                    }
                    anchor = Some((first, pcr));
                }
                Some((was_at, was_pcr)) => {
                    // The clock is 33 bits and wraps every twenty-six hours,
                    // and a cut is not that long, so a step backwards is the
                    // wrap and nothing else.
                    let mut ran = pcr - was_pcr;
                    if ran < 0.0 {
                        ran += (1u64 << 33) as f64 * 300.0;
                    }
                    let n = pending.len();
                    settled += n as u64;
                    for (i, mut held) in pending.drain(..).enumerate() {
                        put(&mut dst, &mut held, was_at + ran * (i + 1) as f64 / n as f64)?;
                    }
                    anchor = Some((was_at + ran, pcr));
                }
            }
        }
        // Whatever followed the last clock reference, at the rate the file
        // has run at so far.
        let tail = anchor.map_or(0.0, |(at, _)| at);
        let step = match settled {
            0 => opening,
            n => tail / n as f64,
        };
        for (i, mut held) in pending.drain(..).enumerate() {
            put(&mut dst, &mut held, tail + (i + 1) as f64 * step)?;
        }
        dst.flush()?;
        Ok(written)
    })();
    match done {
        Ok(n) => {
            std::fs::rename(&temp, path)
                .with_context(|| format!("replacing {}", path.display()))?;
            Ok(n)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// The programme clock reference a packet carries, in the 27 MHz the arrival
/// times are counted in.
///
/// [`crate::si`] reads the same field in 90 kHz, which is the clock
/// everything about presentation is on. Here the extension matters: it is
/// the difference between placing a packet to the millisecond and to the
/// microsecond.
fn pcr_of(p: &[u8]) -> Option<f64> {
    let afc = (p[3] >> 4) & 0x03;
    if afc < 2 || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    let base = ((p[6] as u64) << 25)
        | ((p[7] as u64) << 17)
        | ((p[8] as u64) << 9)
        | ((p[9] as u64) << 1)
        | ((p[10] as u64) >> 7);
    let ext = (((p[10] as u64) & 0x01) << 8) | p[11] as u64;
    Some((base * 300 + ext) as f64)
}

// --- what the written stream says about itself ---------------------------

/// One elementary stream, as a clip index describes it.
struct Carried {
    pid: u16,
    /// The coding type, which for everything a cut writes is the stream type
    /// its own map declares. Taking it from there rather than from the codec
    /// is what keeps the index and the map from ever disagreeing.
    coding: u8,
    /// What the specification puts after the coding type: a shape and a rate
    /// for video, a channel arrangement and a rate and a language for sound,
    /// nothing at all for a private stream, whose contents the index has no
    /// way to describe.
    attributes: Vec<u8>,
}

/// Everything the index files have to say about one written stream.
struct Clip {
    packets: u32,
    pmt_pid: u16,
    pcr_pid: u16,
    /// The first and last moment a picture is shown, in the 45 kHz a
    /// playlist counts in.
    start: u32,
    end: u32,
    video_pid: u16,
    streams: Vec<Carried>,
    /// Every point a player may start at: when it is shown, and which source
    /// packet it begins in.
    entries: Vec<(u64, u32)>,
}

/// Read a written stream for everything the index around it has to say.
fn read_clip(stream: &Path, on: Option<(&(dyn Fn(&str, f64) + Sync), &str)>) -> Result<Clip> {
    let path = stream.to_string_lossy().into_owned();
    let bytes = std::fs::metadata(stream)?.len();
    // Every entry point in the stream, which is a pass over the whole of it.
    // The same pass the editor makes when a recording is opened, for the
    // same answer: where a player may begin.
    let told = on.map(|(f, clip)| {
        let name = clip.to_string();
        move |done: f64| f(&name, done)
    });
    let src = crate::scan_reporting(
        &path,
        &crate::index::PacketScan,
        told.as_ref().map(|f| f as &(dyn Fn(f64) + Sync)),
    )?;
    let video_pid = video_pid(&path, src.video.stream_index)?;
    let service = crate::si::read_service(&src.input, video_pid).ok();

    let mut streams = Vec::new();
    let declared = |pid: u16, fallback: u8| -> u8 {
        service
            .as_ref()
            .and_then(|s| s.stream(pid))
            .map(|s| s.stream_type)
            .unwrap_or(fallback)
    };
    streams.push(Carried {
        pid: video_pid,
        coding: declared(video_pid, video_coding(&src.video.codec)),
        attributes: video_attributes(&src.video),
    });
    for a in &src.audios {
        let pid = a.pid as u16;
        // The map is the authority where it names a sound coding at all.
        // Where it says "private data" it is not naming one: a recording that
        // came out of an MP4 has no map of its own to carry across, and the
        // muxer that wrote this stream had no Blu-ray coding to give AAC --
        // Blu-ray has none, whatever a Japanese recorder writes -- so it fell
        // back on 0x06, which in a clip index means the reader cannot say
        // what the track is. The codec is known here, so it answers instead.
        let coding = match declared(pid, 0) {
            0 | 0x06 => audio_coding(&a.codec),
            named => named,
        };
        streams.push(Carried { pid, coding, attributes: audio_attributes(a) });
    }
    for c in &src.captions {
        let pid = c.pid as u16;
        // A private stream is listed and not described: what a broadcast
        // sends on one is not something a clip index has a field for, which
        // is why a recorder's own disc says no more about its captions than
        // that they are there.
        streams.push(Carried { pid, coding: declared(pid, 0x06), attributes: Vec::new() });
    }

    let ticks = |t: f64| ((t + src.start_time) * TICK).round().max(0.0) as u32;
    let entries = src
        .points
        .iter()
        .filter(|p| p.pos >= 0)
        .map(|p| (ticks(p.time) as u64, (p.pos as u64 / SOURCE_PACKET as u64) as u32))
        .collect();

    Ok(Clip {
        packets: (bytes / SOURCE_PACKET as u64) as u32,
        pmt_pid: service.as_ref().map_or(0x0100, |s| s.pmt_pid),
        pcr_pid: service.as_ref().map_or(video_pid, |s| s.pcr_pid),
        start: ticks(0.0),
        end: ticks(src.duration),
        video_pid,
        streams,
        entries,
    })
}

/// Which PID the pictures are on.
///
/// Every other stream a [`crate::Source`] describes carries its own PID,
/// because a broadcast names its sound and its captions that way and a cut
/// puts them back where they were. The video does not: there is only ever
/// one of it. So it is asked of the file.
fn video_pid(path: &str, index: usize) -> Result<u16> {
    let ictx = ff::format::input(&path)?;
    Ok(ictx.stream(index).map_or(0, |s| s.id()) as u16)
}

/// The coding type of a picture stream, when its own map did not say.
fn video_coding(codec: &str) -> u8 {
    match codec {
        "mpeg1video" => 0x01,
        "h264" => 0x1B,
        "hevc" => 0x24,
        "vc1" => 0xEA,
        _ => 0x02,
    }
}

/// And of a sound track.
fn audio_coding(codec: &str) -> u8 {
    match codec {
        "ac3" => 0x81,
        "eac3" => 0x84,
        "dts" => 0x82,
        "truehd" | "mlp" => 0x83,
        "pcm_bluray" => 0x80,
        "mp2" | "mp3" => 0x04,
        _ => 0x0F,
    }
}

/// The two bytes a clip index describes a picture stream with: the shape of
/// the frame and the rate it is shown at, then the shape of the picture
/// inside it.
fn video_attributes(v: &crate::VideoInfo) -> Vec<u8> {
    let format = match (v.height, v.interlaced()) {
        (0..=480, true) => 1,
        (0..=480, false) => 3,
        (481..=576, true) => 2,
        (481..=576, false) => 7,
        (577..=720, _) => 5,
        (721..=1080, true) => 4,
        (721..=1080, false) => 6,
        _ => 8,
    };
    // The rates a Blu-ray names. A broadcast is 29.97 or 59.94; film on a
    // disc is 23.976.
    let rate = match v.frame_rate {
        r if r < 23.0 => 1,
        r if r < 24.5 => {
            if (r - 24.0).abs() < 0.01 {
                2
            } else {
                1
            }
        }
        r if r < 26.0 => 3,
        r if r < 31.0 => 4,
        r if r < 51.0 => 6,
        _ => 7,
    };
    // 16:9 unless the picture says otherwise, which on a broadcast means a
    // 4:3 recording of something old.
    let wide = v.sample_aspect_ratio * v.width as f64 / v.height.max(1) as f64 > 1.5;
    vec![(format << 4) | rate, if wide { 0x30 } else { 0x20 }]
}

/// And a sound track: how many channels, at what rate, in what language.
fn audio_attributes(a: &crate::AudioInfo) -> Vec<u8> {
    let channels = match a.channels {
        0 | 1 => 1,
        2 => 3,
        _ => 6,
    };
    let rate = match a.sample_rate {
        r if r >= 192_000 => 5,
        r if r >= 96_000 => 4,
        _ => 1,
    };
    let mut out = vec![(channels << 4) | rate];
    // Three letters, and three zeroes where the recording never said. A
    // language field filled in from nowhere would have a bilingual disc
    // claiming both its tracks were Japanese.
    let lang = a.language.as_deref().unwrap_or("");
    let mut three = [0u8; 3];
    for (slot, b) in three.iter_mut().zip(lang.bytes()) {
        *slot = b;
    }
    out.extend_from_slice(&three);
    out
}

// --- the clip's own index ------------------------------------------------

/// `CLIPINF/000NN.clpi`: what the clip carries, and every point in it a
/// player may start at.
///
/// Five sections behind a header that says where each begins. The last two
/// are empty and still have to be there: a chapter list belongs to the
/// playlist on a recorder's disc, and a maker's private data is by
/// definition nobody else's.
fn clpi(clip: &Clip) -> Vec<u8> {
    let mut info = Vec::new();
    // Reserved, then the kind of stream this is (1, a transport stream) and
    // what it is for (1, a recording).
    info.extend_from_slice(&[0x00, 0x00, 0x01, 0x01]);
    // Reserved, and the flag that would say the clip's arrival clock is
    // offset from its neighbour's -- which is a thing only a disc written in
    // several sittings has.
    info.extend_from_slice(&0u32.to_be_bytes());
    info.extend_from_slice(&RECORDING_RATE.to_be_bytes());
    info.extend_from_slice(&clip.packets.to_be_bytes());
    info.extend_from_slice(&[0u8; 128]);
    // The block that says which network the recording came off. Nothing is
    // claimed here -- the flags that say which of its fields to believe are
    // all clear -- because everything in it is already in the stream's own
    // tables, where a player that wants it will look.
    info.extend_from_slice(&30u16.to_be_bytes());
    info.extend_from_slice(&[0u8; 30]);

    let mut sequence = Vec::new();
    // One arrival-time sequence and one clock sequence: a cut is written in
    // one pass, so its clock runs from one end to the other without a break.
    sequence.push(0x00);
    sequence.push(0x01);
    sequence.extend_from_slice(&0u32.to_be_bytes()); // the sequence starts at the first packet
    sequence.push(0x01);
    sequence.push(0x00);
    sequence.extend_from_slice(&clip.pcr_pid.to_be_bytes());
    sequence.extend_from_slice(&0u32.to_be_bytes());
    sequence.extend_from_slice(&clip.start.to_be_bytes());
    sequence.extend_from_slice(&clip.end.to_be_bytes());

    let mut program = Vec::new();
    program.push(0x00);
    program.push(0x01); // one program sequence: the map does not change
    program.extend_from_slice(&0u32.to_be_bytes());
    program.extend_from_slice(&clip.pmt_pid.to_be_bytes());
    program.push(clip.streams.len() as u8);
    program.push(0x00);
    for s in &clip.streams {
        program.extend_from_slice(&s.pid.to_be_bytes());
        program.push(1 + s.attributes.len() as u8);
        program.push(s.coding);
        program.extend_from_slice(&s.attributes);
    }

    let cpi = ep_map(clip);

    let mut out = Vec::new();
    out.extend_from_slice(b"M2TS0100");
    let head = 40;
    let sequence_at = head + 4 + info.len();
    let program_at = sequence_at + 4 + sequence.len();
    let cpi_at = program_at + 4 + program.len();
    let marks_at = cpi_at + 4 + cpi.len();
    for at in [sequence_at, program_at, cpi_at, marks_at] {
        out.extend_from_slice(&(at as u32).to_be_bytes());
    }
    // Nowhere for a maker's private data to be.
    out.extend_from_slice(&0u32.to_be_bytes());
    out.resize(head, 0);
    // Each section says how long it is and then is that long, which is why
    // the addresses above are the lengths below added up.
    for section in [&info, &sequence, &program, &cpi] {
        out.extend_from_slice(&(section.len() as u32).to_be_bytes());
        out.extend_from_slice(section);
    }
    debug_assert_eq!(out.len(), marks_at);
    // The clip's own chapter list, which is empty: on a disc of recordings
    // the chapter points belong to the playlist.
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}

/// The entry point map: for every picture a player may start at, when it is
/// shown and where in the file it begins.
///
/// This is what makes a disc seekable. Without it a player has to read
/// forward looking for a picture it can decode, which on a two hour
/// recording is the difference between a chapter skip landing at once and
/// landing eventually.
///
/// Each point is written twice over, coarsely and finely, because thirty-two
/// bits per entry is not enough for both a time and a position. The fine
/// entry carries the low bits of each -- eleven of the time, seventeen of
/// the packet number -- and a coarse entry carries the high bits and is
/// written afresh whenever they change, which is every twelve seconds or
/// every 131072 packets, whichever comes first.
fn ep_map(clip: &Clip) -> Vec<u8> {
    let mut coarse: Vec<(u32, u32, u32)> = Vec::new(); // fine id, time, packet
    let mut fine: Vec<(u64, u32)> = Vec::new();
    for &(pts, spn) in &clip.entries {
        let new = match coarse.last() {
            None => true,
            Some(&(_, time, packet)) => {
                (pts >> 19) as u32 != time || (spn >> 17) != (packet >> 17)
            }
        };
        if new {
            coarse.push((fine.len() as u32, (pts >> 19) as u32, spn));
        }
        fine.push((pts, spn));
    }

    let mut body = Vec::new();
    body.push(0x00);
    body.push(0x01); // one stream has entry points in it: the pictures
    body.extend_from_slice(&clip.video_pid.to_be_bytes());
    // Ten bits reserved, then the kind of entry point map this is (1, the
    // ordinary one), then how many of each kind of entry there are.
    let head = ((1u64) << 34) | ((coarse.len() as u64) << 18) | fine.len() as u64;
    body.extend_from_slice(&head.to_be_bytes()[2..]);
    // Where this stream's own map begins, counted from the start of the map
    // as a whole -- which is here, since there is only one of them.
    body.extend_from_slice(&14u32.to_be_bytes());

    let mut one = Vec::new();
    // Where the fine entries begin, counted from the start of this stream's
    // map. The coarse ones come first and are eight bytes each, after the
    // four this address itself takes.
    one.extend_from_slice(&((4 + coarse.len() * 8) as u32).to_be_bytes());
    for &(id, time, packet) in &coarse {
        one.extend_from_slice(&(((id as u32) << 14) | (time & 0x3FFF)).to_be_bytes());
        one.extend_from_slice(&packet.to_be_bytes());
    }
    for &(pts, spn) in &fine {
        // The three bits between the angle-change flag and the time say how
        // far past the entry point its picture ends, for a player fetching
        // exactly one picture and no more. Nothing here measures that, and
        // zero is how the field says so.
        let word = ((((pts >> 9) & 0x7FF) as u32) << 17) | (spn & 0x1_FFFF);
        one.extend_from_slice(&word.to_be_bytes());
    }
    body.extend_from_slice(&one);

    let mut out = Vec::new();
    // Twelve bits reserved, and then which kind of index this is: 1, a map
    // of entry points. There is no other kind a recording has.
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&body);
    out
}

// --- the playlist --------------------------------------------------------

/// `PLAYLIST/000NN.rpls`: one recording -- which clip, from when to when,
/// what it is called, and where its chapter points are.
fn rpls(clip_name: &str, clip: &Clip, rec: &Recording) -> Vec<u8> {
    let mut item = Vec::new();
    item.extend_from_slice(clip_name.as_bytes());
    item.extend_from_slice(b"M2TS");
    // Eleven bits reserved and one saying this is not multi-angle, then how
    // this item follows the one before: 1, which is that it does not.
    item.extend_from_slice(&1u16.to_be_bytes());
    item.push(0x00); // the clock sequence the times below are on
    item.extend_from_slice(&clip.start.to_be_bytes());
    item.extend_from_slice(&clip.end.to_be_bytes());

    let mut list = Vec::new();
    list.extend_from_slice(&0u16.to_be_bytes());
    list.extend_from_slice(&1u16.to_be_bytes()); // one item: one recording, one clip
    list.extend_from_slice(&0u16.to_be_bytes()); // and no sub-path
    list.extend_from_slice(&(item.len() as u16).to_be_bytes());
    list.extend_from_slice(&item);

    let mut marks = Vec::new();
    marks.extend_from_slice(&(rec.marks.len() as u16).to_be_bytes());
    for at in &rec.marks {
        let mut entry = [0u8; MARK];
        // The first four bytes and the four after the time are copied from a
        // disc a real tool wrote; what they mean is not written down
        // anywhere this program can reach, and a player has demonstrably
        // accepted them. The play item, the time and the length of the entry
        // are this program's own.
        entry[..4].copy_from_slice(&[0x05, 0x00, 0x02, 0x12]);
        let when = clip.start as f64 + at * TICK;
        entry[6..10].copy_from_slice(&(when.max(0.0) as u32).to_be_bytes());
        entry[10..14].copy_from_slice(&[0xFF; 4]);
        marks.extend_from_slice(&entry);
    }

    // The description the playlist opens with, written at its own offsets
    // into a field of the size both real discs give it.
    let mut out = vec![0u8; LIST_AT];
    out[..8].copy_from_slice(b"PLST0100");
    // The six bytes in front of the date are the same on both of those
    // discs, eighteen years and two unrelated tools apart, and what they
    // mean is not written down anywhere this program can reach. Copying a
    // constant a player has demonstrably accepted is worth more than writing
    // a zero into a field whose name is unknown.
    out[44..48].copy_from_slice(&[0x00, 0x00, 0x01, 0x00]);
    out[48..50].copy_from_slice(&[0x12, 0x00]);
    if let Some(made) = rec.made {
        out[MADE_AT..MADE_AT + 7].copy_from_slice(&bcd(made));
    }
    out[CHANNEL_AT..CHANNEL_AT + 2].copy_from_slice(&rec.channel_number.to_be_bytes());
    // Each of the three texts is a length and then that many bytes of ARIB.
    // What does not fit is cut at a character; see
    // [`crate::arib::encode_within`].
    let mut text = |at: usize, wide: bool, room: usize, what: &str| {
        let raw = crate::arib::encode_within(what, room);
        let len = raw.len();
        if wide {
            out[at..at + 2].copy_from_slice(&(len as u16).to_be_bytes());
            out[at + 2..at + 2 + len].copy_from_slice(&raw);
        } else {
            out[at] = len as u8;
            out[at + 1..at + 1 + len].copy_from_slice(&raw);
        }
    };
    if let Some(channel) = &rec.channel {
        text(CHANNEL_NAME_AT, false, CHANNEL_NAME_MAX, &crate::arib::one_line(channel));
    }
    text(NAME_LEN_AT, false, NAME_MAX, &crate::arib::one_line(&rec.name));
    if let Some(about) = &rec.description {
        // Whatever is left of the description between where it starts and
        // where the play items do, which is nine hundred bytes and more than
        // a broadcaster has ever been seen to write.
        text(DESCRIPTION_AT, true, LIST_AT - DESCRIPTION_AT - 2, about);
    }

    out.extend_from_slice(&(list.len() as u32).to_be_bytes());
    out.extend_from_slice(&list);
    let marks_at = LIST_AT + 4 + list.len();
    out.extend_from_slice(&(marks.len() as u32).to_be_bytes());
    out.extend_from_slice(&marks);

    out[8..12].copy_from_slice(&(LIST_AT as u32).to_be_bytes());
    out[12..16].copy_from_slice(&(marks_at as u32).to_be_bytes());
    out[16..20].copy_from_slice(&0u32.to_be_bytes()); // no maker's private data
    out[40..44].copy_from_slice(&(APP_INFO as u32).to_be_bytes());
    out
}

/// How long one chapter mark is. A pressed disc's is fourteen bytes; a
/// recorder's is this, with room after the time for a name and a thumbnail
/// that neither this nor the disc it was written against fills in.
const MARK: usize = 46;

/// Seven bytes of binary coded decimal, century first.
fn bcd(t: Began) -> [u8; 7] {
    let pair = |n: u32| ((n / 10) << 4) as u8 | (n % 10) as u8;
    [
        pair(t.year as u32 / 100),
        pair(t.year as u32 % 100),
        pair(t.month as u32),
        pair(t.day as u32),
        pair(t.hour as u32),
        pair(t.minute as u32),
        pair(t.second as u32),
    ]
}

// --- the disc's own index ------------------------------------------------

/// `info.bdav`: which playlists there are, in the order a recorder lists
/// them, and what the disc is called.
fn info(playlists: &[String], title: &str) -> Vec<u8> {
    let mut out = vec![0u8; TABLE_AT];
    out[..8].copy_from_slice(b"BDAV0100");
    out[8..12].copy_from_slice(&(TABLE_AT as u32).to_be_bytes());
    out[12..16].copy_from_slice(&0u32.to_be_bytes()); // no maker's private data
    out[40..44].copy_from_slice(&((TABLE_AT - 44) as u32).to_be_bytes());
    // As in a playlist: what a real tool has in the fields this program
    // cannot name, and its own answer everywhere else.
    out[44..48].copy_from_slice(&[0x00, 0x00, 0x01, 0x00]);
    out[48..52].copy_from_slice(b"0000");
    out[62..64].copy_from_slice(&[0xFF, 0xFF]);
    let text = crate::arib::encode_within(&crate::arib::one_line(title), NAME_MAX);
    out[DISC_NAME_AT..DISC_NAME_AT + text.len()].copy_from_slice(&text);

    let mut table = Vec::new();
    table.extend_from_slice(&(playlists.len() as u16).to_be_bytes());
    for name in playlists {
        table.extend_from_slice(name.as_bytes());
    }
    out.extend_from_slice(&(table.len() as u32).to_be_bytes());
    out.extend_from_slice(&table);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Clip {
        Clip {
            packets: 1000,
            pmt_pid: 0x0100,
            pcr_pid: 0x1001,
            start: 20842,
            end: 20842 + 45000 * 30,
            video_pid: 0x1001,
            streams: vec![
                Carried { pid: 0x1001, coding: 0x02, attributes: vec![0x44, 0x30] },
                Carried { pid: 0x1041, coding: 0x0F, attributes: vec![0x31, b'j', b'p', b'n'] },
                Carried { pid: 0x1201, coding: 0x06, attributes: Vec::new() },
            ],
            entries: vec![(20842, 8), (65842, 900), (1 << 20, 200_000)],
        }
    }

    /// The reader in `disc.rs` is the one thing here that can be pointed at
    /// what this writes, and a disc it cannot read is a disc nothing can.
    #[test]
    fn the_playlist_reads_back() {
        let rec = Recording {
            clip: "00001".into(),
            name: "アニメ 第07話「はじめての遠出」".into(),
            made: Some(Began { year: 2026, month: 8, day: 17, hour: 1, minute: 0, second: 0 }),
            description: Some("いつもの部屋で、いつもの話。".into()),
            channel: Some("衛星第一".into()),
            channel_number: 161,
            marks: vec![0.0, 12.5],
        };
        let raw = rpls("00001", &clip(), &rec);
        assert_eq!(&raw[..8], b"PLST0100");
        // The addresses in the header point at sections that are there.
        let list_at = u32::from_be_bytes(raw[8..12].try_into().unwrap()) as usize;
        let marks_at = u32::from_be_bytes(raw[12..16].try_into().unwrap()) as usize;
        assert!(list_at < marks_at && marks_at < raw.len());
        // And everything a recorder writes about the recording comes back
        // out of it, at the offsets a recorder's own disc has them at.
        let counted = |at: usize| {
            let len = raw[at] as usize;
            crate::arib::decode(&raw[at + 1..at + 1 + len])
        };
        assert_eq!(counted(NAME_LEN_AT), rec.name);
        assert_eq!(counted(CHANNEL_NAME_AT), "衛星第一");
        assert_eq!(
            u16::from_be_bytes(raw[CHANNEL_AT..CHANNEL_AT + 2].try_into().unwrap()),
            161
        );
        let about = u16::from_be_bytes(
            raw[DESCRIPTION_AT..DESCRIPTION_AT + 2].try_into().unwrap(),
        ) as usize;
        assert_eq!(
            crate::arib::decode(&raw[DESCRIPTION_AT + 2..DESCRIPTION_AT + 2 + about]),
            rec.description.unwrap()
        );
        // The play items begin where both real discs put them, whatever went
        // into the description above.
        assert_eq!(list_at, LIST_AT);
        // Two marks, at the clip's own start and twelve and a half seconds
        // into it.
        assert_eq!(u16::from_be_bytes(raw[marks_at + 4..marks_at + 6].try_into().unwrap()), 2);
        let second = marks_at + 6 + MARK;
        let at = u32::from_be_bytes(raw[second + 6..second + 10].try_into().unwrap());
        assert_eq!(at, 20842 + (12.5 * TICK) as u32);
    }

    #[test]
    fn the_clip_index_reads_back() {
        let raw = clpi(&clip());
        assert_eq!(&raw[..8], b"M2TS0100");
        let program_at = u32::from_be_bytes(raw[12..16].try_into().unwrap()) as usize;
        // One program sequence, whose map is on the PID a Blu-ray puts it
        // on, and the three streams it names.
        let body = program_at + 4;
        assert_eq!(raw[body + 1], 1);
        assert_eq!(&raw[body + 6..body + 8], &[0x01, 0x00]);
        assert_eq!(raw[body + 8], 3);
        assert_eq!(&raw[body + 10..body + 12], &[0x10, 0x01]);
    }

    /// The map is what makes a disc seekable, and the one part of it worth
    /// checking by arithmetic: an entry read back the way a player reads it
    /// has to give the time and the packet that went in.
    #[test]
    fn every_entry_point_comes_back() {
        let clip = clip();
        let raw = ep_map(&clip);
        let word = |at: usize| u32::from_be_bytes(raw[at..at + 4].try_into().unwrap());
        // How many of each kind of entry there is: sixteen bits and then
        // eighteen, in the six bytes after the PID.
        let counts = raw[6..12].iter().fold(0u64, |n, b| (n << 8) | *b as u64);
        let coarse_n = ((counts >> 18) & 0xFFFF) as usize;
        let fine_n = (counts & 0x3FFFF) as usize;
        assert_eq!(fine_n, clip.entries.len());
        // Where each half of the map begins: past the directory, and then
        // where the map's own first word says its fine half starts.
        let map_at = 2 + 14;
        let fine_at = map_at + word(map_at) as usize;
        let mut coarse = 0usize;
        for (i, &(pts, spn)) in clip.entries.iter().enumerate() {
            // A player reads the coarse entry a fine one belongs to, which
            // is the last one whose fine id is not past it.
            while coarse + 1 < coarse_n
                && (word(map_at + 4 + (coarse + 1) * 8) >> 14) as usize <= i
            {
                coarse += 1;
            }
            let c = word(map_at + 4 + coarse * 8);
            let c_spn = word(map_at + 4 + coarse * 8 + 4);
            let f = word(fine_at + i * 4);
            let time = (((c & 0x3FFF) & !1) as u64) << 19 | (((f >> 17) & 0x7FF) as u64) << 9;
            let packet = (c_spn & !0x1_FFFF) | (f & 0x1_FFFF);
            assert_eq!(packet, spn, "entry {i}");
            // The time comes back to within the nine bits the map does not
            // carry, which is a fifth of a millisecond.
            assert!(pts - time < 512, "entry {i}: {pts} came back as {time}");
        }
    }

    #[test]
    fn the_disc_index_lists_its_playlists() {
        let raw = info(&["00001.rpls".into(), "00002.rpls".into()], "テストディスク");
        assert_eq!(&raw[..8], b"BDAV0100");
        let at = u32::from_be_bytes(raw[8..12].try_into().unwrap()) as usize;
        assert_eq!(u16::from_be_bytes(raw[at + 4..at + 6].try_into().unwrap()), 2);
        assert_eq!(&raw[at + 6..at + 16], b"00001.rpls");
        let name: Vec<u8> = raw[DISC_NAME_AT..TABLE_AT]
            .iter()
            .copied()
            .take_while(|b| *b != 0)
            .collect();
        assert_eq!(crate::arib::decode(&name), "テストディスク");
    }
}
