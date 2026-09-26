//! What a disc holds, and what has to come off for a night's cuts to fit it.
//!
//! A list of cuts is written onto one disc or it is not, and the answer is
//! wanted **before** the writing starts rather than after four hours of it.
//! So this works the size out from what the recordings already say about
//! themselves -- how many bits a second their pictures take, measured while
//! they were indexed, and how many their sound takes -- and from how much of
//! each of them is being kept.
//!
//! What comes out is one number: the share of their own size the pictures
//! must be written back at for the disc to close. That share is the same for
//! every recording in the list, which is the point of it. Each recording's
//! own encoder decided how to spend its bits; giving the busy one a harder
//! time than the quiet one because it happens to be longer would undo that
//! for no reason. See [`crate::cut::CutOptions::video_share`].

use ffmpeg_next as ff;

use crate::Source;

/// A disc, and what it holds.
#[derive(Clone, Copy, Debug)]
pub struct Disc {
    pub name: &'static str,
    pub bytes: u64,
}

/// The discs a recorder or a burner writes. The single and dual layer are
/// what almost everything is; the two above them are BDXL, which many drives
/// and most players cannot read.
pub const DISCS: &[Disc] = &[
    Disc {
        name: "BD-R/RE 25GB",
        bytes: 25_025_314_816,
    },
    Disc {
        name: "BD-R/RE DL 50GB",
        bytes: 50_050_629_632,
    },
    Disc {
        name: "BD-R XL 100GB",
        bytes: 100_103_356_416,
    },
    Disc {
        name: "BD-R XL 128GB",
        bytes: 128_001_769_472,
    },
];

/// How much of a disc to leave alone.
///
/// The estimate below is an estimate: it is built from rates measured over a
/// whole recording and applied to the parts of it being kept, and a cut of
/// the busiest minutes of a programme is dearer per second than the average
/// it was measured against. A disc that turns out a hundred megabytes too
/// large is four hours of writing and a coaster, so the default keeps a
/// little back.
pub const MARGIN: f64 = 0.01;

/// What the disc's own files cost, beyond the streams.
///
/// Measured on the reference discs: a fifty-minute recording's clip index is
/// 28 kB, its playlist 1.6 kB, and the disc's own list of them a few hundred
/// bytes. The index is a table of entry points, so it grows with the length
/// rather than with the size.
fn disc_files(seconds: f64) -> u64 {
    // Saturating, as in [`estimate_at`]: a length out of the window can be
    // anything, and the cast alone already stops at `u64::MAX`.
    ((seconds * 10.0) as u64).saturating_add(4_096)
}

/// And what the stream is padded out to.
///
/// A recorder stops a stream on a 196,608-byte boundary and this program does
/// the same, so a clip costs up to that much more than its contents. Half of
/// it, on average, and the average is what an estimate wants.
const PADDING: u64 = 98_304;

/// What the disc as a whole costs before any recording is on it: the
/// directories, the image's own descriptors, and the room a burner wants.
const DISC_FLOOR: u64 = 1_048_576;

/// What one cut is expected to come to on a disc.
#[derive(Clone, Copy, Debug, Default)]
pub struct Estimate {
    /// How much of the recording is being kept.
    pub seconds: f64,
    /// What it comes to written as it is: pictures, sound, the tables that
    /// describe them and the framing a disc puts round them.
    pub bytes: u64,
    /// Of which the pictures. The only part that can be made smaller.
    pub video_bytes: u64,
    /// Whether the pictures *can* be made smaller -- which is to say whether
    /// they are MPEG-2. See [`crate::cut::CutOptions::video_share`].
    pub can_shrink: bool,
}

/// Where the cut is going, which decides what the framing round it costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Going {
    /// Onto a disc, where every packet carries four more bytes saying when
    /// it arrived.
    Disc,
    /// Into a file, where it does not.
    File,
}

/// How much of a recording a list of kept ranges keeps.
pub fn kept_seconds(keeps: &[(f64, f64)]) -> f64 {
    keeps.iter().map(|(a, b)| (b - a).max(0.0)).sum()
}

/// What a recording costs per second, and whether any of it can be given
/// back.
///
/// Read off a recording once and carried about afterwards. The list window
/// works out what a disc would hold every time a range moves, and neither
/// number changes when one does -- so the recording is not opened again to
/// ask.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rates {
    /// Bits a second the pictures take.
    pub video: f64,
    /// Bits a second the sound takes, all tracks together.
    pub audio: f64,
    /// Whether the pictures can be written back smaller. See
    /// [`crate::cut::CutOptions::video_share`].
    pub can_shrink: bool,
}

/// What a recording costs per second.
///
/// Both rates are measured rather than declared: a transport stream states a
/// bit rate for almost nothing it carries, and the one number in its own
/// header is the multiplex's rather than the programme's.
pub fn rates(src: &Source) -> Rates {
    Rates {
        // The pictures, as counted when the recording was indexed. Where that
        // count is missing -- an index read out of a disc's own map never
        // looked at a picture -- the file's own rate stands in, less what the
        // sound takes, which is what the cut itself falls back on.
        video: src
            .video
            .bit_rate
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or_else(|| file_rate(src) * 0.9),
        audio: src.audios.iter().map(audio_rate).sum(),
        can_shrink: src.video.codec == "mpeg2video",
    }
}

/// What a sound track costs, where the container did not say what it costs.
///
/// Two ways to arrive here and they want opposite answers. Linear PCM states
/// no rate because it does not need one: it is the samples themselves, and
/// what they weigh is arithmetic on the three numbers beside them. Everything
/// else has had its rate taken away rather than never had one -- a recording
/// whose opening belongs to the programme before it carries that programme's
/// sound in its header, and what the container said about it is dropped
/// rather than believed (see [`crate::scan_with`]).
///
/// Sizing a compressed track with the PCM arithmetic puts 1.5 Mbit/s against
/// a track carried at a quarter of that. On the six broadcast recordings
/// measured here four had their opening dropped that way, and the estimate
/// came out 14% over on each of them: eight percent of a disc handed back for
/// nothing. So what a codec is worth at this many channels stands in
/// instead, which is what a re-encode of the track would spend.
pub fn audio_rate(a: &crate::AudioInfo) -> f64 {
    if let Some(stated) = a.bit_rate.filter(|r| *r > 0) {
        return stated as f64;
    }
    let derived = ff::decoder::find_by_name(&a.codec)
        .map(|c| crate::cut::derived_bit_rate(c.id(), a.channels))
        .unwrap_or(0);
    if derived > 0 {
        return derived as f64;
    }
    f64::from(a.sample_rate) * f64::from(a.channels) * f64::from(a.bits.max(16))
}

/// What the sound will be written as, where a command line asked for it to
/// be written differently from how it came.
///
/// Each field is `None` (or [`crate::cut::AudioCodec::Source`]) for "as the
/// recording has it".
#[derive(Clone, Debug, Default)]
pub struct SoundAsked {
    pub codec: crate::cut::AudioCodec,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
    pub bits: Option<u8>,
    pub bit_rate: Option<usize>,
    /// Stream indices left out of the cut.
    pub dropped: Vec<usize>,
    /// Whether the cut is going into a transport stream, where linear PCM
    /// is carried in pairs of channels.
    pub to_ts: bool,
}

/// [`rates`], with the sound counted as it will be written rather than as it
/// came in: a track left out costs nothing, and one re-encoded costs what
/// the encoder spends. The window does the same sum for its disc gauge
/// (`soundRate` in `app.js`); `--fit` asked the recording, and a cut to
/// linear PCM came out a gigabyte and more past the disc it was fitted to.
pub fn rates_as_written(src: &Source, asked: &SoundAsked) -> Rates {
    let mut r = rates(src);
    r.audio = src
        .audios
        .iter()
        .filter(|a| !asked.dropped.contains(&a.stream_index))
        .map(|a| written_rate(a, asked))
        .sum();
    r
}

fn written_rate(a: &crate::AudioInfo, asked: &SoundAsked) -> f64 {
    use crate::cut::AudioCodec;
    let target = match asked.codec {
        AudioCodec::Source => a.codec.as_str(),
        c => c.as_str(),
    };
    let pcm = target == "lpcm" || target.starts_with("pcm_");
    // Lossless sound is carried through frame by frame unless a codec was
    // named for it -- see `plan_audio`.
    let whole = matches!(a.codec.as_str(), "dts" | "truehd" | "mlp")
        && asked.codec == AudioCodec::Source;
    let channels = asked.channels.unwrap_or(a.channels);
    let rewritten = !whole
        && (target != a.codec
            || channels != a.channels
            || asked.sample_rate.is_some_and(|r| r != a.sample_rate)
            || asked.bit_rate.is_some()
            || (pcm && asked.bits.is_some_and(|b| b != a.bits)));
    if !rewritten {
        return audio_rate(a);
    }
    if pcm {
        let rate = asked.sample_rate.unwrap_or(a.sample_rate);
        let bits = asked.bits.unwrap_or(a.bits).max(16);
        let paid = if asked.to_ts { channels + channels % 2 } else { channels };
        return f64::from(rate) * f64::from(paid) * f64::from(bits);
    }
    if let Some(b) = asked.bit_rate {
        return b as f64;
    }
    if target != a.codec {
        return ff::decoder::find_by_name(target)
            .map(|c| crate::cut::derived_bit_rate(c.id(), channels) as f64)
            .unwrap_or_else(|| audio_rate(a));
    }
    // The recording's own codec, folded: its figure comes down with the
    // channels, as the cut takes it down.
    let own = audio_rate(a);
    if a.channels > 0 && channels != a.channels {
        (own * f64::from(channels) / f64::from(a.channels)).max(128_000.0)
    } else {
        own
    }
}

/// What a cut of this recording is expected to come to.
pub fn estimate(src: &Source, keeps: &[(f64, f64)], going: Going) -> Estimate {
    let seconds = if keeps.is_empty() {
        src.duration
    } else {
        kept_seconds(keeps)
    };
    let mut out = estimate_at(rates(src), seconds, going);
    // **A cut into a file carries things a disc has no room for**, and the
    // largest of them by far is the data broadcast: the pages behind the blue
    // button are between a hundredth and a fifth of what a broadcast spends,
    // and nothing measured them. So for a file the recording's own rate
    // stands in for the sum of its parts -- it is the honest witness to
    // everything the recording carries, including what was never counted --
    // wherever it is the larger of the two.
    if going == Going::File {
        let whole = (file_rate(src) / 8.0 * seconds) as u64;
        out.bytes = out.bytes.max(whole);
    }
    out
}

/// The same, for a recording whose rates are already known.
pub fn estimate_at(rates: Rates, seconds: f64, going: Going) -> Estimate {
    let (video_rate, audio_rate) = (rates.video.max(0.0), rates.audio.max(0.0));
    // A file's packets are 188 bytes where a disc's are 192, and everything
    // else about the framing is the same.
    let (framing, sound, extras) = match going {
        Going::Disc => (FRAMING, SOUND_FRAMING, disc_files(seconds).saturating_add(PADDING)),
        Going::File => (FRAMING * 188.0 / 192.0, SOUND_FRAMING * 188.0 / 192.0, 0),
    };
    let video_es = video_rate / 8.0 * seconds;
    let audio_es = audio_rate / 8.0 * seconds;
    let video_bytes = (video_es * framing) as u64;
    // Saturating: a float past the end of a u64 casts to u64::MAX, and the
    // numbers here come from the window (`disc_room`) as well as from a file.
    let bytes = (((video_es * framing + audio_es * sound) * TABLES) as u64).saturating_add(extras);
    Estimate {
        seconds,
        bytes,
        video_bytes,
        can_shrink: rates.can_shrink,
    }
}

/// What the framing round the pictures costs.
///
/// A coded picture is carried in transport packets of 188 bytes, of which 184
/// are payload; a Blu-ray puts four more in front of every packet to say when
/// it arrived; and each picture takes a PES header and leaves its last packet
/// part empty. The first of those is 192/184 and the rest is measured: on the
/// reference disc, a stream whose pictures come to 1.58 MB a second takes
/// 1.67 MB a second of packets.
const FRAMING: f64 = 1.056;

/// And round the sound, which fares worse.
///
/// A sound frame is a fiftieth of the size of a picture and gets a PES header
/// of its own all the same, so the part-empty packet at the end of each one is
/// a far larger share of it. Measured on the same disc: a fifth again.
const SOUND_FRAMING: f64 = 1.4;

/// And what the recording's own account of itself costs on top: the tables
/// that describe the streams, written every frame, and the clock in a packet
/// of its own. See `docs/technical/bdav.md`.
const TABLES: f64 = 1.007;

/// The file's own rate, for a recording whose pictures were never counted.
fn file_rate(src: &Source) -> f64 {
    let bytes = match &src.input.range {
        Some(r) => r.len,
        None => std::fs::metadata(&src.path).map(|m| m.len()).unwrap_or(0),
    };
    if src.duration <= 0.0 || bytes == 0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / src.duration
}

/// What a list of cuts comes to against a disc.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fit {
    /// What they come to written as they are.
    pub bytes: u64,
    /// Of which the pictures.
    pub video_bytes: u64,
    /// The disc, and how much of it is being kept back.
    pub capacity: u64,
    pub usable: u64,
    /// What share of their own size the pictures have to be written at. 1.0
    /// where the list already fits.
    pub share: f64,
    /// Whether it fits as it is.
    pub fits: bool,
    /// Whether it can be made to fit at all. A list whose pictures cannot be
    /// made smaller -- or whose sound alone is larger than the disc -- cannot.
    pub reachable: bool,
}

/// The least a picture may be asked to come to.
///
/// Below about here the pictures stop being a smaller version of themselves.
/// A list that cannot fit without going under this is a list that wants a
/// larger disc or one fewer recording on it, and saying so is more use than
/// writing four hours of mush.
pub const FLOOR: f64 = 0.35;

/// Work out what has to come off.
pub fn fit(estimates: &[Estimate], capacity: u64, margin: f64) -> Fit {
    let bytes: u64 = estimates
        .iter()
        .fold(DISC_FLOOR, |sum, e| sum.saturating_add(e.bytes));
    // Only the pictures of the recordings that have pictures this can rewrite.
    // A recording in H.264 on the same disc still takes its own room; it
    // simply cannot be asked for any of it back.
    let video_bytes: u64 = estimates
        .iter()
        .filter(|e| e.can_shrink)
        .fold(0u64, |sum, e| sum.saturating_add(e.video_bytes));
    let usable = (capacity as f64 * (1.0 - margin.clamp(0.0, 0.5))) as u64;
    if bytes <= usable {
        return Fit {
            bytes,
            video_bytes,
            capacity,
            usable,
            share: 1.0,
            fits: true,
            reachable: true,
        };
    }
    // What is left for the pictures once everything that cannot move has had
    // its room.
    let fixed = bytes.saturating_sub(video_bytes);
    let room = usable.saturating_sub(fixed) as f64;
    let share = if video_bytes == 0 {
        0.0
    } else {
        room / video_bytes as f64
    };
    Fit {
        bytes,
        video_bytes,
        capacity,
        usable,
        share: share.clamp(0.0, 1.0),
        fits: false,
        reachable: share >= FLOOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(codec: &str, channels: u16, bit_rate: Option<usize>) -> crate::AudioInfo {
        crate::AudioInfo {
            stream_index: 1,
            pid: 0x1100,
            language: None,
            codec: codec.into(),
            sample_rate: 48_000,
            channels,
            bits: 16,
            time_base: 1.0 / 90_000.0,
            bit_rate,
            said: None,
        }
    }

    #[test]
    fn a_track_that_states_its_rate_is_taken_at_its_word() {
        assert_eq!(audio_rate(&track("aac", 2, Some(178_000))), 178_000.0);
    }

    /// The case that cost a disc: a recording whose opening belongs to the
    /// programme before it has its stated rate dropped, and the arithmetic
    /// that sizes linear PCM then charged an AAC track 1.5 Mbit/s.
    #[test]
    fn a_compressed_track_with_no_rate_is_not_sized_as_pcm() {
        let pcm_arithmetic = 48_000.0 * 2.0 * 16.0;
        let r = audio_rate(&track("aac", 2, None));
        assert!(r < pcm_arithmetic / 4.0, "{r} is PCM arithmetic, not AAC");
        assert_eq!(r, 192_000.0);
    }

    #[test]
    fn linear_pcm_is_still_its_own_samples() {
        assert_eq!(
            audio_rate(&track("pcm_bluray", 2, None)),
            48_000.0 * 2.0 * 16.0
        );
    }

    /// A rate of nought is a container saying it does not know, which is the
    /// same as saying nothing.
    #[test]
    fn a_stated_nought_is_not_a_rate() {
        assert_eq!(audio_rate(&track("aac", 1, Some(0))), 96_000.0);
    }

    fn clip(bytes: u64, video: u64) -> Estimate {
        Estimate {
            seconds: 3600.0,
            bytes,
            video_bytes: video,
            can_shrink: true,
        }
    }

    #[test]
    fn a_list_that_fits_is_left_alone() {
        let f = fit(&[clip(10_000_000_000, 9_000_000_000)], DISCS[0].bytes, 0.0);
        assert!(f.fits);
        assert_eq!(f.share, 1.0);
    }

    #[test]
    fn the_share_is_what_the_pictures_have_to_give_back() {
        // Thirty gigabytes onto twenty-five, of which twenty-seven are
        // pictures: the three that are not have to stay, so the pictures have
        // to come to (25 - 3) of 27.
        let f = fit(&[clip(30_000_000_000, 27_000_000_000)], 25_000_000_000, 0.0);
        assert!(!f.fits);
        assert!(f.reachable);
        let fixed = 30_000_000_000.0 - 27_000_000_000.0 + DISC_FLOOR as f64;
        let want = (25_000_000_000.0 - fixed) / 27_000_000_000.0;
        assert!((f.share - want).abs() < 1e-6, "{} vs {want}", f.share);
    }

    #[test]
    fn a_margin_holds_some_of_the_disc_back() {
        let tight = fit(&[clip(30_000_000_000, 27_000_000_000)], 25_000_000_000, 0.0);
        let safe = fit(&[clip(30_000_000_000, 27_000_000_000)], 25_000_000_000, 0.02);
        assert!(safe.share < tight.share);
        assert_eq!(safe.usable, 24_500_000_000);
    }

    #[test]
    fn what_cannot_be_reached_says_so() {
        // Sixty gigabytes onto twenty-five is more than the pictures can give.
        let f = fit(&[clip(60_000_000_000, 54_000_000_000)], 25_000_000_000, 0.01);
        assert!(!f.reachable);
    }

    #[test]
    fn a_recording_that_cannot_be_rewritten_still_takes_its_room() {
        let mut fixed = clip(10_000_000_000, 9_000_000_000);
        fixed.can_shrink = false;
        let f = fit(&[fixed, clip(20_000_000_000, 18_000_000_000)], 25_000_000_000, 0.0);
        // Only the second recording's pictures can give anything back, so
        // the share is worked out against those alone -- and the first
        // recording's ten gigabytes count against the disc in full.
        assert_eq!(f.video_bytes, 18_000_000_000);
        let fixed = 30_000_000_000.0 - 18_000_000_000.0 + DISC_FLOOR as f64;
        let want = (25_000_000_000.0 - fixed) / 18_000_000_000.0;
        assert!((f.share - want).abs() < 1e-6, "{} vs {want}", f.share);
    }
}
