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
pub mod bdav;
pub mod bitstream;
pub mod caption;
pub mod cm;
pub mod cut;
pub mod disc;
pub mod dvd;
pub mod index;
pub mod input;
pub mod logo;
pub mod netpath;
pub mod pgs;
pub mod plan;
pub mod playback_audio;
pub mod preview;
pub mod proxy;
pub mod seek_index;
pub mod si;
pub mod thumbs;
pub mod udf;
pub mod udfw;
pub mod vobsub;

pub use adts::{AacVersion, AdtsFormat};
pub use cm::{
    blocks as cm_blocks, blocks_from_logo as cm_blocks_from_logo,
    blocks_from_resets as cm_blocks_from_resets, candidates as cm_candidates, find_silences,
    find_silences_with, refine_boundaries as cm_refine_boundaries, DetectOptions,
};
pub use cut::{
    cut, cut_with_progress, writable_sound, write_audio_es, AudioCodec, AudioMode, CutOptions,
    SoundAsIs, SoundChoices,
};
pub use index::{ContainerIndex, DiscIndex, IndexSource, PacketScan};
pub use plan::{plan, plan_on, plan_range, PlanOptions, RangePlan, Segment, SegmentKind};
pub use playback_audio::play_audio;
pub use preview::{
    frame_at, glance, glance_at, glance_run, glance_sweep, play_from, shot_at, shots_at, Pace, Shot,
};
pub use proxy::{Marks, ProxyOptions};
pub use seek_index::SeekIndex;
pub use thumbs::{ThumbOptions, Track};

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
    /// Bits per second the pictures actually take, counted while the index
    /// was built.
    ///
    /// This is what a re-encoded stretch is written at, so that the pictures
    /// spliced in are worth about what the ones around them are worth. The
    /// frame size alone cannot say: the same 1920x1080 is 17 Mbit/s off the
    /// air and 27 off a Blu-ray, and a figure derived from the size gave the
    /// Blu-ray a sixth of what it came in at.
    ///
    /// `None` where the index came from somewhere that never read the
    /// pictures -- a container's own seek table -- and then there is nothing
    /// to do but derive one from the frame size after all.
    pub bit_rate: Option<f64>,
    /// The sequence and entry-point headers a VC-1 stream declares itself
    /// with, when it is one.
    ///
    /// Every other codec here can be read a packet at a time. VC-1 cannot:
    /// the shape of a picture header depends on flags that appear only in
    /// the sequence header, which a transport stream restates in front of
    /// every entry point and libavformat hands over as extradata. They are
    /// also what an encoded picture has to be introduced by, so that the
    /// copied pictures across the splice are decoded against the parameters
    /// they were coded with. See [`smartcut_vc1`].
    pub vc1: Option<smartcut_vc1::Shape>,
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

/// A graphics stream: the subtitles a disc draws rather than writes.
///
/// Blu-ray's own subtitles are pictures. What arrives is not a statement but
/// a *display set* -- a composition, a window, a palette, a run-length coded
/// image -- spread over several packets that mean nothing apart from one
/// another. That grouping is the only thing that makes this different from
/// [`CaptionInfo`], and [`crate::pgs`] is where it is dealt with.
#[derive(Debug, Clone)]
pub struct GraphicsInfo {
    pub stream_index: usize,
    pub pid: i32,
    pub language: Option<String>,
    pub time_base: f64,
}

/// A subpicture stream: the subtitles a DVD draws.
///
/// The same kind of thing as [`GraphicsInfo`] and it cannot travel the same
/// way: a transport stream has a number for a Blu-ray's graphics and none at
/// all for a DVD's, so these are written beside the cut instead of inside
/// it. See [`crate::vobsub`].
///
/// Named by the substream id the disc gives it -- 0x20 to 0x3F -- because
/// that is the one name everything agrees on. A DVD's subtitles share one
/// stream with the sound and are told apart by the byte in front of each
/// packet, so what libavformat calls a stream index here is a thing it makes
/// up when it first meets one, and it may not meet one for the first
/// gigabyte. The disc's own index has known all along.
#[derive(Debug, Clone)]
pub struct SubpictureInfo {
    pub id: i32,
    pub language: Option<String>,
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
    /// Its index among the container's streams, which is how it is named
    /// where the `pid` beside it is not a PID. See [`track_name`].
    pub stream_index: usize,
    /// Which of them it is: `"superimpose"`, `"data"`, `"menu"` or
    /// `"text subtitles"`. A name rather than a sentence, because the window
    /// says this in the language it is set to and the command line says it
    /// in English.
    pub what: &'static str,
}

impl DroppedStream {
    /// What to call it in English, which is what the command line prints.
    pub fn describe(&self) -> &'static str {
        match self.what {
            "superimpose" => "superimposed text",
            "substream" => "a compatibility stream folded into the track written",
            // The two a disc carries that no cut can take with it. See
            // [`disc::correct_from_index`].
            "menu" => "a menu",
            "text subtitles" => "text subtitles, whose typeface is on the disc",
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
    /// Every graphics stream: a disc's own subtitles, one per language.
    pub graphics: Vec<GraphicsInfo>,
    /// Every subpicture stream, which is what a DVD calls the same thing.
    pub subpictures: Vec<SubpictureInfo>,
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
    /// Whether a track's `pid` is really a PID.
    ///
    /// A transport stream names its tracks that way and a cut puts them back
    /// on the same numbers, so a PID is what a person reading about one
    /// should be told. **Nothing else has PIDs.** An MP4 numbers its tracks
    /// and libavformat hands that number over in the same field, so a note
    /// about "the sound on pid 0x0102" of an MP4 is a note using a word for
    /// something that is not there. See [`track_name`].
    pub on_a_ts: bool,
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

/// Start libav, and stop it talking over the top of this program.
///
/// **What libav prints is not addressed to anyone here.** It is a running
/// commentary from inside the decoders and muxers -- `co located POCs
/// unavailable` from a decoder handed a GOP to re-encode, `Could not find
/// codec parameters` for a subtitle stream a cut was never going to carry,
/// `The encoder 'truehd' is experimental` from a probe that exists only to
/// find that out, printed directly beside this program's own note saying the
/// track is being carried through untouched instead. None of it is a fault,
/// every line of it looks like one, and the program has its own words for
/// everything it actually needs to say.
///
/// So the default is silence, and `SMARTCUT_FFMPEG_LOG` brings it back:
/// `1` for the warnings, `2` for everything libav has to say. Nothing is
/// lost by the default -- a call that fails returns its error, which is
/// carried and reported here rather than printed there.
/// How to name one track where a person will read it.
///
/// `pid 0x1100` off a transport stream, `stream 1` out of anything else. The
/// number in [`AudioInfo::pid`] is whatever the container calls the track, and
/// only one kind of container calls it a PID. See [`Source::on_a_ts`].
pub fn track_name(on_a_ts: bool, pid: i32, stream_index: usize) -> String {
    if on_a_ts {
        format!("pid 0x{pid:04x}")
    } else {
        format!("stream {stream_index}")
    }
}

pub fn init() -> Result<()> {
    ff::init().map_err(|e| anyhow!("ffmpeg init failed: {e}"))?;
    let level = match std::env::var("SMARTCUT_FFMPEG_LOG").as_deref() {
        Ok("2") | Ok("all") => ff::util::log::Level::Verbose,
        Ok("1") | Ok("on") | Ok("yes") => ff::util::log::Level::Warning,
        _ => ff::util::log::Level::Quiet,
    };
    ff::util::log::set_level(level);
    Ok(())
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
            folded.push(DroppedStream {
                pid: a.pid,
                stream_index: a.stream_index,
                what: "substream",
            });
        } else {
            kept.push(a);
        }
    }
    (kept, folded)
}

/// Probe the source and build its access-point index with the given strategy.
pub fn scan_with(path: &str, source: &dyn index::IndexSource) -> Result<Source> {
    scan_reporting(path, source, None)
}

/// As [`scan_with`], saying how far through the read is as it goes.
///
/// Worth having only for [`index::PacketScan`], and worth having *there*
/// because the walk is the whole of the wait before anything else can begin:
/// around a second a gigabyte from a local disc and as long again over a
/// share. Whoever asked for the scan is otherwise looking at a bar that does
/// not move until the pass after this one starts reporting.
pub fn scan_reporting(
    path: &str,
    source: &dyn index::IndexSource,
    on: Option<index::OnProgress>,
) -> Result<Source> {
    let (outline, ictx) = outline_of(path)?;
    let idx = source.build(index::IndexInput {
        path,
        video: &outline.video,
        start_time: outline.start_time,
        ictx,
        on,
    })?;
    assemble(path, outline, idx, source.name())
}

/// Read a recording once and come back with both its index and its pictures.
///
/// Otherwise these are two passes over the same file: [`scan`] walks every
/// packet and decodes none of them, and [`thumbs::build`] walks every packet
/// again to decode the key ones. The second read is nearly free where the
/// machine still holds the file in its page cache -- measured at a tenth of
/// the pair on a local disc -- and is a second transfer where it does not,
/// which a recording on a share always is. There the pair costs twice what
/// this does.
///
/// What is given up is when the index arrives. Two passes can hand it over
/// after the first of them, at disc speed; this has it only when the read is
/// over, which is decoder speed -- around four seconds a gigabyte against
/// one. So it is worth choosing between the two rather than doing one of them
/// always, and the thing to choose on is where the recording is.
///
/// `on` is told how far through the read it is; `stop` is asked the same
/// question the passes are asked, and gives up where it stands.
pub fn scan_with_pictures(
    path: &str,
    opts: &thumbs::ThumbOptions,
    on: Option<index::OnProgress>,
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<(Source, thumbs::Track)> {
    let (outline, ictx) = outline_of(path)?;
    let params = ictx
        .stream(outline.video.stream_index)
        .ok_or_else(|| anyhow!("video stream vanished"))?
        .parameters();
    let mut decoder = video_decoder_with(params, opts.threads)?;
    // The container's length is all there is to go on here, and on a program
    // stream it can be wildly wrong. The collector corrects itself as the
    // read goes on, and the real length goes in at the end; see
    // [`thumbs::Collector::about`].
    let mut collector =
        thumbs::Collector::about(outline.duration, outline.video.sample_aspect_ratio, opts);
    let mut frame = ff::frame::Video::empty();
    let time_base = outline.video.time_base;
    let start_time = outline.start_time;

    let idx = {
        // Non-key packets never reach the decoder: it would still have to
        // parse them to throw them away. Skipping is done here rather than
        // with `skip_frame`, which would be wrong -- see the note in
        // [`thumbs::build_with`], which learned it the hard way on a field
        // coded entry point.
        let mut take = |packet: &ff::Packet| -> Result<()> {
            if decoder.send_packet(packet).is_err() {
                return Ok(());
            }
            while decoder.receive_frame(&mut frame).is_ok() {
                if let Some(pts) = frame.pts() {
                    collector.feed(pts as f64 * time_base - start_time, &frame)?;
                }
            }
            Ok(())
        };
        index::walk(&outline.video, start_time, ictx, on, &mut take, stop)?
    };
    // The decoder holds a picture back for reordering, so the last entry
    // point only comes out on a flush. Without it the strip's final cell has
    // a hole in it.
    let _ = decoder.send_eof();
    while decoder.receive_frame(&mut frame).is_ok() {
        if let Some(pts) = frame.pts() {
            collector.feed(pts as f64 * time_base - start_time, &frame)?;
        }
    }

    // The real length, which only the read knows, before the scenes are
    // spaced against it.
    let duration = match idx.end {
        Some(end) if end > outline.duration => end,
        _ => outline.duration,
    };
    let track = collector.finish(duration);
    let src = assemble(path, outline, idx, "packet scan")?;
    Ok((src, track))
}

/// Put a [`Source`] together out of the container's answer and an index.
fn assemble(
    path: &str,
    outline: Outline,
    idx: index::Index,
    index_name: &'static str,
) -> Result<Source> {
    let Outline {
        path: _,
        input,
        mut video,
        audio,
        audios,
        captions,
        graphics,
        subpictures,
        dropped,
        duration,
        start_time,
        byte_seekable,
        on_a_ts,
    } = outline;
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
    let mean_gop = if gaps.is_empty() {
        1.0
    } else {
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    let seek_margin = (3.0 * mean_gop).clamp(1.0, 30.0);

    video.pulldown = idx.pulldown.unwrap_or(false);
    video.bit_rate = idx.bit_rate;

    // A container that says it is shorter than the pictures it holds is a
    // program stream; see [`index::Index::end`]. Only ever longer, so that a
    // container which knows its own length keeps it: trailing sound after the
    // last picture is part of a recording, and the pictures do not bound it.
    let duration = match idx.end {
        Some(end) if end > duration => end,
        _ => duration,
    };

    let mut src = Source {
        path: path.to_string(),
        input,
        audio,
        audios,
        captions,
        graphics,
        subpictures,
        dropped,
        seek_margin,
        byte_seekable,
        on_a_ts,
        video,
        duration,
        start_time,
        points,
        leading_known: idx.leading_known,
        index_name,
    };
    // A recording on a disc has its languages written beside it and nowhere
    // in it. Done here, once, so that a cut carries them however the
    // recording was reached -- named on the command line, chosen from the
    // list, or opened again out of a project.
    disc::carry_disc_languages(&mut src);
    Ok(src)
}

/// What the container itself says about a recording, before a byte of it has
/// been walked.
///
/// All of this comes out of libavformat's own probe -- a few megabytes off
/// the head of the file -- so it costs tens of milliseconds whether the
/// recording is a hundred megabytes or four gigabytes. What it cannot say is
/// where the access points are: that is [`scan`], and that is a pass over
/// every byte at around a second a gigabyte.
///
/// Which makes this the answer a list can put on a row the instant a file is
/// dropped on it -- how long, how big, how fast, what it is written in, what
/// sound it carries -- with the walk filling in the rest behind it.
pub struct Outline {
    pub path: String,
    pub input: input::Input,
    pub video: VideoInfo,
    pub audio: Option<AudioInfo>,
    pub audios: Vec<AudioInfo>,
    pub captions: Vec<CaptionInfo>,
    pub graphics: Vec<GraphicsInfo>,
    pub subpictures: Vec<SubpictureInfo>,
    pub dropped: Vec<DroppedStream>,
    /// The container's own length, which is usually right and on a program
    /// stream can be wildly wrong -- a DVD's four gigabytes have been seen
    /// to come back as eight seconds. Only the walk can correct that; see
    /// [`index::Index::end`].
    pub duration: f64,
    /// Container start time. MPEG-TS does not begin at zero.
    pub start_time: f64,
    pub byte_seekable: bool,
    /// Whether a track's `pid` is really a PID. See [`Source::on_a_ts`].
    pub on_a_ts: bool,
}

impl Outline {
    /// The container's answer wearing a [`Source`]'s shape, with no index in
    /// it.
    ///
    /// For the passes that *read* a recording rather than seek about in it --
    /// the captions, the audio, the logo -- every one of which takes a
    /// `Source` and none of which so much as looks at `points`. That is what
    /// lets a commercial detection start on a recording the walk has not
    /// finished: those three passes are minutes long, and the walk that would
    /// have gated them is seconds.
    ///
    /// **Not a `Source` to plan, cut or take an exact picture from.** Anything
    /// that seeks by access point finds none here and falls back to reading
    /// from the beginning of the file, which is not wrong but is not
    /// affordable either. Those want [`scan`]. `points` being empty is the
    /// test, and the passes that cannot do without them make it --
    /// [`cm::refine_boundaries`] is the one that has to.
    pub fn into_source(self) -> Source {
        Source {
            path: self.path,
            input: self.input,
            video: self.video,
            audio: self.audio,
            audios: self.audios,
            captions: self.captions,
            graphics: self.graphics,
            subpictures: self.subpictures,
            dropped: self.dropped,
            duration: self.duration,
            start_time: self.start_time,
            byte_seekable: self.byte_seekable,
            on_a_ts: self.on_a_ts,
            points: Vec::new(),
            // Nothing was measured, so nothing is known: a caller that cares
            // about leading pictures must refine before it believes any.
            leading_known: false,
            index_name: "no index",
            // The floor `scan` would clamp to. There are no gaps to take a
            // mean of, and this is only ever used as a "read from a little
            // earlier" margin.
            seek_margin: 1.0,
        }
    }
}

/// The container's own answer about a recording. See [`Outline`].
pub fn outline(path: &str) -> Result<Outline> {
    outline_of(path).map(|(o, _)| o)
}

/// As [`outline`], handing back the demuxer it opened along with it.
///
/// [`scan_reporting`] wants both: the answer, and the open file to walk. An
/// index source takes the demuxer, so it has to come out of here rather than
/// be opened a second time.
fn outline_of(path: &str) -> Result<(Outline, ff::format::context::Input)> {
    init()?;
    let input = input::Input::parse(path)?;
    let ictx = crate::input::demux(&input.url).map_err(|e| anyhow!("cannot open {path}: {e}"))?;
    // Read before the demuxer is handed to the index source, which takes it.
    let byte_seekable = ictx
        .format()
        .name()
        .split(',')
        .any(|n| BYTE_SEEKABLE.contains(&n.trim()));
    // Whether a stream's id is a PID. See [`one_track_per_pid`].
    let on_a_ts = ictx
        .format()
        .name()
        .split(',')
        .any(|n| n.trim() == "mpegts");

    // What the disc's index says this clip carries that a demuxer cannot
    // name. Read before anything the probe said is believed, because for
    // these two the probe is guessing: see [`disc::unreadable_streams`].
    let unreadable = disc::unreadable_streams(path);
    let indexed = |pid: i32| unreadable.iter().any(|(on, _)| *on == pid);

    let stream = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| {
            // A disc's menu clips are files of exactly this shape: graphics
            // and nothing else, no pictures at all. Worth saying, because
            // "no video stream" reads like a fault in a file that is doing
            // precisely what it was written to do.
            if unreadable.iter().any(|(_, what)| *what == "menu") {
                anyhow!("{path} is one of the disc's menus: it carries a menu's graphics and no pictures, so there is nothing here to cut")
            } else {
                anyhow!("no video stream in {path}")
            }
        })?;
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
            if sar.num > 0 && sar.den > 0 {
                sar.num as f64 / sar.den as f64
            } else {
                1.0
            },
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
                if (*raw).bit_rate > 0 {
                    Some((*raw).bit_rate as usize)
                } else {
                    None
                },
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
        .filter(|s| !indexed(s.id()))
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
                    "note: the sound on {} is named by this recording's map but never \
                     appears in it, so there is nothing to describe it with. It is left \
                     out; the rest of the recording is unaffected.",
                    track_name(on_a_ts, a.pid, a.stream_index),
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
    // caption stream is not decoded, it is carried. A disc's subtitles are
    // the other kind and are gathered separately, just below.
    let captions: Vec<CaptionInfo> = ictx
        .streams()
        .filter(|s| s.parameters().id() == ff::codec::Id::ARIB_CAPTION)
        .filter(|s| !indexed(s.id()))
        .map(|s| CaptionInfo {
            stream_index: s.index(),
            pid: s.id(),
            language: s.metadata().get("language").map(str::to_string),
            time_base: f64::from(s.time_base()),
        })
        .collect();

    // The disc's own subtitles, which are drawn rather than written. Kept
    // apart from the captions above for the one reason [`GraphicsInfo`]
    // gives: their packets come in groups, and a group is what a cut has to
    // move. Menus (IGS) and the text format are not here -- a menu is a
    // thing to press rather than a thing to show, and nothing has ever been
    // seen to carry the text one.
    let graphics: Vec<GraphicsInfo> = ictx
        .streams()
        .filter(|s| s.parameters().id() == ff::codec::Id::HDMV_PGS_SUBTITLE)
        .filter(|s| !indexed(s.id()))
        .map(|s| GraphicsInfo {
            stream_index: s.index(),
            pid: s.id(),
            language: s.metadata().get("language").map(str::to_string),
            time_base: f64::from(s.time_base()),
        })
        .collect();

    // A DVD's subtitles, which are not asked of the container at all: the
    // disc's own index names them, and it names them before libavformat has
    // met one. See [`SubpictureInfo`].
    let subpictures: Vec<SubpictureInfo> = dvd::subtitles_of(path)
        .map(|s| {
            s.streams
                .into_iter()
                .map(|(id, language)| SubpictureInfo {
                    id: id as i32,
                    language,
                })
                .collect()
        })
        .unwrap_or_default();

    // What is being left behind, so it can be said out loud. See
    // [`DroppedStream`].
    let mut dropped: Vec<DroppedStream> = ictx
        .streams()
        .filter_map(|s| {
            let p = s.parameters();
            // Named by the disc rather than by the probe, above.
            if let Some((pid, what)) = unreadable.iter().find(|(on, _)| *on == s.id()) {
                return Some(DroppedStream {
                    pid: *pid,
                    stream_index: s.index(),
                    what,
                });
            }
            match (p.medium(), p.id()) {
                // Not dropped at all: this is the event information table,
                // and where it belongs is not in a stream. See [`si`], which
                // puts it back on the PID a broadcast keeps it on.
                (_, ff::codec::Id::EPG) => None,
                (ff::media::Type::Data, ff::codec::Id::BIN_DATA) => Some(DroppedStream {
                    pid: s.id(),
                    stream_index: s.index(),
                    what: "superimpose",
                }),
                (ff::media::Type::Unknown, _) | (ff::media::Type::Data, _) => Some(DroppedStream {
                    pid: s.id(),
                    stream_index: s.index(),
                    what: "data",
                }),
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
            if d == ff::ffi::AV_NOPTS_VALUE {
                0.0
            } else {
                d as f64 / tb
            },
            if s == ff::ffi::AV_NOPTS_VALUE {
                0.0
            } else {
                s as f64 / tb
            },
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
        bit_rate: None,  // and this, when it read the pictures to find out
        // Read once, here, rather than hunted for in every packet: a
        // transport stream restates these in front of each entry point, so
        // libavformat has them before a packet has been asked for.
        vc1: matches!(codec.as_str(), "vc1" | "wmv3")
            .then(|| smartcut_vc1::Shape::read(&extradata))
            .flatten(),
    };

    let outline = Outline {
        path: path.to_string(),
        input,
        video,
        audio,
        audios,
        captions,
        graphics,
        subpictures,
        dropped,
        duration,
        start_time,
        byte_seekable,
        on_a_ts,
    };
    Ok((outline, ictx))
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
        assert_eq!(
            kept.iter().map(|a| a.stream_index).collect::<Vec<_>>(),
            [1, 3]
        );
        assert!(kept.iter().all(|a| a.codec == "truehd"));
        // And what went, said out loud rather than dropped in silence.
        assert_eq!(
            folded.iter().map(|d| d.pid).collect::<Vec<_>>(),
            [0x1100, 0x1101]
        );
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
