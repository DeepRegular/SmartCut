//! Smart-rendering cut engine.
//!
//! Port of the Python reference implementation in `../../../smartcut`, moved
//! onto libav directly so that packets -- and their timestamps -- are ours to
//! place. The prototype drove the ffmpeg CLI and paid for it: an elementary
//! stream carries no timestamps, so ffmpeg had to synthesise them and could
//! not reorder the first few packets, leaving the opening frame 13 ms early.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

pub mod aac;
pub mod adts;
pub mod arib;
pub mod audio;
pub mod bdav;
pub mod bitstream;
pub mod blank;
pub mod carousel;
pub mod carry;
pub mod caption;
pub mod cm;
pub mod cut;
pub mod disc;
pub mod dvd;
pub mod entrypool;
pub mod fit;
pub mod index;
pub mod input;
pub mod latm;
pub mod logo;
pub mod netpath;
pub mod nice;
pub mod pgs;
pub mod plan;
pub mod playback_audio;
pub mod preview;
pub mod proxy;
pub mod restamp;
pub mod seek_index;
pub mod series;
pub mod si;
pub mod subs;
pub mod text;
pub mod thumbs;
pub mod ttml;
pub mod udf;
pub mod udfw;
pub mod vobsub;

pub use aac::Framing;
pub use adts::{AacVersion, AdtsFormat};
pub use latm::LatmFormat;
pub use blank::{
    find_runs as find_blank_runs, find_runs_with as find_blank_runs_with, BlankOptions,
    Run as BlankRun, Shade,
};
pub use cm::{
    blocks as cm_blocks, blocks_from_logo as cm_blocks_from_logo,
    blocks_from_resets as cm_blocks_from_resets, candidates as cm_candidates, find_silences,
    find_silences_with, marks_every_junction as cm_marks_every_junction,
    refine_boundaries as cm_refine_boundaries, DetectOptions,
};
pub use cut::{
    can_carry_data_broadcast, cut, cut_with_progress, tables_for, writable_sound, write_audio_es,
    AudioCodec, AudioMode, CutOptions, SoundAsIs, SoundChoices,
};
pub use index::{ContainerIndex, DiscIndex, IndexSource, PacketScan};
pub use plan::{plan, plan_on, plan_range, PlanOptions, RangePlan, Segment, SegmentKind};
pub use playback_audio::{peaks_at, play_audio, Levels, Volume};
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
    /// The rate the container says its pictures are *meant* to come at --
    /// libavformat's `r_frame_rate`, which for an MP4 is the commonest
    /// duration in the sample table.
    ///
    /// Not the same number as [`frame_rate`], which is the average over the
    /// whole recording. On anything constant they agree; on an interlaced
    /// Blu-ray this is the *field* rate and so is twice the other; and on a
    /// variable-rate recording it is the rate the material was authored at
    /// while the average is whatever the pictures happened to come to. **The
    /// last of those is the one worth having**: it is the only round number
    /// in sight, and a timeline built on the average of a variable recording
    /// is built on a number nothing in the recording ever meant. See
    /// [`crate::cut`], which builds one.
    ///
    /// 0 where the container would not say.
    ///
    /// [`frame_rate`]: VideoInfo::frame_rate
    pub base_rate: f64,
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
    /// Whether the pictures come at a rate the frame arithmetic can predict.
    ///
    /// **A container states one rate for every stream, whether or not the
    /// stream has one.** A screen capture holds a picture for as long as
    /// nothing changed, and anything that has been through `mpdecimate`
    /// holds one wherever the repeats came out; both still declare a rate,
    /// and it is the fastest the recording ever managed rather than what it
    /// does. Nothing about a cut depends on this -- each picture is placed by
    /// its own timestamp and each segment lasts until the next one starts,
    /// which is right either way -- but a frame *number* is not a coordinate
    /// anybody can act on where it is true, so what is counted in frames says
    /// so instead. See [`index::varies`], and the [pulldown] field above,
    /// which is the same fact about a stream that only varies by a field.
    ///
    /// False where the index never read the pictures and could not tell.
    ///
    /// [pulldown]: VideoInfo::pulldown
    pub variable_rate: bool,
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
    /// pictures and could not be made to read a little of them -- a
    /// container's own seek table -- and then there is nothing to do but
    /// derive one from the frame size after all. A recording read off a
    /// disc's entry-point map is not that case: see
    /// [`index::sample_bit_rate`].
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
    /// What a picture of this recording is read against to say whether it is
    /// a whole frame or one field of a pair.
    ///
    /// `None` for every recording that codes whole frames, which is nearly
    /// all of them: broadcast 1080i is interlaced content coded as frames,
    /// and only a recorder writing its own discs was found to code the two
    /// fields separately. See [`bitstream::is_field_picture`].
    pub field_shape: Option<bitstream::FieldShape>,
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
    /// What the broadcast says this track is: the main sound or a second
    /// one, in what language, under what name. See [`si::SoundTrack`].
    ///
    /// `None` for everything that is not a broadcast recording carrying more
    /// than one track -- a disc, a cut this program made, a recording with a
    /// single track, which is the ordinary case and needs no describing.
    pub said: Option<si::SoundTrack>,
}

/// Which of the two text streams a broadcast sends this is.
///
/// They are the same kind of stream doing different jobs. The captions are
/// the programme's own subtitles and belong to it; the crawl is the station
/// writing across whatever is on air -- an earthquake, a vote count, a
/// missing child -- and belongs to the hour rather than to the programme.
/// Both are carried, and a reader is told which it is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Caption,
    Superimpose,
}

/// And which of the two ways it is written.
///
/// A high definition broadcast writes ARIB STD-B24: characters, where to put
/// them, and what to draw them in, as a byte stream with shifts and escapes
/// in it. A 4K broadcast writes TTML instead -- an XML document per caption,
/// with the words in it and the region they go in stated in pixels. Nothing
/// decodes either one for us; see [`crate::caption`] and [`crate::ttml`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    Arib,
    Ttml,
}

/// Which of the three text streams a stream is, from what the recording's
/// own map says about it and what libav made of it.
///
/// **libav names one of them and lumps the other two together.** The captions
/// of a high definition broadcast come back as `arib_caption`; its crawl and
/// a 4K recording's subtitles both come back as `bin_data`, which is to say
/// as bytes of no stated kind. What tells those apart is the component tag
/// the map carries -- 0x30..0x37 the captions, 0x38..0x3F the crawl -- and
/// whether the map also carries the data component descriptor that says the
/// text is ARIB's. A 4K recording carries neither that descriptor nor ARIB
/// text: its subtitles are TTML documents. See [`crate::ttml`].
///
/// `named` is libav's own answer, which stands in for the map on a recording
/// whose map could not be read -- a clip taken off a disc, or a broadcast
/// that adds its caption stream to the map a minute in.
fn text_stream(
    tag: Option<u8>,
    data_component_id: Option<u16>,
    named: bool,
) -> Option<(TextKind, TextFormat)> {
    let kind = match tag {
        Some(0x30..=0x37) => TextKind::Caption,
        Some(0x38..=0x3F) => TextKind::Superimpose,
        _ if named => TextKind::Caption,
        _ => return None,
    };
    let format = match data_component_id {
        // 0x0008 is ARIB STD-B24, which is what a high definition broadcast
        // writes both of these in.
        Some(0x0008) => TextFormat::Arib,
        Some(_) => TextFormat::Arib,
        None if named => TextFormat::Arib,
        None => TextFormat::Ttml,
    };
    Some((kind, format))
}

/// A caption stream: the subtitles the broadcast itself sends, or the crawl
/// it writes over them.
///
/// Kept as its own thing rather than as one more elementary stream, because
/// it is the one non-audio stream that can be put on a cut timeline. Its
/// packets carry a presentation time each, so they shift with the pictures
/// the way audio frames do -- and unlike audio there is nothing to splice,
/// since a caption statement is whole in one packet.
///
/// A 4K recording's subtitles are the exception to that presentation time,
/// and [`crate::ttml`] is where it is dealt with: the packets carry a
/// counter where the clock should be, and the times the words are shown at
/// are inside the document.
#[derive(Debug, Clone)]
pub struct CaptionInfo {
    pub stream_index: usize,
    pub pid: i32,
    pub language: Option<String>,
    pub time_base: f64,
    pub kind: TextKind,
    pub format: TextFormat,
    /// The moment the times inside a TTML document are counted from, on the
    /// same clock as everything else here -- which is to say where the disc
    /// says this clip begins to present, less where the file begins. Zero
    /// for ARIB text, whose packets carry their own times.
    pub base: f64,
}

impl CaptionInfo {
    /// Whether this is the ARIB text [`crate::caption`] reads. The other two
    /// -- a 4K recording's TTML, and a crawl in either -- are carried, and
    /// only the captions themselves are read for where a break is or turned
    /// into the pictures a disc draws.
    pub fn is_arib_caption(&self) -> bool {
        self.kind == TextKind::Caption && self.format == TextFormat::Arib
    }
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
            // [`disc::unreadable`].
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
    /// The seams: where a recording that is several stretches of one clip
    /// joined together passes from one stretch to the next.
    ///
    /// Empty for every ordinary recording. Where there are some, a copy
    /// cannot be carried across one -- the pictures on the far side reference
    /// pictures the recorder made minutes earlier and did not write down --
    /// so a cut is planned as though the seam were a cut of its own. See
    /// [`plan::plan_on`] and [`restamp`].
    pub joins: Vec<restamp::Seam>,
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

/// Say something about a recording once, however many times it is opened.
///
/// A single run opens the same file more than once -- the outline a list row
/// is filled from, the scan that indexes it, the cut's own look at it -- and
/// a note about what is in the recording is as true the third time as the
/// first and no more use to anybody. Keyed on the line itself, so two tracks
/// with the same thing to say about them still say it twice.
pub(crate) fn note_once(line: String) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SAID: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let fresh = SAID
        .get_or_init(Mutex::default)
        .lock()
        .map_or(true, |mut said| said.insert(line.clone()));
    if fresh {
        eprintln!("{line}");
    }
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
pub fn init() -> Result<()> {
    ff::init().map_err(|e| anyhow!("ffmpeg init failed: {e}"))?;
    let level = match std::env::var("SMARTCUT_FFMPEG_LOG").as_deref() {
        Ok("2") | Ok("all") => 2,
        Ok("1") | Ok("on") | Ok("yes") => 1,
        _ => 0,
    };
    set_ffmpeg_log(level);
    Ok(())
}

/// How much of that to let through, after the fact.
///
/// The same three settings [`init`] reads out of the environment -- 0 for
/// silence, 1 for the warnings, 2 for everything -- as something a window
/// can change while it is running. The level lives inside libav rather than
/// here, so this is the whole of it: nothing has to be told, and the next
/// line printed is the first one the new setting applies to.
pub fn set_ffmpeg_log(level: u8) {
    ff::util::log::set_level(match level {
        0 => ff::util::log::Level::Quiet,
        1 => ff::util::log::Level::Warning,
        _ => ff::util::log::Level::Verbose,
    });
    // SVT-AV1 writes its banner and its settings to the terminal itself
    // rather than through libav, the way x265 does -- so quietening libav
    // does not quieten it, and a cut of an AV1 recording printed a dozen
    // lines about an encoder nobody asked for at every seam. The one thing
    // it reads is this, and it reads it when an encoder is opened, so a
    // window that changes the setting while it is running is obeyed by the
    // next seam. `1` is its errors only, which is what is left when the
    // commentary is taken away. See [`crate::cut::encoders_for`].
    std::env::set_var("SVT_LOG", if level >= 2 { "3" } else { "1" });
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

/// How often a pass says where it has got to.
///
/// A pass over a recording knows after every piece it reads how far along it
/// is. Saying so every time is a message a window has to carry, decode and
/// draw for a bar that has not moved: writing a 20 GB image a megabyte at a
/// time came to twenty thousand of them, all but a hundred of which said
/// what the one before had said. Saying so too rarely is the same fault the
/// other way -- the pass that reads a stream back for its entry points
/// counted packets rather than bytes, and a stream of whole pictures made
/// that five reports in six hundred megabytes, which is a bar that stands
/// still and then jumps.
///
/// So neither is left to what the pass happens to be reading: **a
/// five-hundredth of the whole**. The bar has a hundred steps, so it moves
/// smoothly, and the count is bounded whatever the pass is over.
pub struct Told {
    said: f64,
}

/// How much of a pass has to have gone by before it is worth saying so.
const TOLD_STEP: f64 = 0.002;

impl Default for Told {
    fn default() -> Self {
        Self::new()
    }
}

impl Told {
    /// Nothing said yet, so that the first thing offered is said.
    pub fn new() -> Self {
        Self { said: f64::NEG_INFINITY }
    }

    /// Offer `done`, a fraction of the whole, to whoever is waiting. It goes
    /// no further unless the pass has moved far enough since the last one.
    pub fn at(&mut self, on: Option<&(dyn Fn(f64) + Sync)>, done: f64) {
        let Some(f) = on else { return };
        if done - self.said < TOLD_STEP {
            return;
        }
        self.said = done;
        f(done.clamp(0.0, 1.0));
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

/// What a pass that decodes only entry pictures has to hand its decoder.
///
/// Three passes here read a recording for its entry points alone -- the
/// thumbnail track, the one-read scan, the film strip's own decode -- and all
/// three skip the packets between them unparsed, which is most of the file
/// and nearly all of the saving. The rule they skipped by was the key flag:
/// an entry picture is a key packet and a key packet is an entry picture.
///
/// **A field-coded entry picture is two packets and only the first is
/// marked.** A recorder's own BD-RE codes 1440x1080 as PAFF, and libavcodec's
/// H.264 parser hands each field of a pair over separately (its MPEG-2 parser
/// joins them, which is why this only shows up on the discs). Fed the first
/// half on its own, a decoder holds it back waiting for the other and hands
/// nothing over -- not late, never: 0 pictures out of 3802 entry points on the
/// disc measured here. Every picture in the application was then missing at
/// once -- no film strip, no row picture, no scene marks -- which is what the
/// symptom looked like from outside.
///
/// So the half is sent, nothing is read back, and the packet that follows it
/// -- always its other half -- goes in behind it. Then the picture is whole
/// and the decoder can be drained.
pub struct EntryPictures<'a> {
    video: &'a VideoInfo,
    /// A half has gone in and the next packet completes it.
    partner: bool,
}

/// What [`EntryPictures::step`] says to do with a packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// No part of an entry picture. Skip it unparsed.
    Skip,
    /// Send it and read nothing back yet: this is one field of a pair.
    Half,
    /// Send it. The entry picture is complete and the decoder can be drained.
    Whole,
}

impl<'a> EntryPictures<'a> {
    pub fn new(video: &'a VideoInfo) -> Self {
        Self {
            video,
            partner: false,
        }
    }

    /// What to do with the next packet of the video stream.
    pub fn step(&mut self, packet: &ff::Packet) -> Step {
        // The other half of a pair, whether or not it is marked: a recording
        // that marks both fields of its entry pictures is still describing
        // one picture, and the field after a field is that picture's second
        // half either way.
        if std::mem::take(&mut self.partner) {
            return Step::Whole;
        }
        if !packet.is_key() {
            return Step::Skip;
        }
        self.partner = bitstream::is_field_picture(
            packet.data().unwrap_or(&[]),
            &self.video.codec,
            self.video.framing,
            self.video.field_shape.as_ref(),
        );
        if self.partner {
            Step::Half
        } else {
            Step::Whole
        }
    }

    /// Forget a half the caller could not send, so that the packet behind it
    /// is not taken for a partner of nothing.
    pub fn broke(&mut self) {
        self.partner = false;
    }
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
        mut joins,
        // The walk is about to say where every picture a cut can start from
        // is, which is this one and all the rest.
        head: _,
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

    // A map the recording did not read for itself can be about something
    // else. See [`index::mend_stretches`]: on one recorder clip of the twenty
    // measured here, one of its six stretches carries entry points six
    // seconds ahead of the pictures they name, and the cut of that clip
    // stopped. Asked only of a clip that has seams -- which is a clip a
    // recorder wrote -- and only of an index that did not come from the
    // stream, since a walk cannot disagree with what it read.
    let mut mended_any = false;
    if !idx.leading_known && !joins.is_empty() {
        match index::mend_stretches(&input.url, &video, start_time, &mut joins, &mut points) {
            Ok(mended) => {
                mended_any = !mended.is_empty();
                for at in mended {
                    note_once(format!(
                        "note: the entry points this disc records for the stretch of {path} \
                         beginning at {at:.3}s do not name the pictures they sit in front of, \
                         so that stretch was read for its own. The rest of the map is used as \
                         it stands.",
                    ));
                }
            }
            // Nothing here is worth failing an open over: the map is what
            // it was, and a cut that lands in a bad stretch says so itself.
            Err(e) => eprintln!("note: could not check this recording's entry-point map: {e}"),
        }
    }

    let gaps: Vec<f64> = points.windows(2).map(|w| w[1].time - w[0].time).collect();
    let mean_gop = if gaps.is_empty() {
        1.0
    } else {
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    let seek_margin = (3.0 * mean_gop).clamp(1.0, 30.0);

    video.pulldown = idx.pulldown.unwrap_or(false);
    video.variable_rate |= idx.variable.unwrap_or(false);
    video.bit_rate = idx.bit_rate;

    // A container that says it is shorter than the pictures it holds is a
    // program stream; see [`index::Index::end`]. Only ever longer, so that a
    // container which knows its own length keeps it: trailing sound after the
    // last picture is part of a recording, and the pictures do not bound it.
    //
    // Taken again off the points where a stretch was mended, because that is
    // what it was taken off the first time: an index with no length in it
    // stands the last entry point up as one, and a mended stretch may have
    // moved which entry point that is.
    let end = match (idx.end, mended_any) {
        (Some(_), true) => {
            let last = points
                .iter()
                .map(|p| p.time)
                .fold(f64::NEG_INFINITY, f64::max);
            last.is_finite().then(|| last + video.frame_duration())
        }
        (end, _) => end,
    };
    let duration = match end {
        Some(end) if end > duration => end,
        _ => duration,
    };

    let mut src = Source {
        path: path.to_string(),
        joins,
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
    /// See [`Source::joins`].
    pub joins: Vec<restamp::Seam>,
    /// Where the material begins: the presentation time of the first picture
    /// a cut could start from, which is the walk's `points[0]` arrived at
    /// without the walk. `None` where the front of the file did not hold one,
    /// and `None` for every way in but [`outline_with_head`], which is the
    /// only one that pays the read. See [`first_picture`].
    pub head: Option<f64>,
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
            joins: self.joins,
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

/// The same, and with the first picture found as well.
///
/// The demuxer is read a little further before it is let go, for the one
/// thing the probe does not say and a reader can. See [`first_picture`].
///
/// **Apart from [`outline`] because most callers want nothing of it.** The
/// clip list sweeps `outline` over every file dropped on it, and a folder of
/// broadcast recordings on a share is thousands of them; the head is the cut
/// editor's question and nobody else's, so nobody else pays a read for it.
pub fn outline_with_head(path: &str) -> Result<Outline> {
    let (mut o, mut ictx) = outline_of(path)?;
    o.head = first_picture(&mut ictx, &o.video, o.start_time);
    Ok(o)
}

/// As [`outline`], handing back the demuxer it opened along with it.
///
/// [`scan_reporting`] wants both: the answer, and the open file to walk. An
/// index source takes the demuxer, so it has to come out of here rather than
/// be opened a second time.
fn outline_of(path: &str) -> Result<(Outline, input::Demux)> {
    init()?;
    let input = input::Input::parse(path)?;
    // What libavformat says when it is handed encrypted bytes -- "Invalid
    // data found when processing input" -- describes them accurately and
    // says nothing about what is wrong. A recorder's own disc does: it
    // writes its index in the clear and encrypts every stream beside it, so
    // such a disc lists its recordings perfectly and opens none of them.
    // Said here rather than in either front end, because both reach it.
    let ictx = crate::input::demux(&input.url).map_err(|e| {
        if crate::disc::encrypted(path) {
            anyhow!(
                "{path}: this recording is encrypted with AACS, which this program does not \
                 decrypt. The disc's index is not encrypted, which is why its recordings \
                 could be listed at all; an image made with the encryption taken off opens \
                 as any other disc does. ({e})"
            )
        } else {
            anyhow!("cannot open {path}: {e}")
        }
    })?;
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
    // The PID the pictures arrive on, which is how the recording's own
    // service is told from the others its map may name.
    let video_id = stream.id();
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
    // And what the container says its pictures are *meant* to come at, which
    // is a different question and on a variable-rate recording a different
    // number. See [`VideoInfo::base_rate`].
    let base_rate = f64::from(stream.rate());

    // Read before the sound is described rather than with the rest of the
    // file's own numbers below, because describing the sound needs them: see
    // [`audio::settled_shape`], which is told where the middle of the
    // recording is.
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
            said: None,
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
    let (mut audios, folded) = one_track_per_pid(audios, on_a_ts);
    // What the probe described is the head of the file, and the head of a
    // broadcast recording is the end of the programme before it. Where that
    // programme's sound was a different shape from this one's -- mono read
    // ahead of a stereo programme, which is an ordinary evening -- the
    // description is corrected to the recording's own. See
    // [`audio::settled_shape`].
    //
    // Asked of a recording made off the air and of nothing else. A disc's
    // clip is a transport stream too, and is one programme from its first
    // frame: nothing in it can disagree with its own opening, and the seeks
    // this costs are not free enough to spend on finding that out -- least
    // of all inside an image, where every one of them is a walk of the
    // volume as well.
    if on_a_ts && disc::clip_on_a_disc(path).is_none() {
        for a in audios.iter_mut() {
            let Some((channels, sample_rate)) =
                audio::settled_shape(&input.url, a, start_time, duration)
            else {
                continue;
            };
            note_once(format!(
                "note: the sound on {} is {} channel(s) at {} Hz where this recording opens, \
                 and {channels} at {sample_rate} Hz through the rest of it -- the opening \
                 belongs to the programme before this one, which a tuner told to start \
                 early records the end of. The recording's own is followed.",
                track_name(on_a_ts, a.pid, a.stream_index),
                a.channels,
                a.sample_rate,
            ));
            a.channels = channels;
            a.sample_rate = sample_rate;
            // And the rate the container stated, which was read off the same
            // frames and describes the same wrong programme: 208 kbit/s for a
            // stereo opening in front of a 5.1 programme carried at 386. Left
            // in, it is what a whole-track re-encode would spend on six
            // channels. There is no measuring the real one here without
            // reading a second of sound at each place looked at, which is a
            // hundred times what this check costs, so it is given up rather
            // than guessed at: what a codec is worth at this many channels
            // stands in for it. See `derived_bit_rate` in [`cut`].
            a.bit_rate = None;
        }
    }
    // What the recording's own map calls its streams. Only a transport
    // stream has one, and it is asked about the text streams below and about
    // the sound above: everything else here the demuxer already named well
    // enough.
    //
    // The sound's PIDs are handed over so that a map which does not name
    // them all is a map to keep reading past. A broadcast announces a
    // second sound track when the programme starts, and a recorder starts
    // before that: the first copy of the map describes whatever was on
    // before. See [`si::stream_tags`].
    let described = if on_a_ts {
        let pids: Vec<u16> = audios
            .iter()
            .filter_map(|a| u16::try_from(a.pid).ok())
            .collect();
        si::stream_tags(&input, u16::try_from(video_id).unwrap_or(0), &pids)
    } else {
        None
    };
    let streams: &[si::ElementaryStream] = described.as_ref().map_or(&[], |s| &s.streams);
    // What the broadcast says about each of its sound tracks.
    //
    // **More than one sound track means one of two things and the sound
    // cannot say which.** A programme sent in two languages carries the
    // original and the dub; a programme sent with commentary for a viewer
    // who cannot see the picture carries the programme and the commentary.
    // Both arrive as two stereo AAC tracks at the same rate, described
    // identically by the demuxer -- of 37 such recordings in a corpus of
    // 400, 34 name Japanese on both tracks. What separates them is the name
    // the broadcaster writes beside each one, 英語 against 音声解説, and
    // that is in the programme description and nowhere else.
    //
    // Asked only where there is more than one track. On the other 363
    // recordings there is a single track, it is the main sound whatever any
    // table says, and the read this costs would buy nothing.
    if audios.len() > 1 {
        if let Some(service) = &described {
            let tag_of = |a: &AudioInfo| {
                u16::try_from(a.pid)
                    .ok()
                    .and_then(|pid| service.stream(pid))
                    .and_then(si::ElementaryStream::component_tag)
            };
            let tags: Vec<u8> = audios.iter().filter_map(tag_of).collect();
            let said = si::sound_tracks(&input, service.service_id, &tags);
            for a in audios.iter_mut() {
                let Some(tag) = tag_of(a) else { continue };
                let Some(track) = said.iter().find(|s| s.component_tag == tag) else {
                    continue;
                };
                // The map is allowed to have said it first: a language in
                // the map is about the stream, where this one is about the
                // programme being carried on it.
                if a.language.is_none() {
                    a.language.clone_from(&track.language);
                }
                a.said = Some(track.clone());
            }
        }
    }
    // Which of them is the main sound. The broadcast's own answer where it
    // gave one -- it is the only party here that knows whether the second
    // track is a language or a commentary, and a player that opens on the
    // commentary has opened on the wrong one.
    //
    // Otherwise libav's, since it weighs the disposition flags a container
    // may carry; and the first track where it has no opinion. libav's answer
    // is the widest track, which on a disc is right and on a broadcast is a
    // coin toss: on a two-track recording measured here it named the second.
    let audio = audios
        .iter()
        .find(|a| a.said.as_ref().is_some_and(|s| s.main))
        .cloned()
        .or_else(|| {
            ictx.streams()
                .best(ff::media::Type::Audio)
                .map(|a| a.index())
                .and_then(|i| audios.iter().find(|a| a.stream_index == i).cloned())
        })
        .or_else(|| audios.first().cloned());
    // Which of the three text streams a stream is, where it is one at all.
    //
    // **libav names one of them and lumps the other two together.** The
    // captions of a high definition broadcast come back as `arib_caption`;
    // its crawl and a 4K recording's subtitles both come back as `bin_data`,
    // which is to say as bytes of no stated kind. What tells those apart is
    // the component tag in the recording's own map -- 0x30..0x37 the
    // captions, 0x38..0x3F the crawl -- and whether the map also carries the
    // data component descriptor that says the text is ARIB's.
    let text_of = |pid: i32, id: ff::codec::Id| -> Option<(TextKind, TextFormat)> {
        let told = streams.iter().find(|e| i32::from(e.pid) == pid);
        text_stream(
            told.and_then(si::ElementaryStream::component_tag),
            told.and_then(si::ElementaryStream::data_component_id),
            id == ff::codec::Id::ARIB_CAPTION,
        )
    };
    // Captions are subtitles here only in the sense libav means it: neither
    // an ARIB caption stream nor a 4K recording's TTML is decoded to be
    // carried, they are copied. A disc's subtitles are the other kind and
    // are gathered separately, just below.
    let mut captions: Vec<CaptionInfo> = ictx
        .streams()
        .filter(|s| !indexed(s.id()))
        .filter(|s| {
            matches!(
                s.parameters().id(),
                ff::codec::Id::ARIB_CAPTION | ff::codec::Id::BIN_DATA
            )
        })
        .filter_map(|s| {
            let (kind, format) = text_of(s.id(), s.parameters().id())?;
            Some(CaptionInfo {
                stream_index: s.index(),
                pid: s.id(),
                language: s.metadata().get("language").map(str::to_string),
                time_base: f64::from(s.time_base()),
                kind,
                format,
                // On the demuxer's own clock for the moment; rebased below,
                // where the file's own beginning is worked out.
                base: match format {
                    TextFormat::Arib => 0.0,
                    TextFormat::Ttml => disc::clip_presentation_start(path).unwrap_or(0.0),
                },
            })
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
            // Nor is a text stream this cut carries -- the captions, and
            // the crawl beside them. What is left here is the `bin_data`
            // the map had nothing to say about.
            if captions.iter().any(|c| c.stream_index == s.index()) {
                return None;
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

    // Everything else here is counted from where the file begins, so the
    // clock a TTML document's times are counted from is put on the same
    // footing now that that is known.
    for c in captions.iter_mut() {
        if c.format == TextFormat::Ttml {
            c.base -= start_time;
        }
    }

    let video = VideoInfo {
        stream_index,
        codec: codec.clone(),
        width,
        height,
        frame_rate,
        base_rate,
        has_b_frames,
        time_base,
        sample_aspect_ratio,
        framing,
        field_order,
        pulldown: false, // the index source reports this, when it can
        // **The container can see one half of this for itself.** A recording
        // whose pictures average out faster than the rate it declares has
        // some of them closer together than that rate allows -- there is no
        // other way to arrive at the average -- and that is the half which
        // has to be known before a frame of the output is written, because it
        // decides how finely the output timeline is divided. The other half,
        // a recording that holds a picture longer than it says, only the walk
        // sees, and it says so when it has.
        //
        // **The margin is a hundredth of a percent, and it has to be.** A
        // recording of twenty-five minutes with ten fast pictures in it
        // averages 23.980 against a declared 23.976, which is two parts in
        // ten thousand; anything looser lets those ten pictures through, and
        // ten pictures with nowhere to go are ten pictures dropped. What a
        // margin that tight costs is a constant-rate recording whose two
        // figures disagree in the last place being called variable -- and
        // that costs nothing at all, because the timeline it then gets is
        // the same rate divided more finely, which lands every picture in
        // exactly the same place. The one answer that would be wrong is
        // taking an interlaced recording's field rate for its frame rate,
        // and that cannot arrive here: there the declared rate is twice the
        // average, not below it.
        variable_rate: base_rate > 0.0 && frame_rate > base_rate * 1.0001,
        bit_rate: None,       // and this, when it read the pictures to find out
        // Read once, here, rather than hunted for in every packet: a
        // transport stream restates these in front of each entry point, so
        // libavformat has them before a packet has been asked for.
        vc1: matches!(codec.as_str(), "vc1" | "wmv3")
            .then(|| smartcut_vc1::Shape::read(&extradata))
            .flatten(),
        // Likewise, and for the same reason: whether a picture is a field is
        // one bit of it, and where that bit sits is in here.
        field_shape: bitstream::field_shape(&codec, &extradata, framing),
    };

    let outline = Outline {
        path: path.to_string(),
        // On the same clock as everything else here. [`input::joins`] answers
        // on the demuxer's own, which a recording that does not begin at zero
        // -- a transport stream almost never does -- reads seconds later than
        // the times a cut is asked for. Left uncorrected, a seam was planned
        // that far into the stretch *after* it: the range before it asked to
        // re-encode pictures that belong to the next recording and cannot be
        // decoded from this one, so the last seconds of every stretch were
        // quietly lost, and where nothing at all in that window would decode
        // the cut stopped with "no pictures decoded". On one clip the error
        // was five seconds.
        joins: input::joins(&input)
            .into_iter()
            .map(|s| restamp::Seam {
                time: s.time - start_time,
                ends: s.ends - start_time,
                ..s
            })
            .collect(),
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
        // Filled by [`outline_with_head`] alone. See [`first_picture`].
        head: None,
    };
    Ok((outline, ictx))
}

/// Where the material begins, read off the front of the file.
///
/// The walk's answer to this is `points[0]`, and this is the same picture;
/// what it saves is the walk. Everything the editor counts from the first
/// picture is out by the second or so a broadcast recording opens with until
/// somebody knows where that picture is -- the frame counter, the length of
/// the timeline, and the numbers in a mark file above all, which count
/// pictures from it. The walk is tens of seconds over a share; this is a
/// megabyte or two.
///
/// **The smallest presentation time among the key pictures at the front**,
/// rather than the first key picture to arrive: the list the walk builds is
/// sorted by presentation time, and on an open GOP the two are not always the
/// same packet. A second past the first of them is further than any reorder
/// goes.
///
/// Bounded at both ends, because the point of it is to be cheap: a stream
/// whose first key picture is further in than the limit says nothing rather
/// than reading on, and the window waits for the walk as it did before.
fn first_picture(ictx: &mut input::Demux, video: &VideoInfo, start_time: f64) -> Option<f64> {
    /// How far into the file to look for the first of them. Room for a
    /// Blu-ray's pictures, which are a megabyte apiece, where a broadcast
    /// recording opens on a sequence header inside the first one.
    const LIMIT: isize = 32 << 20;
    /// And how much further to read once one has been found, for the reorder.
    const REORDER: f64 = 1.0;
    /// What bounds it where the demuxer will not say which byte a packet came
    /// from -- a pipe, a stream.
    const PACKETS: u64 = 200_000;
    let mut head: Option<f64> = None;
    let mut seen: u64 = 0;
    for (s, p) in ictx.packets() {
        seen += 1;
        if p.position() > LIMIT || seen > PACKETS {
            break;
        }
        if s.index() != video.stream_index || !p.is_key() {
            continue;
        }
        let Some(pts) = p
            .pts()
            .map(|t| t as f64 * video.time_base - start_time)
            .filter(|t| t.is_finite())
        else {
            continue;
        };
        match head {
            Some(t) if pts >= t + REORDER => break,
            Some(t) => head = Some(t.min(pts)),
            None => head = Some(pts),
        }
    }
    // The same floor the walk's points are given, and for the same reason:
    // rebasing by the container's start time puts the first picture a
    // fraction of a microsecond below zero where the two disagree in the last
    // bit. Left off, the head read as -3.3e-7 where the walk said 0, and a
    // mark put down on it was a mark the timeline had already closed over --
    // which is a keyframe list that reads five marks and shows four. See
    // [`scan_with`].
    head.map(|t| t.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three text streams, told apart by what a recording's own map says
    /// about them -- the tags and descriptors here are the ones read off a
    /// high definition broadcast and a 4K recorder's disc.
    #[test]
    fn tells_the_text_streams_apart() {
        // A high definition broadcast: captions and crawl, both ARIB.
        assert_eq!(
            text_stream(Some(0x30), Some(0x0008), true),
            Some((TextKind::Caption, TextFormat::Arib))
        );
        assert_eq!(
            text_stream(Some(0x38), Some(0x0008), false),
            Some((TextKind::Superimpose, TextFormat::Arib))
        );
        // A 4K recording: the captions' own tag, no data component
        // descriptor, and libav with no name for it.
        assert_eq!(
            text_stream(Some(0x30), None, false),
            Some((TextKind::Caption, TextFormat::Ttml))
        );
        // A recording that adds its caption stream to the map after the
        // opening, which libav finds for itself.
        assert_eq!(
            text_stream(None, None, true),
            Some((TextKind::Caption, TextFormat::Arib))
        );
        // The data broadcast, which is neither and is not carried.
        assert_eq!(text_stream(Some(0x40), None, false), None);
        assert_eq!(text_stream(None, None, false), None);
    }

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
            said: None,
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
