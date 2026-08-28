//! Smart-rendering cut engine.
//!
//! Port of the Python reference implementation in `../../../smartcut`, moved
//! onto libav directly so that packets -- and their timestamps -- are ours to
//! place. The prototype drove the ffmpeg CLI and paid for it: an elementary
//! stream carries no timestamps, so ffmpeg had to synthesise them and could
//! not reorder the first few packets, leaving the opening frame 13 ms early.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

pub mod audio;
pub mod bitstream;
pub mod caption;
pub mod cm;
pub mod cut;
pub mod index;
pub mod logo;
pub mod plan;
pub mod playback_audio;
pub mod preview;
pub mod proxy;
pub mod thumbs;

pub use cm::{
    blocks as cm_blocks, blocks_from_logo as cm_blocks_from_logo,
    blocks_from_resets as cm_blocks_from_resets, candidates as cm_candidates,
    find_silences, find_silences_with, refine_boundaries as cm_refine_boundaries,
    DetectOptions,
};
pub use cut::{cut, cut_with_progress, write_audio_es, AudioMode, CutOptions};
pub use index::{ContainerIndex, IndexSource, PacketScan};
pub use preview::{frame_at, play_from, shot_at, shots_at, Pace, Shot};
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
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub time_base: f64,
    pub bit_rate: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub path: String,
    pub video: VideoInfo,
    pub audio: Option<AudioInfo>,
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
    pub seek_margin: f64,
}

pub fn init() -> Result<()> {
    ff::init().map_err(|e| anyhow!("ffmpeg init failed: {e}"))
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
    let mut ctx = ff::codec::context::Context::from_parameters(params)?;
    // ffmpeg-next's `set_threading` writes the kind as well as the count, and
    // naming one kind is exactly how a codec that only has the other ends up
    // single threaded anyway. The count is the one field that needs saying:
    // zero means "as many as this machine has".
    unsafe {
        (*ctx.as_mut_ptr()).thread_count = 0;
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

/// Probe the source and build its access-point index with the given strategy.
pub fn scan_with(path: &str, source: &dyn index::IndexSource) -> Result<Source> {
    init()?;
    let ictx = ff::format::input(&path).map_err(|e| anyhow!("cannot open {path}: {e}"))?;

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

    let audio = ictx.streams().best(ff::media::Type::Audio).map(|a| {
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
            codec: format!("{:?}", p.id()).to_lowercase(),
            sample_rate,
            channels,
            time_base: f64::from(a.time_base()),
            bit_rate,
        }
    });

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
        audio,
        seek_margin,
        video,
        duration,
        start_time,
        points,
        leading_known: idx.leading_known,
        index_name: source.name(),
    })
}
