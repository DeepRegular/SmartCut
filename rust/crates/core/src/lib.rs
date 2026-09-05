//! Smart-rendering cut engine.
//!
//! Port of the Python reference implementation in `../../../smartcut`, moved
//! onto libav directly so that packets -- and their timestamps -- are ours to
//! place. The prototype drove the ffmpeg CLI and paid for it: an elementary
//! stream carries no timestamps, so ffmpeg had to synthesise them and could
//! not reorder the first few packets, leaving the opening frame 13 ms early.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

pub mod adts;
pub mod arib;
pub mod audio;
pub mod bitstream;
pub mod caption;
pub mod cm;
pub mod cut;
pub mod disc;
pub mod index;
pub mod input;
pub mod logo;
pub mod netpath;
pub mod plan;
pub mod playback_audio;
pub mod preview;
pub mod proxy;
pub mod seek_index;
pub mod si;
pub mod thumbs;
pub mod udf;

pub use cm::{
    blocks as cm_blocks, blocks_from_logo as cm_blocks_from_logo,
    blocks_from_resets as cm_blocks_from_resets, candidates as cm_candidates,
    find_silences, find_silences_with, refine_boundaries as cm_refine_boundaries,
    DetectOptions,
};
pub use adts::{AacVersion, AdtsFormat};
pub use cut::{
    cut, cut_with_progress, writable_sound, write_audio_es, AudioCodec, AudioMode, CutOptions,
    SoundAsIs, SoundChoices,
};
pub use index::{ContainerIndex, IndexSource, PacketScan};
pub use seek_index::SeekIndex;
pub use preview::{frame_at, glance, play_from, shot_at, shots_at, Pace, Shot};
pub use proxy::{Marks, ProxyOptions};
pub use thumbs::{ThumbOptions, Track};
pub use plan::{plan, plan_range, PlanOptions, RangePlan, Segment, SegmentKind};
pub use playback_audio::play_audio;

/// A random access point and the leading pictures that hang off it.
#[derive(Debug, Clone)]
pub struct AccessPoint {
    /// Presentation time of the I picture, rebased to the start of the file.
    pub time: f64,
    /// Earliest presentation time among its leading pictures; equals `time`
    /// when the GOP is closed.
    pub lead_start: f64,
    /// Decode-order offsets, relative to the I picture, of those leading
    /// pictures.
    pub lead_indices: Vec<usize>,
    /// Whether those leading pictures may be cut away, i.e. none of them is
    /// itself a reference picture.
    pub droppable: bool,
    /// Byte offset of the packet the I picture arrives in, or -1 when the
    /// index could not say.
    ///
    /// This is what makes a seek exact. A transport stream has no seek table,
    /// so libavformat answers a timestamp by bisecting the file on byte
    /// position -- which lands near the instant asked for and not on it, and
    /// in decode order an I picture sits *before* its leading pictures, so
    /// landing a little late means missing the entry point entirely. Given
    /// the byte it starts at there is nothing left to approximate.
    pub pos: i64,
}

impl AccessPoint {
    pub fn open_gop(&self) -> bool {
        !self.lead_indices.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub stream_index: usize,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    /// Reorder depth: how far DTS trails PTS.
    pub has_b_frames: i32,
    pub time_base: f64,
    /// Pixel aspect ratio. Broadcast 1440x1080 is not square-pixel.
    pub sample_aspect_ratio: f64,
    pub framing: bitstream::NalFraming,
    /// Whether the stream carries 2:3 pulldown, i.e. pictures are shown for
    /// varying numbers of fields. Such a stream is not constant frame rate at
    /// the picture level, whatever its container claims.
    pub pulldown: bool,
    /// AVFieldOrder from the source. Broadcast material is interlaced, and an
    /// encoder told nothing about that quietly produces progressive pictures
    /// -- which comb against the copied ones at every splice.
    pub field_order: i32,
}

/// Field orders that mean "interlaced" (AV_FIELD_TT/BB/TB/BT).
pub const INTERLACED_FIELD_ORDERS: [i32; 4] = [2, 3, 4, 5];

impl VideoInfo {
    pub fn interlaced(&self) -> bool {
        INTERLACED_FIELD_ORDERS.contains(&self.field_order)
    }

    /// AV_FIELD_TT and AV_FIELD_TB lead with the top field.
    pub fn top_field_first(&self) -> bool {
        self.field_order == 2 || self.field_order == 4
    }

    pub fn frame_duration(&self) -> f64 {
        if self.frame_rate > 0.0 {
            1.0 / self.frame_rate
        } else {
            1.0 / 30.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub stream_index: usize,
    /// The PID this arrived on, when the recording is a transport stream.
    ///
    /// Written back out as it was. A broadcast names its tracks by PID and
    /// by the component tag beside it in the map -- 0x10 the main sound,
    /// 0x11 the second -- and a bilingual recording whose two tracks come
    /// back on fresh PIDs has lost which was which.
    pub pid: i32,
    /// The language the map declared, when it declared one.
    pub language: Option<String>,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// How wide this track's samples are once they are written as linear
    /// PCM: 24 for a recording that has more than 16 bits in it, 16 for
    /// everything else. See [`audio::pcm_bits`].
    ///
    /// Not a description of the recording so much as of what it costs: an
    /// uncompressed track's size is channels times this times the sample
    /// rate, which is the figure the output settings screen shows in place
    /// of a bitrate nobody can choose.
    pub bits: u8,
    pub time_base: f64,
    pub bit_rate: Option<usize>,
}

/// A caption stream: the subtitles the broadcast itself sends.
///
/// Kept as its own thing rather than as one more elementary stream, because
/// it is the one non-audio stream that can be put on a cut timeline. Its
/// packets carry a presentation time each, so they shift with the pictures
/// the way audio frames do -- and unlike audio there is nothing to splice,
/// since a caption statement is whole in one packet.
#[derive(Debug, Clone)]
pub struct CaptionInfo {
    pub stream_index: usize,
    pub pid: i32,
    pub language: Option<String>,
    pub time_base: f64,
}

/// A stream the recording carries that a cut cannot take with it.
///
/// Superimposed crawls and the data broadcast sit on their own PIDs, and
/// neither can be moved onto a new timeline: the demuxer hands over the
/// crawls with no presentation time at all, and the data broadcast is a
/// carousel of sections rather than a stream of timed packets.
///
/// A `"substream"` is the third kind and the odd one: a piece the demuxer
/// split out of a track that *is* being carried. See [`one_track_per_pid`].
///
/// Naming them is so the tool can say what it left behind instead of quietly
/// dropping it.
#[derive(Debug, Clone)]
pub struct DroppedStream {
    pub pid: i32,
    /// Which of them it is: `"superimpose"` or `"data"`. A name rather than
    /// a sentence, because the window says this in the language it is set to
    /// and the command line says it in English.
    pub what: &'static str,
}

impl DroppedStream {
    /// What to call it in English, which is what the command line prints.
    pub fn describe(&self) -> &'static str {
        match self.what {
            "superimpose" => "superimposed text",
            "substream" => "a compatibility stream folded into the track written",
            _ => "data broadcast",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    /// The name the recording is known by: a path, or a path into a disc
    /// image. What the list shows, what the caches are keyed on, and what
    /// the output is named beside.
    pub path: String,
    /// How that name is opened. Everything that hands the recording to
    /// libavformat gives it [`input::Input::url`], which for a clip inside
    /// an image is the range of the image it occupies.
    pub input: input::Input,
    pub video: VideoInfo,
    /// The main sound, which is the track everything that reads one track
    /// reads: commercial detection, the preview player, the sidecar.
    pub audio: Option<AudioInfo>,
    /// Every audio track, in the order the recording carries them.
    ///
    /// A bilingual broadcast sends two -- the original and the dub, or the
    /// commentary and the crowd -- on separate PIDs, and until this existed
    /// the second one was simply not looked at. `audio` is the first of
    /// these unless the container nominated another.
    pub audios: Vec<AudioInfo>,
    /// Every caption stream. More than one means more than one language.
    pub captions: Vec<CaptionInfo>,
    /// What the recording carries that the output cannot; see
    /// [`DroppedStream`].
    pub dropped: Vec<DroppedStream>,
    pub duration: f64,
    /// Container start time. MPEG-TS does not begin at zero.
    pub start_time: f64,
    pub points: Vec<AccessPoint>,
    /// Whether the index could speak for the leading pictures. A precomputed
    /// index cannot; call [`index::refine_leading`] before planning.
    pub leading_known: bool,
    /// Which strategy produced the index, for reporting.
    pub index_name: &'static str,
    /// How far before a target to seek. MPEG-TS seeking is byte-position
    /// based and only approximately honours timestamps, so it can land past
    /// the picture that was asked for -- and in decode order an I picture
    /// sits *before* its leading pictures, so overshooting is invisible
    /// until the entry point simply never turns up.
    ///
    /// The fallback, now: where the index knows the byte an access point
    /// starts at, [`index::seek_to_entry`] goes straight there and no margin
    /// is spent. This is what is left for the containers and the indexes that
    /// cannot say.
    pub seek_margin: f64,
    /// Whether a raw byte offset may be seeked to.
    ///
    /// True of the stream formats, which are demuxed by reading forward from
    /// wherever the file happens to be positioned. Not true of MP4 or
    /// Matroska: their demuxers walk an internal sample table and a
    /// repositioned file handle does not move it, so a byte seek there would
    /// be quietly ignored -- and they carry a real seek table anyway, which
    /// is exact already.
    pub byte_seekable: bool,
}

pub fn init() -> Result<()> {
    ff::init().map_err(|e| anyhow!("ffmpeg init failed: {e}"))
}

/// This crate's own version, which is the version of the cutting engine --
/// the workspace carries one number and the CLI, the engine and the windows
/// all take it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What libav says about itself.
///
/// The versions of the libraries actually loaded, not the ones this was
/// compiled against: the AppImage carries its own and a distribution build
/// takes the system's, and which of them answered is the first thing worth
/// knowing about a recording that decoded differently on one machine than on
/// another.
#[derive(Debug, Clone)]
pub struct Libav {
    /// `major.minor.micro` of libavformat, the demuxer and muxer.
    pub avformat: String,
    /// ...of libavcodec, the decoders and the one encoder.
    pub avcodec: String,
    /// ...of libavutil, which the other two are versioned alongside.
    pub avutil: String,
    /// What the build was licensed under -- "LGPL version 2.1 or later",
    /// or GPL for one built with the GPL-only parts turned on. It is not
    /// this program's licence and it need not agree with it.
    pub license: String,
}

/// Read the loaded libraries' version numbers.
pub fn libav() -> Libav {
    /// libav packs a version into one word, a byte per part.
    fn triple(v: u32) -> String {
        format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
    }
    Libav {
        avformat: triple(ff::format::version()),
        avcodec: triple(ff::codec::version()),
        avutil: triple(ff::util::version()),
        license: ff::util::license().to_string(),
    }
}

/// A video decoder allowed to use every core.
///
/// libavcodec threads only when it is told a number, and its own default is
/// one -- which for a straight pass over 1440x1080 MPEG-2 is most of the wall
/// clock: the same half hour decodes in 23.6s on one core and 10.6s across
/// four. Which *kind* of threading is left to the codec, because they do not
/// all offer the same one (MPEG-2 has slice threading only, H.264 frame
/// threading as well) and `thread_type` already asks for whichever is there.
///
/// For passes over the whole file only. A decoder opened to fetch one
/// picture, or one that stops as soon as it has read far enough, wants its
/// answer back on the packet that carried it -- and frame threading holds
/// pictures back until the pipeline fills, so the last few would never come.
pub fn video_decoder(params: ff::codec::Parameters) -> Result<ff::decoder::Video> {
    video_decoder_with(params, 0)
}

/// As [`video_decoder`], but held to `threads` of them; zero still means
/// every core.
///
/// For a pass that is not the one being waited on. The clip list reads its
/// recordings while another one is open in the cut editor, and a pass that
/// takes every core is precisely what leaves nothing for the picture the
/// pointer is asking for -- the film strip stops following it, which is a
/// worse trade than the pass finishing a few seconds later.
pub fn video_decoder_with(
    params: ff::codec::Parameters,
    threads: usize,
) -> Result<ff::decoder::Video> {
    let mut ctx = ff::codec::context::Context::from_parameters(params)?;
    // ffmpeg-next's `set_threading` writes the kind as well as the count, and
    // naming one kind is exactly how a codec that only has the other ends up
    // single threaded anyway. The count is the one field that needs saying:
    // zero means "as many as this machine has".
    unsafe {
        (*ctx.as_mut_ptr()).thread_count = threads as i32;
    }
    Ok(ctx.decoder().video()?)
}

/// Index the video stream's random access points by walking packets.
///
/// Packets arrive in decode order and need no decoding, which matters twice
/// over: a decode-based scan silently loses entry points on open-GOP streams
/// (the decoder cannot output an I picture whose references are missing), and
/// decoding a long file just to find its keyframes is slow.
///
/// Decode order is also what exposes leading pictures: a picture that follows
/// an I picture in decode order but presents *before* it references the
/// previous GOP, so a copy starting at that I picture cannot include it.
///
/// Each packet's reference flag is read in the same pass, which is what makes
/// `droppable` exact here. The Python prototype could only sample one access
/// point and assume the encoder never changed strategy, because answering the
/// question at all cost it a separate ffmpeg invocation.
pub fn scan(path: &str) -> Result<Source> {
    scan_with(path, &index::PacketScan)
}

/// Container formats whose demuxer reads forward from wherever the file is
/// positioned, and so can be placed by byte offset. See
/// [`Source::byte_seekable`].
const BYTE_SEEKABLE: [&str; 5] = ["mpegts", "mpeg", "h264", "hevc", "mpegvideo"];

/// One sound track per PID, and what that leaves out.
///
/// A PID is how a transport stream names a track, and a cut puts each stream
/// back on the PID it arrived on. Two streams cannot share one: ask the
/// muxer for that and it says so and stops -- `Duplicate stream id 4352`.
///
/// They do share one on a Blu-ray. Its lossless sound is a TrueHD track with
/// an AC-3 core folded into the same PES for a player that cannot decode the
/// rest, and libavformat hands the two halves over as separate streams
/// carrying the same PID.
///
/// The first is the stream the programme map named; anything after it on the
/// same PID is a piece the demuxer split out of it. So the first is kept --
/// which for a Blu-ray is the TrueHD, and a TrueHD elementary stream on its
/// own is a track a player decodes -- and the rest are named as left behind,
/// because what goes is real: the fallback that was folded inside it.
fn one_track_per_pid(
    audios: Vec<AudioInfo>,
    on_a_ts: bool,
) -> (Vec<AudioInfo>, Vec<DroppedStream>) {
    // Only a transport stream names its streams by PID. Every other container
    // numbers them however it likes, and more than one of them numbers them
    // all the same -- which would fold a whole recording's sound into its
    // first track.
    if !on_a_ts {
        return (audios, Vec::new());
    }
    let mut kept: Vec<AudioInfo> = Vec::with_capacity(audios.len());
    let mut folded = Vec::new();
    for a in audios {
        if kept.iter().any(|k| k.pid == a.pid) {
            folded.push(DroppedStream { pid: a.pid, what: "substream" });
        } else {
            kept.push(a);
        }
    }
    (kept, folded)
}

/// Probe the source and build its access-point index with the given strategy.
pub fn scan_with(path: &str, source: &dyn index::IndexSource) -> Result<Source> {
    init()?;
    let input = input::Input::parse(path)?;
    let ictx =
        ff::format::input(&input.url).map_err(|e| anyhow!("cannot open {path}: {e}"))?;
    // Read before the demuxer is handed to the index source, which takes it.
    let byte_seekable = ictx
        .format()
        .name()
        .split(',')
        .any(|n| BYTE_SEEKABLE.contains(&n.trim()));
    // Whether a stream's id is a PID. See [`one_track_per_pid`].
    let on_a_ts = ictx.format().name().split(',').any(|n| n.trim() == "mpegts");

    let stream = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| anyhow!("no video stream in {path}"))?;
    let stream_index = stream.index();
    let time_base = f64::from(stream.time_base());
    let params = stream.parameters();
    let codec = format!("{:?}", params.id()).to_lowercase();
    let (width, height, has_b_frames, field_order, sample_aspect_ratio, extradata) = unsafe {
        let p = params.as_ptr();
        let extra = if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec()
        };
        let sar = (*p).sample_aspect_ratio;
        (
            (*p).width as u32,
            (*p).height as u32,
            (*p).video_delay,
            (*p).field_order as i32,
            if sar.num > 0 && sar.den > 0 { sar.num as f64 / sar.den as f64 } else { 1.0 },
            extra,
        )
    };
    let framing = bitstream::framing_from_extradata(&codec, &extradata);
    let frame_rate = f64::from(stream.avg_frame_rate());

    let read_audio = |a: &ff::format::stream::Stream| {
        let p = a.parameters();
        let (sample_rate, channels, bit_rate) = unsafe {
            let raw = p.as_ptr();
            (
                (*raw).sample_rate as u32,
                (*raw).ch_layout.nb_channels as u16,
                if (*raw).bit_rate > 0 { Some((*raw).bit_rate as usize) } else { None },
            )
        };
        AudioInfo {
            stream_index: a.index(),
            pid: a.id(),
            language: a.metadata().get("language").map(str::to_string),
            codec: format!("{:?}", p.id()).to_lowercase(),
            sample_rate,
            channels,
            bits: audio::pcm_bits(&p),
            time_base: f64::from(a.time_base()),
            bit_rate,
        }
    };
    let audios: Vec<AudioInfo> = ictx
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| read_audio(&s))
        // A track the container never managed to describe is not a track
        // this program can carry. It happens on a broadcast recording whose
        // service changes its map part-way through: the map names a second
        // set of PIDs, the probe at the head of the file finds no frame on
        // them, and what comes back is a stream with no sample rate and no
        // channels. Nothing can be done with one -- the muxer refuses to
        // declare it ("sample rate not set"), and there is no rate to
        // re-encode it at either -- so it is left out here rather than
        // carried as far as the write and failing the whole cut there.
        .filter(|a| {
            let described = a.sample_rate > 0 && a.channels > 0;
            if !described {
                eprintln!(
                    "note: the sound on pid 0x{:04x} is named by this recording's map but \
                     never appears in it, so there is nothing to describe it with. It is \
                     left out; the rest of the recording is unaffected.",
                    a.pid,
                );
            }
            described
        })
        .collect();
    let (audios, folded) = one_track_per_pid(audios, on_a_ts);
    // Which of them is the main sound. libav's own answer, since it weighs
    // the disposition flags a container may carry; the first track when it
    // has no opinion, which is what a broadcast recording amounts to.
    let audio = ictx
        .streams()
        .best(ff::media::Type::Audio)
        .map(|a| a.index())
        .and_then(|i| audios.iter().find(|a| a.stream_index == i).cloned())
        .or_else(|| audios.first().cloned());

    // Captions are subtitles here only in the sense libav means it: an ARIB
    // caption stream is not decoded, it is carried. Any other subtitle codec
    // in a broadcast recording is not one of these and is left alone.
    let captions: Vec<CaptionInfo> = ictx
        .streams()
        .filter(|s| s.parameters().id() == ff::codec::Id::ARIB_CAPTION)
        .map(|s| CaptionInfo {
            stream_index: s.index(),
            pid: s.id(),
            language: s.metadata().get("language").map(str::to_string),
            time_base: f64::from(s.time_base()),
        })
        .collect();

    // What is being left behind, so it can be said out loud. See
    // [`DroppedStream`].
    let mut dropped: Vec<DroppedStream> = ictx
        .streams()
        .filter_map(|s| {
            let p = s.parameters();
            match (p.medium(), p.id()) {
                // Not dropped at all: this is the event information table,
                // and where it belongs is not in a stream. See [`si`], which
                // puts it back on the PID a broadcast keeps it on.
                (_, ff::codec::Id::EPG) => None,
                (ff::media::Type::Data, ff::codec::Id::BIN_DATA) => {
                    Some(DroppedStream { pid: s.id(), what: "superimpose" })
                }
                (ff::media::Type::Unknown, _) | (ff::media::Type::Data, _) => {
                    Some(DroppedStream { pid: s.id(), what: "data" })
                }
                _ => None,
            }
        })
        .collect();
    dropped.extend(folded);

    let (duration, start_time) = unsafe {
        let p = ictx.as_ptr();
        let tb = ff::ffi::AV_TIME_BASE as f64;
        let d = (*p).duration;
        let s = (*p).start_time;
        (
            if d == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { d as f64 / tb },
            if s == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { s as f64 / tb },
        )
    };

    let video = VideoInfo {
        stream_index,
        codec: codec.clone(),
        width,
        height,
        frame_rate,
        has_b_frames,
        time_base,
        sample_aspect_ratio,
        framing,
        field_order,
        pulldown: false, // the index source reports this, when it can
    };

    let idx = source.build(index::IndexInput {
        path,
        video: &video,
        start_time,
        ictx,
    })?;
    let mut points = idx.points;
    if points.is_empty() {
        return Err(anyhow!("no random access points found in {path}"));
    }
    // Times are rebased by the container's start time, and the first picture
    // can land a fraction of a microsecond below zero when the two disagree
    // in the last bit. Nothing can be seeked to before the file begins, and
    // a range clamped to a negative entry point simply fails, so the floor
    // goes in here rather than at every call site.
    for p in points.iter_mut() {
        p.time = p.time.max(0.0);
        p.lead_start = p.lead_start.max(0.0);
    }

    let gaps: Vec<f64> = points.windows(2).map(|w| w[1].time - w[0].time).collect();
    let mean_gop =
        if gaps.is_empty() { 1.0 } else { gaps.iter().sum::<f64>() / gaps.len() as f64 };
    let seek_margin = (3.0 * mean_gop).clamp(1.0, 30.0);

    let mut video = video;
    video.pulldown = idx.pulldown.unwrap_or(false);

    Ok(Source {
        path: path.to_string(),
        input,
        audio,
        audios,
        captions,
        dropped,
        seek_margin,
        byte_seekable,
        video,
        duration,
        start_time,
        points,
        leading_known: idx.leading_known,
        index_name: source.name(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(stream_index: usize, pid: i32, codec: &str, channels: u16) -> AudioInfo {
        AudioInfo {
            stream_index,
            pid,
            language: None,
            codec: codec.to_string(),
            sample_rate: 48_000,
            channels,
            bits: 16,
            time_base: 1.0 / 90_000.0,
            bit_rate: None,
        }
    }

    #[test]
    fn a_blu_rays_lossless_sound_is_one_track_and_not_two() {
        // What libavformat hands over for a disc with a 5.1 and a stereo
        // TrueHD track: each is the MLP itself and the AC-3 core folded
        // inside it, split into two streams carrying the one PID.
        let handed = vec![
            sound(1, 0x1100, "truehd", 6),
            sound(2, 0x1100, "ac3", 6),
            sound(3, 0x1101, "truehd", 2),
            sound(4, 0x1101, "ac3", 2),
        ];
        let (kept, folded) = one_track_per_pid(handed.clone(), true);
        // The track the programme map named, which is the one the disc
        // advertises and the one worth keeping.
        assert_eq!(kept.iter().map(|a| a.stream_index).collect::<Vec<_>>(), [1, 3]);
        assert!(kept.iter().all(|a| a.codec == "truehd"));
        // And what went, said out loud rather than dropped in silence.
        assert_eq!(folded.iter().map(|d| d.pid).collect::<Vec<_>>(), [0x1100, 0x1101]);
        assert!(folded.iter().all(|d| d.what == "substream"));

        // A broadcast's two sound tracks are two PIDs and stay two tracks.
        let bilingual = vec![sound(1, 0x0110, "aac", 2), sound(2, 0x0111, "aac", 2)];
        let (kept, folded) = one_track_per_pid(bilingual.clone(), true);
        assert_eq!(kept.len(), 2);
        assert!(folded.is_empty());

        // Off a transport stream a stream's id is not a PID, and more than
        // one container numbers every stream the same. Folding there would
        // throw away a recording's sound.
        let (kept, folded) = one_track_per_pid(handed, false);
        assert_eq!(kept.len(), 4);
        assert!(folded.is_empty());
    }
}
