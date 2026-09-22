//! Assemble the planned segments into an output file.
//!
//! This is the part the CLI prototype could not do. Driving ffmpeg meant
//! handing it elementary streams that carry no timestamps at all, so it had
//! to synthesise them from a frame rate plus picture-order counts -- and the
//! first few packets came out with no PTS, because the parser cannot reorder
//! until its window fills. Here every timestamp is assigned directly, in
//! integer ticks, from the display index each picture is known to occupy.
//!
//! Segments are processed one at a time, each seeking into its own input
//! context. A re-encode has to start decoding well before the frames it
//! actually wants, so a single forward pass over the file cannot serve both
//! kinds of segment.

use anyhow::{anyhow, bail, Context, Result};
use ffmpeg_next as ff;

use crate::adts::AacVersion;
use crate::bitstream::{
    annexb_to_length, is_annexb, length_to_annexb, parameter_sets, prepend_parameter_sets,
    prepend_parameter_sets_annexb, NalFraming,
};
use crate::{RangePlan, Segment, SegmentKind, Source};

/// How long a disc's stream waits before it shows its first picture, in
/// seconds. See where it is set, below.
const LEAD_IN: f64 = 0.4;

/// How H.264/HEVC payloads have to be shaped on the way out.
///
/// MP4 stores NAL units length-prefixed and keeps one set of parameter sets
/// in `avcC`. A smart cut breaks both assumptions: the encoder emits Annex-B,
/// and its SPS necessarily differs from the source's. The fix is the `avc3`
/// sample entry, which allows parameter sets in-band -- so the encoder's own
/// sets travel with its pictures, and the source's are restated in front of
/// every copied keyframe to re-activate them after a splice.
struct Reframe {
    nal_length: usize,
    sets: Vec<Vec<u8>>,
}

/// The other direction: a recording out of an MP4 on its way into a
/// transport stream.
///
/// **Both containers hold the same pictures and disagree about how a NAL
/// begins.** An MP4 puts a length in front of each and keeps the parameter
/// sets in the `hvcC`; a transport stream separates them with start codes and
/// expects the sets to arrive in the stream. Copied pictures went across
/// untouched, so a cut of an MP4 written as `.ts` came out with four bytes of
/// length where the decoder wanted `00 00 00 01`: everything but the
/// re-encoded fringes was unreadable, which on a ten second cut was 599
/// pictures out of 635. The re-encoded ones are already start-coded -- that
/// is what the encoder writes -- so only the copies pass through here.
struct Unframe {
    nal_length: usize,
    sets: Vec<Vec<u8>>,
}

/// How the audio track is produced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    /// Keep the source's own frames, whatever is in them. Lossless to the
    /// byte, and each range's boundary lands on a whole audio frame -- so the
    /// frame the cut falls inside arrives whole, carrying up to 21 ms of the
    /// material that was cut away.
    Copy,
    /// As [`AudioMode::Copy`], except for the frames a boundary falls inside:
    /// those are re-encoded from the recording's own samples with the far
    /// side of the cut faded out. Smart rendering, applied to audio -- the
    /// boundary still lands on a whole frame, but what fills the rest of that
    /// frame is silence instead of the material that was cut away.
    ///
    /// The default, and in the ordinary case identical to [`AudioMode::Copy`]
    /// byte for byte: a commercial break is cut in the silence around it,
    /// where there is nothing on the far side to remove and nothing is
    /// re-encoded. What it costs is two frames per boundary that lands in the
    /// middle of sound; what it buys is that no cut is heard twice.
    #[default]
    Smart,
    /// Decode, trim to the exact sample, re-encode. Sample-exact at every
    /// boundary, at the cost of re-encoding the whole track.
    Reencode,
}

impl AudioMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioMode::Copy => "copy",
            AudioMode::Smart => "smart",
            AudioMode::Reencode => "reencode",
        }
    }
}

/// What the sound is written as.
///
/// The default is the recording's own codec, which is what every mode but
/// [`AudioMode::Reencode`] can offer: a copied frame is a frame of whatever
/// it already was. Naming one of the others asks for the track to be built
/// again from its samples, and that is a whole-track re-encode however the
/// mode was set -- the same way a downmix is.
///
/// Which four these are is a question of what a cut of a recording is
/// afterwards *for*. AAC is what the broadcast carried and what every player
/// on a phone takes. AC-3 is what a disc player expects and what a receiver
/// decodes without being asked twice. DTS is the other one a receiver knows.
/// LPCM is no codec at all -- the samples, written down -- which is what an
/// editor further down the line would rather be handed than a second
/// generation of something lossy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// The recording's own, wherever the container has a box for it.
    #[default]
    Source,
    Aac,
    /// Linear PCM. Blu-ray's flavour of it into a transport stream, because
    /// that is the only one a transport stream can declare; big-endian PCM
    /// anywhere else. See [`carriage`].
    Lpcm,
    Ac3,
    Dts,
}

impl AudioCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioCodec::Source => "source",
            AudioCodec::Aac => "aac",
            AudioCodec::Lpcm => "lpcm",
            AudioCodec::Ac3 => "ac3",
            AudioCodec::Dts => "dts",
        }
    }

    /// The name as it is written on a command line or sent down from the
    /// window. `None` for anything else, so the caller can say what it
    /// wanted rather than silently taking the recording's own.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "source" | "same" => Some(AudioCodec::Source),
            "aac" => Some(AudioCodec::Aac),
            "lpcm" | "pcm" => Some(AudioCodec::Lpcm),
            "ac3" | "ac-3" => Some(AudioCodec::Ac3),
            "dts" => Some(AudioCodec::Dts),
            _ => None,
        }
    }
}

/// What becomes of the subtitles a disc draws.
///
/// Two answers, and neither is wrong. **Pgs** puts them inside the cut, as
/// the kind of subtitle a transport stream carries: a Blu-ray's own travel
/// untouched and a DVD's are converted -- the same pixels and the same
/// colours, spelled the way a Blu-ray spells them -- so that the cut is one
/// file with its subtitles in it and every reader that opens it finds them.
/// **Beside** writes them out as the `.idx` and `.sub` pair players already
/// read: two files next to the cut, which is where a DVD's can stay
/// untouched and where a Blu-ray's can be read by a tool that has never
/// heard of a display set.
///
/// Which way round the conversion runs is decided by what the recording is,
/// and both are here: [`crate::pgs::write`] writes a DVD's subtitles as a
/// Blu-ray's, [`crate::vobsub::unit`] writes a Blu-ray's as a DVD's, and
/// [`crate::vobsub`] holds the pair either of them may end up in.
///
/// **Sup** is the third answer and the one that converts nothing at all: the
/// display sets themselves, written into the file a PGS stream lives in
/// outside a container -- a `.sup`, which is what every tool that works on a
/// disc's subtitles reads. A Blu-ray's come out of it byte for byte, and a
/// DVD's are converted on the way as they are for the cut itself.
///
/// Inside the cut is the default because one file is one file. A cut that is
/// not a transport stream cannot hold either kind, and falls back to the pair
/// beside it rather than losing them; the two files beside it are written
/// whatever the container is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Subtitles {
    Beside,
    #[default]
    Pgs,
    Sup,
}

#[derive(Debug, Clone, Default)]
pub struct CutOptions {
    /// Reorder depth used when deriving DTS from decode order.
    /// `None` takes the source stream's own depth.
    pub reorder_depth: Option<i64>,
    /// Bits per second for re-encoded pictures. `None` derives it from the
    /// source, with a little headroom so the splice does not visibly soften.
    pub bit_rate: Option<usize>,
    /// The quantizer step VC-1 pictures are written at, 3 (fine) to 31
    /// (coarse). `None` takes [`VC1_DEFAULT_QUANT`]. Nothing else uses it:
    /// every other codec is given a bit rate and left to spend it.
    pub vc1_quant: Option<u8>,
    pub audio_mode: AudioMode,
    /// What the sound is written as. The default is the recording's own
    /// codec; anything else is a whole-track re-encode, whatever
    /// `audio_mode` says. See [`AudioCodec`].
    pub audio_codec: AudioCodec,
    /// Bits per second for re-encoded audio. `None` follows the source --
    /// or, where the codec is not the source's, what that codec is worth at
    /// the channel count being written. See [`derived_bit_rate`].
    pub audio_bit_rate: Option<usize>,
    /// Channels the audio is written with. `None` follows the source.
    ///
    /// A count that is not the source's is a downmix -- 5.1 into stereo, for
    /// the recording whose surround track is a nuisance everywhere it is
    /// played back. Nothing about it is a splice: a stereo frame cannot sit
    /// among the recording's 5.1 ones, so asking for one asks for the whole
    /// track, and [`AudioMode::Reencode`] is what actually runs.
    pub audio_channels: Option<u16>,
    /// Samples per second the audio is written at. `None` follows the source.
    ///
    /// A rate that is not the source's is a resample, and like a downmix it
    /// leaves no frame of the recording's that can be copied through: the
    /// samples land on a different grid, so the whole track is built again
    /// and [`AudioMode::Reencode`] is what actually runs.
    ///
    /// Not every codec speaks every rate -- AC-3 has three -- so what is
    /// asked for here is taken to the nearest the codec being written can
    /// name. See [`crate::audio::writable_rate`].
    pub audio_sample_rate: Option<u32>,
    /// Bits per sample the audio is written with. `None` follows the source.
    ///
    /// Only means anything where what is being written carries samples
    /// rather than a description of them -- linear PCM, in other words. A
    /// lossy encoder takes a float and spends a bitrate; how many bits the
    /// sound had before it is not a number it has anywhere to put, and one
    /// asked for is declined out loud.
    ///
    /// 16 or 24. Like a rate, a width that is not the source's leaves no
    /// frame to copy and runs [`AudioMode::Reencode`].
    pub audio_bits: Option<u8>,
    /// Which AAC the frames this tool encodes announce themselves as.
    /// The default follows the recording, which for a Japanese broadcast
    /// means MPEG-2 AAC.
    pub aac: AacVersion,
    /// Source stream indices to leave out of the output.
    ///
    /// Everything the recording carries that a cut can carry is written
    /// unless it is named here. A bilingual broadcast has two sound tracks
    /// and both are kept, because which of them is wanted is not something
    /// this can know -- the caller says, or neither is dropped.
    pub drop_streams: Vec<usize>,
    /// Where a DVD's subtitles go. See [`Subtitles`].
    pub subtitles: Subtitles,
    /// Subpicture streams to leave out, by the substream id the disc names
    /// them with.
    ///
    /// Not an index, because a DVD's subtitles may have none: see
    /// [`crate::SubpictureInfo`].
    pub drop_subpictures: Vec<i32>,
    /// Which account of itself the output carries. `None` takes whichever
    /// suits where the cut is going; see [`tables_for`].
    ///
    /// The muxer writes a description of the streams and stops there. What
    /// the other two settings restore is everything else a broadcast says
    /// about itself -- the service and its name, the programme, the times,
    /// and the descriptors that say what each stream is -- either as the one
    /// table a recording is written down in or in the shape the broadcast
    /// sent them. See [`crate::si::Tables`]. Only means anything writing a
    /// transport stream.
    pub tables: Option<crate::si::Tables>,
    /// Whether to carry the recording's data broadcast into the cut.
    ///
    /// `None` takes the answer that suits where the cut is going, which is
    /// to carry it wherever it can be carried: **a cut of a recording is
    /// meant to be the recording, shorter**, and what is behind the blue
    /// button was in the recording. `Some(false)` leaves it out, which is
    /// what to ask for when the file matters more than the pages -- a
    /// carousel is between a hundredth and a fifth of what a broadcast
    /// multiplex spends.
    ///
    /// Only means anything writing a plain `.ts` that keeps the broadcast's
    /// own tables. A carousel cannot be muxed at all -- see
    /// [`crate::carousel`] -- so it is carried by the same pass that puts the
    /// tables back, and a disc's own framing has no place to put it. Asked
    /// for outright where it cannot go, the cut says so; left to the default
    /// it goes quietly, because a default cannot be disappointed.
    pub data_broadcast: Option<bool>,
    /// What share of their own size the pictures are to be written back as,
    /// where the cut has to fit somewhere it otherwise would not.
    ///
    /// `None`, and every copied picture is the recording's own bytes. A share
    /// less than one puts each of them through
    /// [`smartcut_mpeg2::Transrater`], which writes the same picture with
    /// less in it: the same GOP, the same motion, the same timestamps, the
    /// same coded structure, and the coefficients written less finely and
    /// thinned of what is not worth its bits. It is not a re-encode and it is
    /// far faster than one -- nothing here decodes a picture.
    ///
    /// **MPEG-2 only.** It is the format a broadcast and a recorder's own
    /// disc are in, and it is the one whose macroblock layer can be rewritten
    /// without decoding. A recording in anything else is copied as it is and
    /// the cut says so.
    pub video_share: Option<f64>,
    /// How long the sound takes to leave and to come back at a seam, in
    /// seconds. `0.0` for none, which is the default.
    ///
    /// A cut joins two instants that were never next to each other, and what
    /// the sound does at that instant is a step: a click at best, half a
    /// word at worst. A fade takes the level down into the join and brings it
    /// back out, so that what is heard is a pause rather than a jump.
    ///
    /// **It is not free, and not only in re-encoded frames.** What it fades
    /// is the programme: a second either side of every seam is a second of
    /// the recording quieter than it was recorded. That is why the default is
    /// none -- a cut of a recording is meant to be the recording, shorter --
    /// and why the number is the caller's to choose rather than something
    /// picked here.
    ///
    /// Only where the sound is being written by this program: smart
    /// rendering, where the frames the fade runs over are rewritten, and a
    /// whole-track re-encode, where every frame is anyway. A track copied
    /// through is copied through, and one carried whole because re-encoding
    /// it would lose what makes it lossless stays carried whole. The cut says
    /// so rather than fading nothing quietly.
    pub audio_fade: f64,
}

/// How far past a segment's end the reader will go for a stream that has
/// nothing more to say.
///
/// A segment stops when every stream it is gathering has run past the end,
/// which works for streams that are actually there. A caption PID a
/// recording declares in its map and never sends a packet on is never going
/// to run past anything, and without a bound it holds the read open to the
/// end of the file -- once per segment, on a recording of several gigabytes.
///
/// The pictures say where the read has got to. Streams are interleaved
/// within a fraction of a second of each other, so a packet that has not
/// arrived three seconds later is not going to.
const TRAIL: f64 = 3.0;

/// One packet on its way out, before timestamps are assigned.
struct Emitted {
    packet: ff::Packet,
    /// Position on the output display timeline, in fields.
    display: i64,
    /// How long this picture is shown, in fields.
    fields: i64,
}

/// Where the source's audio is and how to place it on the output timeline.
///
/// Audio has no GOP structure, so it is not cut per video segment -- it is
/// mapped continuously across a whole keep-range. Each range is anchored to
/// the output time its video starts at, which is what keeps A/V sync from
/// drifting when several ranges are joined: an error at one seam cannot
/// accumulate into the next.
struct AudioCtx {
    /// Which of the output's sound tracks this is.
    track: usize,
    in_index: usize,
    in_tb: f64,
    /// Seconds to add to a source time to reach the output timeline.
    offset: f64,
    /// Source time whose content should land at this range's output start.
    ///
    /// Not simply `range_in`. An MP4 audio track honours where the track
    /// begins but stores everything after that as consecutive durations, so
    /// samples are laid end to end and a frame dropped at one range boundary
    /// shifts all later ranges for good. Carrying the accumulated error into
    /// the next range's choice of opening frame keeps the error bounded to
    /// half a frame instead of compounding.
    pick_from: f64,
    /// Earliest frame the range may open with. The first range must not start
    /// before its own beginning -- there is no negative output time to put it
    /// at -- but later ranges may, since they are placed by concatenation.
    min_start: f64,
    /// Sample range of this keep-range on the source timeline, used when the
    /// audio is re-encoded and boundaries can be exact.
    window: (i64, i64),
    /// How far the fade reaches into this range at each end, in samples.
    /// Zero at both ends unless one was asked for; see [`CutOptions::audio_fade`].
    fades: crate::audio::Fades,
    /// Where the range starts, in seconds.
    range_in: f64,
    mode: AudioMode,
}

/// Where a caption stream sits on the output timeline.
///
/// Simpler than sound, and for a reason worth stating: a caption statement
/// is whole inside one packet with one presentation time, so there is
/// nothing to splice and nothing that can straddle a boundary. Each packet
/// either falls inside a kept range or it does not, and the ones that do are
/// moved by the same offset the pictures were.
struct CaptionCtx {
    track: usize,
    in_index: usize,
    in_tb: f64,
    offset: f64,
    /// See [`CaptionTrack::ttml`].
    ttml: bool,
    base: f64,
    /// The whole kept range, which is what a TTML document's times are
    /// clipped to -- a caption may straddle either end of one, and what it
    /// says is shown for as much of it as the cut keeps.
    range: (f64, f64),
}

/// Where a graphics stream sits on the output timeline.
///
/// The same three numbers a caption needs, and one difference that runs
/// through everything below: the packets these time come in groups, and a
/// group means nothing in halves. So what is carried is decided a display
/// set at a time, and both ends of a kept range need a set that was never
/// sent there. See [`crate::pgs`].
struct GraphicsCtx {
    track: usize,
    in_index: usize,
    in_tb: f64,
    offset: f64,
}

/// What a segment contributed, so the next one can be placed after it.
///
/// Spans are measured from the pictures that were actually emitted rather
/// than from the planner's arithmetic. The planner works on an idealised
/// grid of whole frames at multiples of the frame duration; real streams put
/// their pictures at an arbitrary phase, and pulldown material shows some of
/// them for three fields. Anchoring each segment on its own first picture
/// keeps the joins exact under both.
///
/// **What the pictures cannot say is how long the last one stays up.** A
/// picture is on screen until the next picture replaces it, and the next
/// picture belongs to the segment after this one -- or, at the end of a
/// range, to the material the cut leaves out. A stream that codes a constant
/// rate never has to say so, because there the two answers are the same
/// number; a variable-rate one holds a picture for as long as nothing
/// changed, and there the coded length is a floor and nothing more. So the
/// span is also asked where the segment's display coverage ends, and takes
/// whichever answer is longer. See [`held_to_the_end`].
#[derive(Debug, Default, Clone, Copy)]
struct Span {
    /// Fields this segment occupies on the output timeline.
    fields: i64,
    /// Pictures emitted.
    pictures: i64,
}

/// Hold the segment's last picture until its coverage ends.
///
/// **Measured on a variable-rate recording**: a thirty-second range of a
/// WebM whose pictures sit between 33 ms and 2.4 s apart came out 28.1 s
/// long, and 46 of its 505 pictures were a whole frame early -- every one of
/// them after the seam where the held picture was cut short. On a
/// recording whose gaps run from 16 ms to 117 ms it was 598 of 628. Each
/// seam loses the difference between what the last picture says it is worth
/// and how long it was really up, and everything after that seam moves by
/// the sum of them.
///
/// `seg.end` is a real instant rather than a figure off the planner's grid:
/// it is the entry point the next segment starts on, or the end of the
/// range. The file's own end bounds it, because a range may be asked for
/// past where the recording stops and a last picture held to an hour is not
/// a hold, it is an hour of nothing.
///
/// Never shortens a span. A copy that stopped early has its own reasons and
/// they are not this function's to overrule.
fn held_to_the_end(span: Span, anchor: Option<f64>, seg: &Segment, src: &Source, field: f64) -> Span {
    let Some(a) = anchor else { return span };
    let until = if src.duration > 0.0 {
        seg.end.min(src.duration)
    } else {
        seg.end
    };
    Span {
        fields: span.fields.max(((until - a) / field).round() as i64),
        ..span
    }
}

/// Where a segment sits in the output and how its packets must be shaped.
struct SegmentCtx<'a> {
    /// First display index this segment contributes.
    display_base: i64,
    /// What the output timeline is counted in. See [`Grid`].
    grid: Grid,
    reframe: Option<&'a Reframe>,
    /// Set where the recording's NALs carry lengths and the container being
    /// written wants start codes. See [`Unframe`].
    unframe: Option<&'a Unframe>,
    audio: &'a [AudioCtx],
    captions: &'a [CaptionCtx],
    graphics: &'a [GraphicsCtx],
    /// How far this range moved, which is what a subtitle written beside the
    /// cut is timed by. The stream ctxs above carry the same number; a
    /// subpicture has no ctx of its own because it has no output stream --
    /// see [`Subpictures`].
    offset: f64,
    /// Whether this is the opening segment of its keep-range, which is where
    /// the range's audio boundary decision is made.
    first: bool,
    /// What the recording says about its own colour, read once for the
    /// whole cut. See [`signalling_of`].
    signalling: &'a Signalling,
}

/// Everything the writer needs that is shared across segments.
struct Writer {
    octx: ff::format::context::Output,
    /// Whether a track's `pid` is really a PID, for the messages that name
    /// one. See [`crate::Source::on_a_ts`].
    on_a_ts: bool,
    /// Whether the cut is being written as a transport stream, which is the
    /// one output that will not take two frames of sound at the same
    /// instant. See [`Writer::push_audio`].
    into_ts: bool,
    /// Ticks per *unit*. The output timeline is measured in fields, not
    /// frames, because 2:3 pulldown shows some pictures for three fields and
    /// others for two -- a frame grid cannot express that, a field grid can,
    /// and for constant-rate material every picture is simply two fields.
    field_ticks: i64,
    /// Units per field, which is one unless the recording needs the
    /// timeline divided more finely than that. See [`Grid`].
    sub: i64,
    our_tb: ff::Rational,
    out_tb: ff::Rational,
    depth: i64,
    /// Pictures held back so DTS can be derived from display order.
    pending: std::collections::VecDeque<Emitted>,
    /// Display positions seen so far, smallest first.
    seen: std::collections::BinaryHeap<std::cmp::Reverse<i64>>,
    /// The decode time given to the last picture written. A muxer refuses a
    /// packet whose decode time does not come after the one before it, so
    /// each has to be told about its predecessor.
    last_dts: Option<i64>,
    written: i64,
    /// Pictures left out for having nowhere to go on the timeline. See
    /// [`Writer::emit_one`].
    skipped: i64,
    /// How many of the written pictures were half a frame.
    ///
    /// The plan counts frames and `written` counts pictures, which are the
    /// same thing until a recording codes field pairs. Then it is two
    /// pictures to the frame, and a bar built on the count alone reaches the
    /// end halfway through the cut. Halves are counted apart rather than the
    /// whole being scaled, because a picture shown for *three* fields is
    /// still one frame of the plan and pulldown must not move the bar
    /// either.
    halves: i64,
    /// The output's sound tracks, in the order they were added. A broadcast
    /// in two languages has two, and each is cut on its own -- one track's
    /// boundary frame is no business of another's.
    audio: Vec<AudioTrack>,
    /// The output's caption tracks, likewise.
    captions: Vec<CaptionTrack>,
    /// And the graphics tracks: a disc's own subtitles, which carry the
    /// state of the plane along with them. See [`GraphicsTrack`].
    graphics: Vec<GraphicsTrack>,
    /// A DVD's subtitles, which go beside the cut rather than into it.
    subpictures: Option<Subpictures>,
    /// Or into it, converted. Never both: see [`Subtitles`].
    converted: Vec<Converted>,
    /// Called as pictures land, with a 0..1 fraction. A long cut is mostly
    /// I/O, so the caller needs something to show.
    progress: Option<Box<dyn Fn(f64) + Send + Sync>>,
    expected: i64,
    /// Where the pictures are being written back smaller. See [`Shrink`].
    shrink: Option<Shrink>,
}

/// Writing the pictures back smaller, so that a run fits where it has to.
///
/// One of these lives for the whole of a cut rather than for a segment: what
/// the requantiser carries from picture to picture is the scale the material
/// has settled at and what the pictures so far have spent, and both of those
/// are about the recording rather than about a stretch of it. See
/// [`smartcut_mpeg2::Transrater`].
struct Shrink {
    rater: smartcut_mpeg2::Transrater,
    /// What share of its size the video is to come to.
    share: f64,
    /// The first picture that could not be rewritten, and how many followed
    /// it. Such a picture is written through as it arrived, so the answer is
    /// a cut that is a little larger than it was aiming for rather than a cut
    /// that failed -- but it is worth saying once.
    declined: Option<(String, u64)>,
}

impl Shrink {
    /// One picture's bytes, smaller.
    fn picture(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        match self.rater.picture(data, self.share) {
            Ok(out) => Some(out),
            Err(e) => {
                match &mut self.declined {
                    Some((_, n)) => *n += 1,
                    None => self.declined = Some((e.to_string(), 1)),
                }
                None
            }
        }
    }

    /// What the run came to, once it is over.
    fn say(&self) {
        let tally = self.rater.written;
        if tally.pictures == 0 {
            return;
        }
        crate::note_once(format!(
            "note: the pictures were written back at {:.1}% of their own size, which is {} of \
             {} MB, to fit where this is going",
            tally.share() * 100.0,
            tally.written_bytes / 1_000_000,
            tally.source_bytes / 1_000_000,
        ));
        if let Some((why, n)) = &self.declined {
            crate::note_once(format!(
                "note: {n} picture(s) were written through as they arrived rather than made \
                 smaller, because of {why}. The cut is that much larger than it was asked to be."
            ));
        }
    }
}

/// One sound track being written, and everything that is true of it alone.
///
/// Every field here used to be a field of the writer, back when there could
/// only be one. What made them a group is that a bilingual recording has two
/// tracks whose frames land at different instants, are re-encoded at
/// different boundaries and drift by different amounts -- so a single
/// running position, a single encoder and a single patch table describe one
/// of them and corrupt the other.
struct AudioTrack {
    /// Index of the stream in the output, and the time base the muxer gave
    /// it -- which is not the one it was asked for: MPEG-TS keeps time in
    /// 90 kHz whatever it is handed.
    out_index: usize,
    out_tb: f64,
    /// Where it came from.
    in_index: usize,
    info: crate::AudioInfo,
    /// The rate the track is written at -- the recording's own unless one
    /// was asked for. What a re-encoded packet's sample count is turned into
    /// a time with, which is not the recording's rate once the two differ.
    out_rate: u32,
    /// How this track is produced, which is not always how the caller asked:
    /// a downmix is a whole-track re-encode however it was requested.
    mode: AudioMode,
    written: i64,
    /// Source `pts` of the last frame written, which is what a guard frame's
    /// condition is checked against.
    prev: Option<i64>,
    /// Output time at which what has been written ends. The container
    /// concatenates samples, so this -- not the packet's nominal timestamp --
    /// is where the next frame will actually be heard.
    end: Option<f64>,
    /// Set when this track is being re-encoded; packets then carry the
    /// encoder's own timestamps instead of the source's.
    reencoder: Option<crate::audio::Reencoder>,
    /// Frames re-encoded because a boundary falls inside them, by the source
    /// packet's `pts`.
    patches: std::collections::HashMap<i64, crate::audio::Patch>,
    /// Set while the track is still waiting for a frame it may open on. See
    /// [`opens_a_truehd_track`]; cleared as soon as one is written.
    need_sync: bool,
    /// Whether this is a track that has to be joined at a sync at all. Kept
    /// beside `need_sync` because the wait comes round again at every kept
    /// range, and by then the flag that started it has been cleared.
    joins_at_sync: bool,
    /// The instant the last frame written to this track was placed at, and
    /// how many frames were left out for not coming after it. See
    /// [`Writer::push_audio`].
    last_out: Option<i64>,
    dropped: usize,
    /// What one frame of this track is worth in time, where the recording's
    /// own packets do not say and it had to be measured. Nought where they
    /// do say, which is the ordinary case. See [`assumed_frame`].
    frame_secs: f64,
    /// Frames left out for having no length at all -- neither their own nor
    /// a measured one. See [`Writer::push_audio`].
    no_length: usize,
}

/// One caption track being written.
struct CaptionTrack {
    out_index: usize,
    out_tb: f64,
    in_index: usize,
    in_tb: f64,
    /// Whether the times are inside the documents rather than on the
    /// packets, and where they are counted from. See [`crate::ttml`].
    ttml: bool,
    base: f64,
    written: i64,
    /// The moment given to the last statement written. A muxer refuses a
    /// packet that does not come after the one before it, and says only
    /// "Invalid argument" about it -- which stops the whole cut. Two
    /// statements sent a few microseconds apart land on the same tick of a
    /// 90 kHz clock honestly, and a damaged recording can send them out of
    /// order outright. See [`Writer::push_caption`]; the sound
    /// ([`Writer::push_audio`]) and a disc's graphics
    /// ([`Writer::push_graphics`]) each answer this in their own way.
    last_out: Option<i64>,
}

/// The subtitles going beside the cut, gathered as it is written.
///
/// Not a track: nothing of this goes into the output file. The units are put
/// aside as they go past and written beside the cut at the end, in the pair
/// [`crate::vobsub`] describes.
///
/// A DVD's arrive as units already and are put aside untouched. A Blu-ray's
/// arrive as display sets and are drawn into units here -- see [`draw`] --
/// which is where `ink` comes from: a DVD hands over its sixteen colours with
/// the disc's index, and a Blu-ray has no sixteen anywhere to hand over.
///
/// [`draw`]: Subpictures::draw
///
/// The two ends of a kept range are mended the way a Blu-ray's graphics are
/// ([`crate::pgs`]), and more simply, because a unit carries its own
/// timing: what was on screen when the range opens is written again at its
/// first frame, and what is still on screen when it ends is taken down by a
/// unit that says so and nothing else.
struct Subpictures {
    side: crate::vobsub::Sidecar,
    /// The palette being invented, where the subtitles came with none that
    /// fits the pair. `None` for a DVD's, whose own palette is the disc's.
    ink: Option<crate::vobsub::Ink>,
    /// What each stream last put up: when, on the output's own clock, and
    /// how long after that the unit takes itself down -- `None` for a unit
    /// that never says, which stands until something replaces it.
    standing: Vec<(i32, f64, Option<f64>)>,
    /// Subtitles that could not be drawn into a unit, for the note that says
    /// so. A unit states its own length in two bytes and a picture can be
    /// bigger than that; see [`crate::vobsub::unit`].
    refused: usize,
}

impl Subpictures {
    /// Draw one subtitle a disc put up as a display set into the unit that
    /// says the same thing, with its colours written into the palette being
    /// built. `None` where it will not go into one; see
    /// [`crate::vobsub::unit`].
    fn drew(&mut self, drawn: &crate::vobsub::Drawn) -> Option<Vec<u8>> {
        let unit = crate::vobsub::unit(drawn, self.ink.as_mut()?);
        if unit.is_none() {
            self.refused += 1;
        }
        unit
    }

    /// Take one unit, shown at `at` on the output's clock.
    fn take(&mut self, id: i32, at: f64, unit: &[u8]) {
        self.side.add(id as u8, at, unit);
        let stops = crate::vobsub::stops_after(unit);
        self.standing.retain(|(other, _, _)| *other != id);
        self.standing.push((id, at, stops));
    }

    /// End every display still standing at `at`.
    ///
    /// A unit that has already taken itself down by then is left alone: the
    /// disc said when it ends, and it ended.
    fn end_range(&mut self, at: f64) {
        // Within one tick of the clock a unit counts its delay in, which is
        // eleven milliseconds: a stop written to land exactly here -- which
        // is what the set that clears a plane at a range's end writes -- can
        // come back a fraction the other side of it, and ending it again
        // would be a unit saying what the one before it just said.
        let over = |&(_, shown, stops): &(i32, f64, Option<f64>)| {
            stops.is_some_and(|after| shown + after <= at + crate::vobsub::TICK)
        };
        let standing: Vec<i32> = self
            .standing
            .iter()
            .filter(|s| !over(s))
            .map(|&(id, _, _)| id)
            .collect();
        for id in standing {
            self.side.add(id as u8, at, &crate::vobsub::take_down());
        }
        self.standing.clear();
    }
}

/// One subpicture stream on its way into the cut as a Blu-ray's kind of
/// subtitle.
///
/// The reader takes the picture out of a DVD's unit and the composer decides
/// what display sets say it: see [`crate::vobsub::Reader`] and
/// [`crate::pgs::write::Composer`]. What comes out goes down the same track
/// a Blu-ray's own graphics would have gone down.
struct Converted {
    /// Which of the writer's graphics tracks this is written on.
    track: usize,
    /// The substream id the disc knew it by, which is how packets are
    /// matched to it.
    id: i32,
    screen: (u16, u16),
    reader: crate::vobsub::Reader,
    composer: crate::pgs::write::Composer,
    /// How many subtitles were converted, for the note that says so.
    shown: usize,
}

/// What one stream's `.sup` is called beside the cut, or `None` where it is
/// the only one and takes the cut's own name.
///
/// A pair holds as many streams as the disc had; a `.sup` holds one. So where
/// there are several, each takes the language the disc says it is in --
/// `cut_title.eng.sup` -- and the number it sits on where the disc says
/// nothing, or says the same thing for two of them.
fn sup_tag(languages: &[Option<String>], k: usize, id: i32) -> Option<String> {
    if languages.len() < 2 {
        return None;
    }
    let mine = languages.get(k).and_then(|l| l.as_deref());
    let alone = mine.is_some_and(|l| {
        languages
            .iter()
            .filter(|other| other.as_deref() == Some(l))
            .count()
            == 1
    });
    Some(match (alone, mine) {
        (true, Some(language)) => language.to_string(),
        _ => format!("{id:04x}"),
    })
}

/// Where a graphics track goes when it does not go into the cut.
///
/// It hangs off the track rather than standing beside one because everything
/// that reaches a track is a display set -- whether the recording sent it,
/// the cut wrote it to mend a range's end, or a DVD's unit was converted into
/// it -- and all of it has to go the same way.
///
/// Two of them, and the difference is how much is converted. **Redrawn**
/// takes the picture out and writes it as the kind a DVD draws, for the pair
/// beside the cut. **Sup** writes the display sets down as they are. See
/// [`Subtitles`].
enum Aside {
    Redrawn(Redrawn),
    Sup(crate::pgs::Sup),
}

/// One graphics stream on its way *out* of the cut, as a DVD's kind of
/// subtitle.
///
/// The mirror of [`Converted`]. See [`crate::pgs::read`] and
/// [`crate::vobsub::unit`].
struct Redrawn {
    /// The substream id the pair knows it by. A DVD's own subtitles are
    /// numbered 0x20 upwards and there is no other convention to follow, so
    /// a Blu-ray's are numbered into the same run.
    id: i32,
    reader: crate::pgs::read::Reader,
    /// The set being gathered, end to end. A display set arrives a packet at
    /// a time and means nothing until its END: the decoder is given the whole
    /// of one, which is how it is given the composition, the palette and the
    /// picture together.
    building: Vec<u8>,
    /// When that set puts its subtitle up, on the output's own clock.
    at: Option<f64>,
    /// The unit last drawn and when it went up, held back until the set that
    /// ends it arrives.
    ///
    /// Held because a display set does not say how long its subtitle stands
    /// -- what takes it down is a later set -- and a unit can say, in the
    /// sequence that stops it. Waiting one set is what turns the one into the
    /// other. Left standing instead, the pair is still right on a player that
    /// follows a DVD's own rule, and is a subtitle of no duration at all to
    /// everything built on libavcodec.
    pending: Option<(f64, Vec<u8>)>,
}

/// One graphics track being written, and what it has on screen.
///
/// The plane is here rather than beside the read because it is a fact about
/// the whole cut and not about one segment of it: what a range has to put up
/// when it opens was read before the range began, and what it has to take
/// down when it ends was read inside it.
struct GraphicsTrack {
    out_index: usize,
    out_tb: f64,
    in_index: usize,
    in_tb: f64,
    /// What to call it where a person will read about it.
    pid: i32,
    /// Packets carried across as they arrived.
    written: i64,
    /// Packets written that the recording never sent here: a subtitle put up
    /// again at a range's opening, or taken down at its end. Counted apart
    /// because they are this program's own words rather than the disc's.
    mended: i64,
    plane: crate::pgs::Plane,
    /// The decode time given to the last packet written. Two pieces of one
    /// display set are microseconds apart and the output counts in 90 kHz,
    /// so they can land on the same tick honestly -- and a muxer refuses a
    /// packet that does not come after the one before it.
    last_out: Option<i64>,
    /// Set where this track goes beside the cut instead of into it, which is
    /// the one case where `out_index` names no stream. See [`Aside`].
    aside: Option<Aside>,
}

impl Writer {
    /// Queue a picture. Writing is delayed by the reorder depth so that its
    /// DTS can be taken from the display order rather than the decode order.
    ///
    /// DTS has to be the display timeline shifted back, not a running sum of
    /// each picture's own length: under pulldown the lengths differ, and a
    /// decode-order sum overtakes the presentation time it is supposed to
    /// precede. The muxer rejects that outright -- `pts < dts`.
    fn push(&mut self, e: Emitted) -> Result<()> {
        self.seen.push(std::cmp::Reverse(e.display));
        self.pending.push_back(e);
        // **The queue is measured in halves of a frame, not in pictures.**
        // The reorder depth is the source's own and counts frames, and for
        // as long as a picture is a whole frame the two are the same number.
        // A recording coded as field pairs hands over two pictures per frame,
        // and a queue of one picture then settles a P frame's decode time
        // before the B frames that display ahead of it have arrived -- which
        // leaves every one of those with nowhere to go. That was half the
        // pictures of a recorder's own disc, and the cut it wrote was blocky
        // mush from the first copied picture on.
        //
        // Counted in halves rather than scaled by what the recording is,
        // because a recording is not all one thing: a broadcast in the
        // sample here is frame-coded but for seven field pairs in half a
        // minute, and only those seven want the extra room. A picture shown
        // for *three* fields is still one frame and still counts two, so
        // pulldown does not move the boundary either.
        while self.held_halves() > (self.depth * 2).max(0) {
            self.emit_one()?;
        }
        Ok(())
    }

    /// Halves of a frame the queue is holding. See [`Writer::push`].
    fn held_halves(&self) -> i64 {
        self.pending
            .iter()
            .map(|e| if e.fields < 2 * self.sub { 1 } else { 2 })
            .sum()
    }

    fn emit_one(&mut self) -> Result<()> {
        let Some(mut e) = self.pending.pop_front() else {
            return Ok(());
        };
        let Some(std::cmp::Reverse(next_in_display)) = self.seen.pop() else {
            return Ok(());
        };
        // Three fields of lead-in per level of reordering, and then the
        // picture's own display position as a ceiling.
        //
        // The lead-in alone does not settle it. Three fields is generous
        // while the pictures sit two fields apart and is nothing else: a
        // recording with holes in it puts them further apart -- the pictures
        // inside a damaged stretch never decode, so their display positions
        // are simply missing -- and so does a stream that shows some pictures
        // for three fields. There the derived time overtakes the picture it
        // belongs to, the muxer refuses `pts < dts` outright, and the whole
        // cut stopped over it. A picture is never decoded after it is shown,
        // so that is the ceiling.
        let mut dts = (next_in_display - self.depth * 3 * self.sub).min(e.display);
        // And never behind the picture before it. The value above is the
        // smallest display position still waiting, which climbs while the
        // pictures arrive in one of the orders a coder produces and does not
        // where a damaged stretch hands them over out of any order at all.
        if let Some(prev) = self.last_dts {
            dts = dts.max(prev + 1);
        }
        // Where the two cannot both be met the picture has nowhere to go: its
        // display position is behind one already written, which is what a
        // damaged stretch does to the order pictures arrive in. Left out
        // rather than allowed to stop the cut -- the muxer would refuse it,
        // and the stretch it belongs to is the damage.
        if dts > e.display {
            self.skipped += 1;
            return Ok(());
        }
        self.last_dts = Some(dts);
        e.packet.set_stream(0);
        e.packet.set_pts(Some(e.display * self.field_ticks));
        e.packet.set_dts(Some(dts * self.field_ticks));
        e.packet.set_duration(e.fields * self.field_ticks);
        e.packet.set_position(-1);
        e.packet.rescale_ts(self.our_tb, self.out_tb);
        // The muxer refuses a picture whose timestamps it cannot place and
        // says only "Invalid argument" about it, the same way it does for
        // sound. Which picture, and what was wrong with it, is the whole of
        // what makes that answerable -- and on a recording with holes in it
        // the answer is usually the holes.
        let (pts, dts) = (e.packet.pts().unwrap_or(0), e.packet.dts().unwrap_or(0));
        e.packet
            .write_interleaved(&mut self.octx)
            .with_context(|| {
                format!(
                    "writing picture {} of the output (display {}, pts {pts}, dts {dts})",
                    self.written + 1,
                    e.display
                )
            })?;
        self.written += 1;
        if e.fields < 2 * self.sub {
            self.halves += 1;
        }
        if let Some(report) = &self.progress {
            if self.expected > 0 && self.written % 16 == 0 {
                let done = self.written as f64 - self.halves as f64 / 2.0;
                report((done / self.expected as f64).min(1.0));
            }
        }
        Ok(())
    }

    fn push_audio_encoded(&mut self, track: usize, mut packet: ff::Packet, pts: i64) -> Result<()> {
        let Some(t) = self.audio.get(track) else {
            return Ok(());
        };
        let (index, tb, rate) = (t.out_index, t.out_tb, t.out_rate);
        packet.set_stream(index);
        // `pts` counts samples, because that is the encoder's own clock. The
        // container keeps time in whatever it likes -- MP4 happens to use the
        // sample rate, which hid this for a long time, while MPEG-TS insists
        // on 90 kHz and turned a 799-second track into a 426-second one.
        let at = ((pts as f64 / rate.max(1) as f64) / tb).round() as i64;
        packet.set_pts(Some(at));
        packet.set_dts(Some(at));
        packet.set_position(-1);
        packet.write_interleaved(&mut self.octx)?;
        self.audio[track].written += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        while !self.pending.is_empty() {
            self.emit_one()?;
        }
        Ok(())
    }

    fn push_audio(
        &mut self,
        track: usize,
        mut packet: ff::Packet,
        out_start: f64,
        out_dur: f64,
    ) -> Result<()> {
        let Some(t) = self.audio.get(track) else {
            return Ok(());
        };
        let (index, tb) = (t.out_index, t.out_tb);
        // A frame with no length is a frame there is nowhere to put: the
        // output lays its sound end to end, and a length of nought would
        // leave the next frame where this one is. Every track whose packets
        // carry their own durations is past this in the ordinary way, and
        // the ones that do not have a length measured for them before the
        // cut starts -- see [`assumed_frame`]. What reaches here is a track
        // that has neither, and it is counted rather than passed over: a
        // cut that wrote no sound at all used to say nothing about it.
        if out_dur <= 0.0 {
            self.audio[track].no_length += 1;
            return Ok(());
        }
        packet.set_stream(index);
        // Where this frame goes. The instant the recording gives it, except
        // on a track whose frames had to have their length measured: there
        // the timestamps are the container's own and are kept at the
        // container's resolution, which is coarser than the frames are.
        // Matroska counts in milliseconds and a TrueHD frame is 1/1200 of a
        // second, so one frame in six carries the timestamp of the frame
        // before it and would be written where that one already is. Such a
        // track is laid from where it has reached instead -- end to end,
        // which is how its sound is played back anyway -- and the
        // recording's own instant is still what carries it forward at the
        // start of a kept range, where it is the later of the two.
        let out_start = match self.audio[track].end {
            Some(end) if self.audio[track].frame_secs > 0.0 => out_start.max(end),
            _ => out_start,
        };
        let pts = (out_start.max(0.0) / tb).round() as i64;
        // A frame that does not come after the last one written is a frame
        // the muxer refuses, and refusing it stopped the whole cut. It
        // happens where a recording is damaged: the timestamps inside a burst
        // of noise jump about, and two frames arrive claiming the same
        // instant or an earlier one. The frame is left out instead. The sound
        // in that stretch is lost either way -- it is the damage -- and how
        // much was left out is said when the cut finishes.
        //
        // Two frames at the *same* instant are a different matter, and only
        // a transport stream minds them. They are what a container keeping
        // time more coarsely than its frames are long hands over: Matroska
        // counts in milliseconds, a TrueHD frame is 1/1200 of a second, and
        // a sixth of them round onto the instant before. Written back into
        // a Matroska file they go where they came from and nothing is lost;
        // left out, a sixth of the sound would be.
        let clashes = self.audio[track]
            .last_out
            .is_some_and(|last| if self.into_ts { pts <= last } else { pts < last });
        if clashes {
            self.audio[track].dropped += 1;
            return Ok(());
        }
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_duration((out_dur / tb).round() as i64);
        packet.set_position(-1);
        // The muxer refuses a timestamp that does not follow the last one it
        // was given, and says only "Invalid argument" about it. Which track
        // and which instant is the whole of what makes that answerable.
        let named = {
            let t = &self.audio[track].info;
            crate::track_name(self.on_a_ts, t.pid, t.stream_index)
        };
        packet
            .write_interleaved(&mut self.octx)
            .with_context(|| format!("writing sound on {named} at {out_start:.4}s (pts {pts})"))?;
        let t = &mut self.audio[track];
        t.written += 1;
        t.last_out = Some(pts);
        t.end = Some(t.end.unwrap_or(out_start.max(0.0)) + out_dur);
        Ok(())
    }

    /// Write one caption statement at the output time it now belongs at.
    ///
    /// No duration is set. A caption is displayed until the next statement
    /// replaces or clears it, which is a property of the stream and not of
    /// the packet, and a duration invented here would only be a claim the
    /// muxer then has to reconcile with the next packet's timestamp.
    fn push_caption(&mut self, track: usize, mut packet: ff::Packet, at: f64) -> Result<()> {
        let Some(t) = self.captions.get(track) else {
            return Ok(());
        };
        let (index, tb, last) = (t.out_index, t.out_tb, t.last_out);
        packet.set_stream(index);
        // Nudged rather than dropped, which is what a display set gets and
        // for the same reason: a statement is a line of the programme, and a
        // tick of 90 kHz is a ninetieth of a millisecond. The sound is the
        // one that drops instead, because a frame moved off its own instant
        // is a frame in the wrong place.
        let mut pts = (at.max(0.0) / tb).round() as i64;
        if let Some(last) = last {
            pts = pts.max(last + 1);
        }
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_duration(0);
        packet.set_position(-1);
        packet.write_interleaved(&mut self.octx)?;
        let t = &mut self.captions[track];
        t.written += 1;
        t.last_out = Some(pts);
        Ok(())
    }

    /// Write one packet of a display set at the output time it now belongs
    /// at.
    ///
    /// Both times travel, where a caption's do not. A display set is decoded
    /// over the few milliseconds before it is shown and says so in its own
    /// packets -- the picture is handed over first and the composition that
    /// puts it up last -- and a decoder given that spacing has the subtitle
    /// ready at the instant it is called for.
    ///
    /// `mended` is for the packets this program wrote rather than carried:
    /// the set put up again at a range's opening and the one that takes it
    /// down at the end.
    fn push_graphics(
        &mut self,
        track: usize,
        held: &crate::pgs::Held,
        offset: f64,
        mended: bool,
    ) -> Result<()> {
        let Some(t) = self.graphics.get(track) else {
            return Ok(());
        };
        // A track going beside the cut has no stream to be written to. What
        // reaches it is the same display set either way -- carried, replayed
        // at a range's opening, or written to clear one at its end -- so the
        // turning aside is here, where all three meet.
        if t.aside.is_some() {
            return self.push_aside(track, held, offset, mended);
        }
        let (index, tb, last) = (t.out_index, t.out_tb, t.last_out);
        let mut dts = ((held.dts + offset).max(0.0) / tb).round() as i64;
        // Nudged rather than dropped: half a display set is not a subtitle,
        // and a tick of 90 kHz is a ninetieth of a millisecond.
        if let Some(last) = last {
            dts = dts.max(last + 1);
        }
        let pts = (((held.pts + offset).max(0.0) / tb).round() as i64).max(dts);
        let mut packet = ff::Packet::copy(&held.data);
        packet.set_stream(index);
        packet.set_pts(Some(pts));
        packet.set_dts(Some(dts));
        packet.set_duration(0);
        packet.set_position(-1);
        packet.write_interleaved(&mut self.octx)?;
        let t = &mut self.graphics[track];
        t.last_out = Some(dts);
        if mended {
            t.mended += 1;
        } else {
            t.written += 1;
        }
        Ok(())
    }

    /// Take one packet of a display set for a track that goes beside the cut
    /// rather than into it.
    fn push_aside(
        &mut self,
        track: usize,
        held: &crate::pgs::Held,
        offset: f64,
        mended: bool,
    ) -> Result<()> {
        let t = &mut self.graphics[track];
        if mended {
            t.mended += 1;
        } else {
            t.written += 1;
        }
        match t.aside.as_mut() {
            None => Ok(()),
            // The display sets as they are. Nothing to gather and nothing to
            // decide: a `.sup` is the packets with their times in front of
            // them, which is what is in hand.
            Some(Aside::Sup(sup)) => {
                sup.take(held, offset);
                Ok(())
            }
            Some(Aside::Redrawn(_)) => self.push_redrawn(track, held, offset),
        }
    }

    /// The same, for a track being written back into the units a DVD draws.
    ///
    /// Gathered until the END that finishes the set, then read back into the
    /// picture it draws and drawn again as a DVD's kind of unit. A set that
    /// draws nothing is the one that clears the plane, and beside the cut
    /// that is what fills in how long the last subtitle stood.
    fn push_redrawn(&mut self, track: usize, held: &crate::pgs::Held, offset: f64) -> Result<()> {
        let opens = crate::pgs::segments(&held.data).any(|(kind, _)| kind == crate::pgs::PCS);
        let ends = crate::pgs::segments(&held.data).any(|(kind, _)| kind == crate::pgs::END);
        let Some(Aside::Redrawn(aside)) = self.graphics[track].aside.as_mut() else {
            return Ok(());
        };
        aside.building.extend_from_slice(&held.data);
        // The composition is what says when the subtitle appears; the pieces
        // around it carry the times they are handed over at, which are a
        // decoder's business and not a subtitle's.
        if opens || aside.at.is_none() {
            aside.at = Some((held.pts + offset).max(0.0));
        }
        if !ends {
            return Ok(());
        }
        let set = std::mem::take(&mut aside.building);
        let at = aside.at.take().unwrap_or(0.0);
        let id = aside.id;
        let drawn = aside.reader.read(&set)?;
        let pending = aside.pending.take();
        let Some(subs) = self.subpictures.as_mut() else {
            return Ok(());
        };
        // Whatever was on screen ends where this set begins -- it is either
        // replaced by it or cleared by it -- and now that that is known the
        // unit can be written saying so itself.
        if let Some((shown, unit)) = pending {
            subs.take(id, shown, &crate::vobsub::stopped_after(&unit, at - shown));
        }
        // And this one waits for the same answer. A set that draws nothing
        // is the one that cleared the screen, and it has now been spent
        // saying when the last subtitle went: there is nothing else it has
        // to put in the pair.
        let Some(drawn) = drawn else {
            return Ok(());
        };
        let Some(unit) = subs.drew(&drawn) else {
            return Ok(());
        };
        if let Some(Aside::Redrawn(aside)) = self.graphics[track].aside.as_mut() {
            aside.pending = Some((at, unit));
        }
        Ok(())
    }
}

/// Emit an audio packet if it belongs to this segment's stretch of time.
///
/// A packet is claimed by whichever segment contains the instant it starts,
/// so the segments tile the range without dropping or duplicating anything.
/// The range's own edges are the exception: the frame straddling the start
/// belongs to the first segment even though it begins earlier, and the frame
/// straddling the end overruns. What becomes of those two is what the audio
/// mode decides -- rounded away, re-encoded in place, or decoded into a track
/// that is re-encoded whole.
fn take_audio(
    audio: &AudioCtx,
    src: &Source,
    seg: &Segment,
    first_segment: bool,
    packet: ff::Packet,
    writer: &mut Writer,
) -> Result<bool> {
    let Some(pts) = packet.pts() else {
        return Ok(false);
    };
    let t = pts as f64 * audio.in_tb - src.start_time;
    // The packet's own length, or -- where it has none -- the one measured
    // off the spacing of this track's timestamps before the cut started.
    // Matroska stores a TrueHD track's access units without durations, each
    // being 1/1200 of a second and the container leaving that to be worked
    // out from the timestamps; every frame of such a track was being left
    // out of the cut, which wrote a file declaring sound it did not contain.
    // See [`assumed_frame`].
    let dur = match packet.duration() {
        own if own > 0 => own as f64 * audio.in_tb,
        _ => writer.audio[audio.track].frame_secs,
    };
    let past_end = t >= seg.end;

    if audio.mode == AudioMode::Reencode {
        // Claim exclusively, exactly as the copy path does, or a frame lying
        // across an internal seam is fed by both neighbours and the whole
        // track drifts a frame later each time. The opening segment also
        // takes the frame straddling the range's start; the sample window
        // trims whatever of it belongs to the material before the cut.
        let claimed = if first_segment {
            t + dur > audio.range_in && t < seg.end
        } else {
            t >= seg.start && t < seg.end
        };
        if claimed {
            let mut out = Vec::new();
            let AudioTrack {
                info, reencoder, ..
            } = &mut writer.audio[audio.track];
            if let Some(re) = reencoder.as_mut() {
                re.take(&packet, info, src.start_time, audio.window, audio.fades)?;
                re.drain(&mut out)?;
            }
            for (p, pts) in out {
                writer.push_audio_encoded(audio.track, p, pts)?;
            }
        }
        return Ok(past_end);
    }

    // Open on whichever frame sits nearest the chosen start, so the error is
    // at most half a frame either way rather than a whole frame late. Later
    // segments claim strictly by start time, or the frame straddling an
    // internal seam would be emitted twice.
    //
    // Smart mode does not get to be cleverer here, and the reason is worth
    // recording. Opening on the frame the boundary falls *inside* would lose
    // nothing at all -- but that frame begins before the boundary, so it
    // reaches back into the range before it, where the previous range's own
    // overrunning last frame already is. Two frames cannot share an instant:
    // an MP4 lays its samples end to end, and MPEG-TS rejects a timestamp
    // that goes backwards outright. Keeping a whole number of frames per
    // range and centring the error is the only arrangement that neither
    // overlaps nor accumulates, so it is what every mode uses.
    let claimed = if first_segment {
        t + dur / 2.0 > audio.pick_from && t >= audio.min_start
    } else {
        t >= seg.start
    };
    if !claimed || past_end {
        return Ok(past_end);
    }
    // Trimming the frame that straddles a boundary was tried with
    // AV_PKT_DATA_SKIP_SAMPLES; the MP4 muxer does not act on it, and the
    // skipped samples came back through intact. So a boundary either snaps to
    // a whole audio frame, or -- in smart mode -- the frame it lands inside
    // was re-encoded beforehand with the far side faded out, and stands here
    // in place of the recording's own.
    if writer.audio[audio.track].need_sync {
        if !opens_a_truehd_track(packet.data()) {
            // The frame is not written, but the instant it occupied is still
            // spent: what follows belongs where the recording had it, not a
            // frame earlier. Counting it keeps `end` on the output clock, so
            // the next range measures its drift against where the sound
            // actually reaches -- and, in a transport stream, so the muxer is
            // never handed a timestamp behind the one before it.
            let track = &mut writer.audio[audio.track];
            track.end = Some(track.end.unwrap_or((t + audio.offset).max(0.0)) + dur);
            return Ok(past_end);
        }
        writer.audio[audio.track].need_sync = false;
    }
    let track = &writer.audio[audio.track];
    let patch = track
        .patches
        .get(&pts)
        .filter(|p| p.after.is_none() || p.after == track.prev);
    let packet = match patch {
        Some(p) => {
            let mut patched = ff::Packet::copy(&p.bytes);
            patched.set_flags(ff::packet::Flags::KEY);
            patched.set_duration(packet.duration());
            patched
        }
        None => packet,
    };
    writer.audio[audio.track].prev = Some(pts);
    writer.push_audio(audio.track, packet, t + audio.offset, dur)?;
    Ok(past_end)
}

/// Whether a TrueHD packet is one a track may open on.
///
/// TrueHD carries its format in a *major sync* that recurs through the
/// stream -- every 16 access units in the first streams measured here, and
/// about every 106 ms on the Blu-ray this was measured against later, which
/// is the figure to plan around. Everything between two of them is read
/// against the last one, so a track opening anywhere else is a track whose
/// first frames say nothing about themselves.
///
/// **This is why every kept range waits, not only the first.** A transport
/// stream is forgiving about the file's own opening -- it declares the track
/// in its programme map, and a decoder joining mid-stream waits for the next
/// sync, which is what a decoder joining a broadcast does anyway. A seam is
/// not that. There the decoder is already reading, and a frame from another
/// part of the recording is read against the header it happens to be holding:
/// the restart header stops matching, the matrix count runs past what the
/// format allows, and what comes out is noise or nothing. Measured on a
/// two-track Blu-ray, one seam was worth 79 complaints from the decoder and
/// 76 ms of sound that never arrived. An MP4 cares about the opening as well
/// -- it builds the track's `dmlp` box out of the first packet it is handed,
/// and refuses the file outright when that packet has no sync in it.
///
/// The price either way is up to one sync interval of silence at each seam,
/// which is the smallest price available while the cut lands where the
/// pictures say. Landing it on a sync instead is a question for the planner.
fn opens_a_truehd_track(data: Option<&[u8]>) -> bool {
    data.is_some_and(|d| d.len() >= 8 && d[4..8] == [0xF8, 0x72, 0x6F, 0xBA])
}

/// Emit a caption statement if it falls inside this segment's stretch.
///
/// Returns whether the segment's end has been passed, so the reader can stop
/// once every stream it is gathering has run out.
///
/// A statement carried across a cut is not always the whole story. What is
/// on screen at the moment a range ends stays on screen into the next one,
/// because what would have cleared it lives in the material that was
/// removed. In practice the case this tool exists for does not run into it:
/// a broadcaster who marks a commercial junction marks it by clearing the
/// caption plane, so the statement that opens the next range is the clear
/// itself. Where the marks are absent, a caption can outlive its scene by a
/// line.
fn take_caption(
    caption: &CaptionCtx,
    src: &Source,
    seg: &Segment,
    first_segment: bool,
    packet: ff::Packet,
    writer: &mut Writer,
) -> Result<bool> {
    if caption.ttml {
        return take_ttml(caption, seg, first_segment, packet, writer);
    }
    let Some(pts) = packet.pts() else {
        return Ok(false);
    };
    let t = pts as f64 * caption.in_tb - src.start_time;
    if t >= seg.end {
        return Ok(true);
    }
    if t >= seg.start {
        writer.push_caption(caption.track, packet, t + caption.offset)?;
    }
    Ok(false)
}

/// Take one document of a 4K recording's subtitles.
///
/// **The times are in the document and the packet has none**, so none of
/// what [`take_caption`] does applies: a recorder stamps these packets with
/// a counter, and the document says `begin` and `end` on the clip's own
/// presentation clock. See [`crate::ttml`].
///
/// So a document is claimed by the segment its beginning falls in -- or, for
/// one already on screen when the range opened, by the range's first segment
/// -- and what is written is the same document with its times moved onto the
/// output's clock and clipped to the range. A caption that would have run
/// past the end of a range ends with it; a caption that belongs to no kept
/// range is not carried at all.
fn take_ttml(
    caption: &CaptionCtx,
    seg: &Segment,
    first_segment: bool,
    packet: ff::Packet,
    writer: &mut Writer,
) -> Result<bool> {
    let Some(data) = packet.data() else {
        return Ok(false);
    };
    let Some((begin, end)) = crate::ttml::cue(data) else {
        return Ok(false);
    };
    let (begin, end) = (begin + caption.base, end + caption.base);
    // Documents arrive in the order they are shown, so one that begins past
    // this segment is the end of what this segment can hold.
    if begin >= seg.end {
        return Ok(true);
    }
    let claimed = begin >= seg.start || (first_segment && end > caption.range.0);
    if !claimed {
        return Ok(false);
    }
    // The document counts from the clip's presentation start and the cut
    // counts from the file's beginning, so both the window the document is
    // clipped to and the move it is given are put on the document's own
    // clock. What comes out is counted from the first picture of the cut,
    // which is where a file with no playlist in front of it begins.
    let window = (caption.range.0 - caption.base, caption.range.1 - caption.base);
    let Some(bytes) = crate::ttml::retimed(data, caption.base + caption.offset, window) else {
        return Ok(false);
    };
    let at = begin.max(caption.range.0) + caption.offset;
    let mut out = ff::Packet::copy(&bytes);
    out.set_flags(ff::packet::Flags::KEY);
    writer.push_caption(caption.track, out, at)?;
    Ok(false)
}

/// Take one packet of a graphics stream: give it to the plane, and carry the
/// display set it completes if the whole of that set falls inside this
/// segment's stretch.
///
/// Returns whether the segment's end has been passed, as [`take_caption`]
/// does.
///
/// Every packet is fed to the plane, carried or not. The ones before the
/// segment begins are how the plane knows which subtitle is on screen when
/// it does -- which is the question the opening of a range turns on -- and
/// the ones inside it leave the plane knowing what is on screen at the end.
///
/// A set is carried whole or not at all. One that straddles the end of a
/// range is simply not carried: its pieces would arrive without the
/// composition that gives them meaning, and what it would have shown is put
/// up by the next range's opening instead.
fn take_graphics(
    graphics: &GraphicsCtx,
    src: &Source,
    seg: &Segment,
    packet: ff::Packet,
    writer: &mut Writer,
) -> Result<bool> {
    let Some(pts) = packet.pts() else {
        return Ok(false);
    };
    let at = |stamp: i64| stamp as f64 * graphics.in_tb - src.start_time;
    let t = at(pts);
    let dts = packet.dts().map_or(t, at);
    // When this packet *arrives*, which is the earlier of the two times it
    // carries and not the one it is shown at. A display set is sent a
    // fraction of a second before it appears -- the picture first, the
    // composition that puts it up last -- so the first packet of a set is
    // the one whose presentation time is furthest ahead. Judged by that, a
    // reader stops on the packet that opens the last set it should have
    // carried, and the set is lost between two segments: this one never
    // finished it, and the next one starts inside it.
    let arrives = t.min(dts);
    let plane = &mut writer.graphics[graphics.track].plane;
    // Past the end, and not in the middle of anything: the segment is done
    // with this stream. A set that *is* part-read runs on past the end
    // instead, because this read is the only one that will ever hold all of
    // it -- the next segment starts inside the set, misses the composition,
    // and throws the rest away. The bound is there for a recording that
    // sends a composition and never ends it.
    if arrives >= seg.end && (!plane.building() || arrives >= seg.end + TRAIL) {
        return Ok(true);
    }
    let held = crate::pgs::Held {
        pts: t,
        dts,
        data: packet.data().unwrap_or(&[]).to_vec(),
    };
    let set = plane.feed(held);
    if let Some(set) = set {
        // Where the set began is where it is claimed: one that opened before
        // this segment did belongs to whatever came before, and one that
        // opened inside it belongs here however far past the end it runs.
        let opened = set
            .iter()
            .map(|h| h.pts.min(h.dts))
            .fold(f64::INFINITY, f64::min);
        if (seg.start..seg.end).contains(&opened) {
            for held in &set {
                writer.push_graphics(graphics.track, held, graphics.offset, false)?;
            }
        }
    }
    // Done only once the set in hand is whole. Saying so a packet early is
    // how the reader stops in the middle of one.
    Ok(arrives >= seg.end && !writer.graphics[graphics.track].plane.building())
}

fn open_input(path: &str) -> Result<(crate::input::Demux, usize)> {
    let ictx = crate::input::demux(&path)?;
    let index = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| anyhow!("no video stream"))?
        .index();
    Ok((ictx, index))
}

/// Seek so that the next read is safely *before* `time` (rebased seconds).
///
/// The margin matters: MPEG-TS seeking is byte-position based and only
/// approximates timestamps, so asking for exactly the target can land past
/// it. Reading a few extra GOPs forward is cheap; overshooting is not
/// recoverable.
/// Write the audio of a finished cut out on its own, as an elementary stream.
///
/// The chain a broadcast recording usually goes down -- index, encode the
/// video, mux the two back together -- wants the audio as a bare ADTS file,
/// and the demuxers that produce one are a weak link: one was seen writing a
/// header claiming Main profile at 88.2 kHz with no channels, from a stream
/// that plainly says LC, 48 kHz, stereo. Writing it here removes that step.
///
/// Reads back what was just written rather than cutting again, so the file
/// beside the video is by construction the audio that is *in* the video.
///
/// Frames that arrive framed already are written as they are, headers and
/// all, so an MPEG-2 AAC recording stays MPEG-2 AAC without anything being
/// said. `aac` only reaches the frames this has to frame itself, which is
/// what audio taken back out of an MP4 amounts to.
pub fn write_audio_es(cut: &str, output: &str, aac: AacVersion) -> Result<usize> {
    let done = write_one_track_out(cut, output, aac);
    if done.is_err() {
        // What a refused stream leaves behind is a nought-byte file with an
        // `.aac` on the end of it, which reads as an AAC stream and is not
        // one. Worse than no file at all, and this is the only place that
        // knows it is not wanted.
        let _ = std::fs::remove_file(output);
    }
    done
}

fn write_one_track_out(cut: &str, output: &str, aac: AacVersion) -> Result<usize> {
    crate::init()?;
    let mut ictx = crate::input::demux(&cut)?;
    let ist = ictx
        .streams()
        .best(ff::media::Type::Audio)
        .ok_or_else(|| anyhow!("{cut} has no audio to write out"))?;
    let index = ist.index();
    let params = ist.parameters();
    let in_tb = ist.time_base();

    let mut octx = ff::format::output(&output)?;
    {
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(params);
        ost.set_time_base(in_tb);
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    let mut muxer_opts = ff::Dictionary::new();
    if octx.format().name().contains("adts") && aac == AacVersion::Mpeg2 {
        muxer_opts.set("write_mpeg2", "1");
    }
    octx.write_header_with(muxer_opts)?;
    let out_tb = octx
        .stream(0)
        .ok_or_else(|| anyhow!("no output stream"))?
        .time_base();

    let mut written = 0usize;
    for (stream, mut packet) in ictx.packets() {
        if stream.index() != index {
            continue;
        }
        packet.rescale_ts(in_tb, out_tb);
        packet.set_stream(0);
        packet.set_position(-1);
        packet.write(&mut octx)?;
        written += 1;
    }
    octx.write_trailer()?;
    Ok(written)
}

/// The PIDs and service number a transport stream was carrying.
struct TsLayout {
    pmt_pid: i32,
    first_pid: i32,
    service_id: i32,
}

/// Whether the output is Blu-ray's own shape of transport stream.
///
/// The two differ in the framing -- a `.m2ts` puts four bytes of arrival
/// time in front of every packet -- and in the numbering: see [`Pids`].
fn writing_m2ts(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m2ts"))
}

/// Whether the output is a transport stream, in either of the two shapes one
/// is written in.
///
/// Asked for a `.m2ts`, libavformat writes Blu-ray's own framing: the same
/// packets with four bytes of arrival time in front of each. The recording's
/// own tables go into that as they go into a `.ts` -- the pass that puts
/// them back reads whichever framing it finds -- which is what lets a cut be
/// written straight onto a disc. See [`crate::bdav`].
fn writing_ts(path: &str) -> bool {
    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ts" | "m2ts" | "mts" | "m2t")
    )
}

/// Which account of itself a cut carries where the caller has not said.
///
/// There are two honest answers and the file name picks between them,
/// because what is being written decides which is right.
///
/// A Blu-ray clip is a partial transport stream. That is not a preference;
/// it is what the format is, and it is what a recorder's own disc and every
/// authoring tool's disc carries -- one selection information table and
/// none of the tables that describe a live multiplex. So a `.m2ts` -- the
/// standalone one as much as the one being written onto a disc -- gets
/// [`crate::si::Tables::Partial`]. It is the same test the framing is
/// chosen by ([`writing_m2ts`]), because it is the same question: this is
/// the shape a disc's stream is written in.
///
/// Everything else is a file, and the software that opens one reads the
/// broadcast's own tables. A player shows the programme name, the station
/// and the clock out of EIT, SDT and TOT, on the PIDs a broadcast puts them
/// on; handed a cut that says all of it in a SIT instead, it finds nothing
/// and shows nothing. The standards' answer to "what is a recording" is the
/// partial stream, but nothing downstream of here asks that question -- so a
/// `.ts` gets [`crate::si::Tables::Broadcast`], and `--tables partial` is
/// how to ask for the other one anyway.
///
/// Anything that is not a transport stream at all ignores the answer.
pub fn tables_for(output: &str, asked: Option<crate::si::Tables>) -> crate::si::Tables {
    asked.unwrap_or(if writing_m2ts(output) {
        crate::si::Tables::Partial
    } else {
        crate::si::Tables::Broadcast
    })
}

/// Whether a cut written here can hold the recording's data broadcast.
///
/// Three things have to be true together, and this is the one place that
/// says so -- the run that writes the cut and the run that tells the user
/// what is going into it both ask here, because a summary that promises a
/// carousel the cut does not carry is worse than no summary.
///
/// It has to be a transport stream, since nothing else has a PID to put one
/// on. Its tables have to be the recording's own, since the carousel is
/// written by the pass that puts those back and by nothing else. And it must
/// not be Blu-ray's own framing: a disc's stream carries pictures, sound and
/// subtitles, and neither a clip index nor a player has anywhere to put the
/// pages behind the d button. See [`crate::carousel`].
pub fn can_carry_data_broadcast(output: &str, tables: Option<crate::si::Tables>) -> bool {
    writing_ts(output)
        && !writing_m2ts(output)
        && tables_for(output, tables) != crate::si::Tables::Muxer
}

/// Read the layout off the input, when the input is a transport stream at all.
///
/// The video's own PID is the starting point, not the lowest PID in the file:
/// a broadcast recording carries service tables as streams too, and their
/// PIDs sit below the range a muxer will accept. The muxer numbers the
/// streams it writes from here in the order they were added, so the video
/// keeps its PID and the audio lands beside it.
fn ts_layout(ictx: &ff::format::context::Input, video_index: usize) -> Option<TsLayout> {
    // What the mpegts muxer will take; anything else is left to its defaults.
    const PID_MIN: i32 = 0x0020;
    const PID_MAX: i32 = 0x1FFA;
    unsafe {
        let ic = ictx.as_ptr();
        let iformat = (*ic).iformat;
        if iformat.is_null() || (*iformat).name.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr((*iformat).name).to_string_lossy();
        if !name.contains("mpegts") {
            return None;
        }
        let video_pid = ictx.stream(video_index).map(|s| s.id()).unwrap_or(0);
        let first_pid = if (PID_MIN..=PID_MAX).contains(&video_pid) {
            video_pid
        } else {
            0
        };
        // The service this recording is of, which is the one whose map names
        // the pictures. A recorder that keeps the multiplex's own PAT names
        // the neighbouring services too, and the first of them is as likely
        // to be one that was never recorded as the one that was -- taking it
        // would label the output with somebody else's service number.
        let programs: &[*mut ff::ffi::AVProgram] = if (*ic).nb_programs > 0 {
            std::slice::from_raw_parts((*ic).programs, (*ic).nb_programs as usize)
        } else {
            &[]
        };
        // The first is the answer only when none of them names the video,
        // which is a container that groups its streams some other way.
        let mut ours = programs.first().copied();
        for &p in programs {
            let n = (*p).nb_stream_indexes as usize;
            let in_it: &[u32] = if n > 0 {
                std::slice::from_raw_parts((*p).stream_index, n)
            } else {
                &[]
            };
            if in_it.contains(&(video_index as u32)) {
                ours = Some(p);
                break;
            }
        }
        let (mut pmt_pid, service_id) = match ours {
            Some(p) => ((*p).pmt_pid, (*p).id),
            None => (0, 0),
        };
        if !(0x0010..=PID_MAX).contains(&pmt_pid) {
            pmt_pid = 0;
        }
        // The muxer hands out PIDs in a run from `first_pid`; a PMT sitting
        // inside that run would be written over.
        if first_pid > 0 && (first_pid..first_pid + 8).contains(&pmt_pid) {
            pmt_pid = 0;
        }
        Some(TsLayout {
            pmt_pid,
            first_pid,
            service_id: if (1..=0xFFFF).contains(&service_id) {
                service_id
            } else {
                0
            },
        })
    }
}

/// The bytes of the clip one segment's pictures come out of.
///
/// **A stretch is told from the one beside it by where it is in the file, not
/// by when it plays.** Where a stretch begins is read off an index that
/// understates it at both ends -- see [`crate::disc`] -- so two stretches can
/// overlap by a fraction of a second on the joined clock, and on one recorder
/// clip measured here they overlap by 0.46 of one. A packet's time then says
/// nothing about which of the two wrote it; its position says exactly.
///
/// A segment never spans a seam, because [`crate::plan::plan_on`] cuts at
/// them, so the stretch is the one this segment's times fall in and what
/// comes back is that stretch's first and last byte. `None` either side where
/// there is no seam that way, which for an ordinary recording is both.
fn stretch_bytes(
    src: &Source,
    seg: &Segment,
    tol: f64,
) -> (Option<crate::restamp::Seam>, Option<u64>) {
    let floor = src
        .joins
        .iter()
        .copied()
        .filter(|j| j.time <= seg.start + tol)
        .next_back();
    let wall = src
        .joins
        .iter()
        .find(|j| j.time >= seg.end - tol)
        .map(|j| j.at);
    (floor, wall)
}

/// Put the read where a segment begins, and never in front of the stretch
/// that segment belongs to.
///
/// **A time on the far side of a seam cannot be seeked to as a time.**
/// libavformat searches a transport stream by reading timestamps at byte
/// positions, which works only while the times climb with the bytes. Over a
/// seam they do not: the stretches can overlap, and the search then settles
/// past the entry point it was asked for -- which the copy reads, rightly, as
/// a seek that overshot, and the cut stops. It stopped on a whole recording
/// here, every time.
///
/// So where the landing would fall in front of the seam, the seam's own byte
/// is seeked to instead. That is where the stretch begins, so the read picks
/// up ahead of the entry point with none of the stretch before it in the way.
fn seek_into(
    ictx: &mut ff::format::context::Input,
    src: &Source,
    time: f64,
    floor: Option<crate::restamp::Seam>,
) -> Result<()> {
    if let Some(seam) = floor.filter(|_| src.byte_seekable) {
        if time - src.seek_margin < seam.time {
            // Stream index -1 with AVSEEK_FLAG_BYTE means the timestamp is a
            // byte offset. See [`crate::index`], which seeks the same way.
            let placed = unsafe {
                ff::ffi::av_seek_frame(
                    ictx.as_mut_ptr(),
                    -1,
                    seam.at as i64,
                    ff::ffi::AVSEEK_FLAG_BYTE,
                ) >= 0
            };
            if placed {
                return Ok(());
            }
        }
    }
    seek_to(ictx, src, time)
}

fn seek_to(ictx: &mut ff::format::context::Input, src: &Source, time: f64) -> Result<()> {
    let landing = (time - src.seek_margin).max(0.0);
    // Asking for the beginning has to mean the beginning. Aiming at the
    // container's own start time is not the same thing: a transport stream
    // is searched by timestamp over byte positions, and the first PES it
    // finds can carry a stamp later than the one the header advertises --
    // which lands the read *after* the file's first entry point, the one
    // place there is nothing earlier to fall back to.
    let target = if landing <= 0.0 {
        i64::MIN / 2
    } else {
        ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64
    };
    ictx.seek(target, ..target).context("seek failed")?;
    Ok(())
}

/// Put one subtitle of a DVD aside, if it falls inside this segment's
/// stretch.
///
/// Nothing here is written to the output: see [`Subpictures`]. The unit is
/// whole in one packet -- libavformat joins the pieces a DVD splits it into
/// -- so unlike a Blu-ray's graphics there is no grouping to respect and no
/// straddling to think about.
fn take_subpicture(
    seg: &Segment,
    offset: f64,
    id: i32,
    in_tb: f64,
    src: &Source,
    packet: &ff::Packet,
    writer: &mut Writer,
) -> bool {
    let Some(pts) = packet.pts() else {
        return false;
    };
    let t = pts as f64 * in_tb - src.start_time;
    if t >= seg.end {
        return true;
    }
    if t < seg.start {
        return false;
    }
    let Some(data) = packet.data() else {
        return false;
    };
    if let Some(subs) = writer.subpictures.as_mut() {
        subs.take(id, t + offset, data);
    }
    if let Err(e) = convert_subpicture(id, t + offset, data, false, writer) {
        eprintln!("note: a subtitle at {:.3}s could not be converted: {e}", t);
    }
    false
}

/// Turn one of a DVD's units into the display sets that say the same thing,
/// and write them.
///
/// Nothing at all where this cut is not converting: the two destinations are
/// exclusive, and [`Subtitles::Beside`] leaves this untouched.
///
/// A unit that carries no picture is one that only takes the last one down,
/// which the composer does of its own accord when the next one arrives or
/// the range ends -- so there is nothing to do with it here.
fn convert_subpicture(
    id: i32,
    at: f64,
    unit: &[u8],
    mended: bool,
    writer: &mut Writer,
) -> Result<()> {
    let Some(which) = writer.converted.iter().position(|c| c.id == id) else {
        return Ok(());
    };
    let (track, sets) = {
        let c = &mut writer.converted[which];
        let Some(drawn) = c.reader.read(unit)? else {
            return Ok(());
        };
        let picture = crate::pgs::write::Picture {
            x: drawn.x,
            y: drawn.y,
            width: drawn.width,
            height: drawn.height,
            indices: &drawn.indices,
            palette: &drawn.palette,
        };
        let until = drawn.until.map(|d| at + d);
        c.shown += 1;
        (c.track, c.composer.show(c.screen, at, until, &picture))
    };
    for held in &sets {
        writer.push_graphics(track, held, 0.0, mended)?;
    }
    Ok(())
}

/// Read the subtitles of the stretch in front of a kept range, so that one
/// already on screen when it opens can be written again at its first frame.
///
/// The same shape as [`read_graphics_before`] and the same reason for
/// existing. What comes back is the last unit each stream sent before the
/// range, with the time it was shown at, and whether it had already taken
/// itself down by the time the range begins is decided by the caller.
fn read_subpictures_before(src: &Source, until: f64) -> Result<Vec<(i32, f64, Vec<u8>)>> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    // Seeked before anything is discarded, and only where there is somewhere
    // to seek to. **Both halves of that are load-bearing.** A recording is
    // seeked by one of its streams, and libavformat refuses outright to seek
    // one that has been thrown away -- which leaves the context in no state
    // to read from at all, so the discards go on afterwards. And a range
    // that opens inside the first few seconds has nothing behind it to seek
    // to: a fresh context is already at the top, which is where the look
    // back wants to start.
    //
    // Failing to look back is not a reason to fail a cut. What it costs is
    // one subtitle at one boundary.
    let from = until - crate::pgs::LOOKBACK - src.seek_margin;
    if from > 0.0 {
        let _ = seek_to(&mut ictx, src, until - crate::pgs::LOOKBACK);
    }
    // The pictures and the sound are walked past rather than assembled. What
    // is left is the subtitles and a DVD's navigation packs, which are small.
    let mut heavy: Vec<usize> = src.audios.iter().map(|a| a.stream_index).collect();
    heavy.push(src.video.stream_index);
    for stream in ictx.streams() {
        if heavy.contains(&stream.index()) {
            unsafe {
                (*(stream.as_ptr() as *mut ff::ffi::AVStream)).discard = ff::Discard::All.into();
            }
        }
    }
    let mut last: Vec<(i32, f64, Vec<u8>)> = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.parameters().id() != ff::codec::Id::DVD_SUBTITLE {
            continue;
        }
        let Some(pts) = packet.pts() else { continue };
        let t = pts as f64 * f64::from(stream.time_base()) - src.start_time;
        if t >= until {
            break;
        }
        let id = stream.id();
        last.retain(|(other, _, _)| *other != id);
        last.push((id, t, packet.data().unwrap_or(&[]).to_vec()));
    }
    Ok(last)
}

/// Read the graphics of the stretch in front of a kept range, so that
/// whatever is on screen when the range opens is known.
///
/// A subtitle a range opens in the middle of was put up by a display set
/// that the cut left behind, and there is no way to know which set that was
/// except to have read it. So the stretch before the range is read -- see
/// [`crate::pgs::LOOKBACK`] for how much and why that is enough -- and the
/// plane is left describing the moment the range begins.
///
/// This is a read and not a demux: every stream but the graphics is
/// discarded inside libavformat, so the pictures and the sound are walked
/// past rather than assembled. What it costs is the disc reading a few
/// seconds of itself, once per kept range, on a recording that has graphics
/// in it at all.
fn read_graphics_before(src: &Source, until: f64, writer: &mut Writer) -> Result<()> {
    for t in writer.graphics.iter_mut() {
        t.plane.reset();
    }
    let wanted: Vec<usize> = writer.graphics.iter().map(|t| t.in_index).collect();
    let mut ictx = crate::input::demux(&src.input.url)?;
    // Seeked first, and only where there is somewhere to seek to. See
    // [`read_subpictures_before`], where both halves of that were learnt.
    let from = until - crate::pgs::LOOKBACK - src.seek_margin;
    if from > 0.0 {
        seek_to(&mut ictx, src, until - crate::pgs::LOOKBACK)?;
    }
    for stream in ictx.streams() {
        if !wanted.contains(&stream.index()) {
            unsafe {
                (*(stream.as_ptr() as *mut ff::ffi::AVStream)).discard = ff::Discard::All.into();
            }
        }
    }
    let mut done = vec![false; writer.graphics.len()];
    for (stream, packet) in ictx.packets() {
        let Some(k) = wanted.iter().position(|i| *i == stream.index()) else {
            continue;
        };
        if done[k] {
            continue;
        }
        let Some(pts) = packet.pts() else { continue };
        let tb = writer.graphics[k].in_tb;
        let at = |stamp: i64| stamp as f64 * tb - src.start_time;
        let t = at(pts);
        if t >= until {
            done[k] = true;
            if done.iter().all(|&d| d) {
                break;
            }
            continue;
        }
        writer.graphics[k].plane.feed(crate::pgs::Held {
            pts: t,
            dts: packet.dts().map_or(t, at),
            data: packet.data().unwrap_or(&[]).to_vec(),
        });
    }
    Ok(())
}

/// Take the segment's packets straight from the source, untouched.
fn copy_segment(
    src: &Source,
    seg: &Segment,
    ctx: &SegmentCtx,
    writer: &mut Writer,
) -> Result<Span> {
    let (display_base, reframe, first_segment) = (ctx.display_base, ctx.reframe, ctx.first);
    let (mut ictx, ist_index) = open_input(&src.input.url)?;
    let in_tb = f64::from(ictx.stream(ist_index).unwrap().time_base());
    let fd = src.video.frame_duration();
    let field = ctx.grid.unit();
    // The stretch this segment copies, as bytes. A copy that begins just
    // after a seam is looking for an entry point that the stretch before it
    // can reach over: the last entry point of that stretch is written earlier
    // in the file but presents later on the joined clock, so it arrived
    // first, looked like a seek that had overshot, and stopped the cut. See
    // [`stretch_bytes`].
    let (floor, wall) = stretch_bytes(src, seg, fd / 2.0);
    let mut read_at: Option<u64> = None;
    seek_into(&mut ictx, src, seg.start, floor)?;
    // Anchored on the first picture actually emitted, not on the planner's
    // idealised time for it.
    let mut anchor: Option<f64> = None;
    let mut span = Span::default();
    // Where the previous picture went, when it was the first field of a pair
    // and the next one completes it. See where the pair is placed.
    let mut first_field: Option<i64> = None;

    let mut started = false;
    let mut overshot = false;
    // The first picture met past the entry point that was asked for, where
    // the entry point itself never turned up.
    let mut instead: Option<f64> = None;
    let mut video_done = false;
    // One flag per stream being gathered. The read stops when every one of
    // them has run past this segment, not when the first does: the sound of
    // a bilingual recording arrives on two PIDs that are interleaved but not
    // in step, and stopping on either would truncate the other.
    let mut audio_done = vec![false; ctx.audio.len()];
    let mut caption_done = vec![false; ctx.captions.len()];
    let mut graphics_done = vec![false; ctx.graphics.len()];
    // A recording with no subtitles of this kind is done with them before it
    // begins, which is what keeps the read from waiting on packets that are
    // never coming.
    let mut sub_done =
        src.subpictures.is_empty() || (writer.subpictures.is_none() && writer.converted.is_empty());

    for (stream, packet) in ictx.packets() {
        let index = stream.index();
        // Where the read has got to, in bytes into the clip. Not every packet
        // says -- the second field of a pair does not -- so the last one that
        // did stands in for those that do not, which is what the position
        // means anyway: the read goes one way through the file.
        if packet.position() >= 0 {
            read_at = Some(packet.position() as u64);
        }
        if index != ist_index {
            if let Some(k) = ctx.audio.iter().position(|a| a.in_index == index) {
                if !audio_done[k] {
                    audio_done[k] =
                        take_audio(&ctx.audio[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.captions.iter().position(|c| c.in_index == index) {
                if !caption_done[k] {
                    caption_done[k] =
                        take_caption(&ctx.captions[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.graphics.iter().position(|g| g.in_index == index) {
                if !graphics_done[k] {
                    graphics_done[k] = take_graphics(&ctx.graphics[k], src, seg, packet, writer)?;
                }
            } else if !sub_done && stream.parameters().id() == ff::codec::Id::DVD_SUBTITLE {
                // Matched on what the stream *is* rather than on an index
                // settled in advance: a DVD's subtitles may not exist as
                // streams until the demuxer has read a long way into the
                // recording. See [`crate::SubpictureInfo`].
                let id = stream.id();
                let tb = f64::from(stream.time_base());
                sub_done = take_subpicture(seg, ctx.offset, id, tb, src, &packet, writer);
            }
            if video_done
                && audio_done.iter().all(|&d| d)
                && caption_done.iter().all(|&d| d)
                && graphics_done.iter().all(|&d| d)
                && sub_done
            {
                break;
            }
            continue;
        }
        if video_done {
            if audio_done.iter().all(|&d| d)
                && caption_done.iter().all(|&d| d)
                && graphics_done.iter().all(|&d| d)
                && sub_done
            {
                break;
            }
            // See [`TRAIL`]. The pictures are still arriving, so they are
            // what says how far past the end the read has gone.
            if let Some(pts) = packet.pts() {
                if pts as f64 * in_tb - src.start_time > seg.end + TRAIL {
                    break;
                }
            }
            continue;
        }
        // Pictures from the stretches either side of this one are not this
        // segment's to copy, whatever their times say. See `floor`/`wall`.
        if read_at.zip(floor).is_some_and(|(at, f)| at < f.at)
            || read_at.zip(wall).is_some_and(|(at, w)| at >= w)
        {
            continue;
        }
        let Some(pts) = packet.pts() else { continue };
        let t = pts as f64 * in_tb - src.start_time;

        if !started {
            // Roll forward to the entry point.
            if !(packet.is_key() && (t - seg.start).abs() < fd / 2.0) {
                // A keyframe beyond the target means the entry point is
                // already behind us, and this is the first picture a copy
                // could have started on instead.
                if packet.is_key() && t > seg.start + fd {
                    overshot = true;
                    instead = Some(t);
                    break;
                }
                continue;
            }
            started = true;
        }
        // The copy ends when the terminating access point turns up. Its own
        // leading pictures are decoded after it, so stopping here leaves them
        // out -- which is exactly the display range the planner asked for.
        //
        // Never in the middle of a field pair, though: half a frame is not a
        // frame, and the access point that ends a copy opens one, so a pair
        // left open here can only be one the fallback below caught partway.
        if let Some(until) = seg.copy_until.filter(|_| first_field.is_none()) {
            if (packet.is_key() && (t - until).abs() < fd / 2.0) || t > until + fd {
                video_done = true;
                if audio_done.iter().all(|&d| d)
                    && caption_done.iter().all(|&d| d)
                    && graphics_done.iter().all(|&d| d)
                    && sub_done
                {
                    break;
                }
                continue;
            }
        }
        // Leading pictures of an open-GOP entry point present before it, so
        // they have no display slot here -- and the planner has established
        // that none of them is a reference.
        let a = *anchor.get_or_insert(t);
        if t < a - fd / 4.0 {
            continue;
        }
        let data = packet.data().unwrap_or(&[]);
        // Two field pictures are one frame between them, and the timeline
        // counts in fields, so a pair takes two places on it and lasts a
        // field each. The second field is placed after the first rather than
        // by its own timestamp: a recording that gives a pair one timestamp
        // between them -- or two close enough to round together -- would
        // otherwise put both fields in the same place, and the muxer would
        // take only one of them. The two are always next to each other in
        // decode order, so the first is always the one just seen.
        let (display, fields) = match (
            crate::bitstream::is_field_picture(
                data,
                &src.video.codec,
                src.video.framing,
                src.video.field_shape.as_ref(),
            ),
            first_field.take(),
        ) {
            (true, Some(first)) => (first + ctx.grid.sub, ctx.grid.sub),
            (true, None) => {
                let at = display_base + ((t - a) / field).round() as i64;
                first_field = Some(at);
                (at, 1)
            }
            (false, _) => (
                display_base + ((t - a) / field).round() as i64,
                ctx.grid.fields(crate::bitstream::display_fields(
                    data,
                    &src.video.codec,
                    src.video.vc1.as_ref(),
                )),
            ),
        };
        span.fields = span.fields.max(display - display_base + fields);
        span.pictures += 1;
        let packet = match (reframe, ctx.unframe) {
            (Some(r), _) if packet.is_key() => {
                let data = packet.data().unwrap_or(&[]);
                let mut out =
                    ff::Packet::copy(&prepend_parameter_sets(data, &r.sets, r.nal_length));
                out.set_flags(ff::packet::Flags::KEY);
                out
            }
            (_, Some(u)) => {
                let annexb = length_to_annexb(packet.data().unwrap_or(&[]), u.nal_length);
                let data = if packet.is_key() {
                    prepend_parameter_sets_annexb(&annexb, &u.sets)
                } else {
                    annexb
                };
                let mut out = ff::Packet::copy(&data);
                if packet.is_key() {
                    out.set_flags(ff::packet::Flags::KEY);
                }
                out
            }
            _ => packet,
        };
        // The one place a copied picture is not the recording's own bytes:
        // where the run has to fit a size, every picture goes through the
        // requantiser on its way out. See [`Shrink`].
        let packet = match writer.shrink.as_mut() {
            Some(shrink) => match shrink.picture(packet.data().unwrap_or(&[])) {
                Some(data) => {
                    let mut out = ff::Packet::copy(&data);
                    out.set_flags(packet.flags());
                    out
                }
                None => packet,
            },
            None => packet,
        };
        writer.push(Emitted {
            packet,
            display,
            fields,
        })?;
    }
    if !started {
        // **Two things look the same here and only one of them is a seek.**
        // Either the read landed past the entry point, which a larger margin
        // would cure, or the recording carries no picture there at all --
        // which is what a disc index that disagrees with its own stream reads
        // like, and measured here on one clip of one recorder's disc, where a
        // whole stretch of the entry-point map states times six seconds in
        // front of the pictures it indexes. How far the first picture that
        // could have started a copy is past the entry point tells them apart,
        // so it is said rather than guessed at.
        if let Some(found) = instead.filter(|_| overshot) {
            bail!(
                "the entry point at {:.3}s was passed without being met, and the first picture a \
                 copy could start on is {:.3}s further on, at {found:.3}s. Either the read \
                 overshot it, with {:.1}s of margin allowed for that, or the recording carries no \
                 entry point there and its index is describing something else.",
                seg.start,
                found - seg.start,
                src.seek_margin
            );
        }
        bail!("could not find the entry point at {:.3}s", seg.start);
    }
    Ok(held_to_the_end(span, anchor, seg, src, field))
}

/// How the pictures of a re-encoded segment are produced.
///
/// Every codec this program cuts has an encoder in libavcodec, bar one.
/// There is no VC-1 encoder anywhere -- not in libavcodec, not on a graphics
/// card -- so a Blu-ray written in VC-1 had nothing to build its partial
/// GOPs with, and a cut of one could only ever be a copy that began and
/// ended where the recording's own entry points happened to fall. See
/// [`smartcut_vc1`], which writes the few dozen pictures such a cut needs.
enum Pictures {
    Libav(Box<ff::encoder::video::Encoder>),
    Vc1(Box<smartcut_vc1::Encoder>),
}

/// The quantizer step VC-1 pictures are written at when nothing says
/// otherwise.
///
/// Fine. What this encoder writes is intra pictures, which are several times
/// the size of the predicted ones they stand in for, so a step that would be
/// extravagant over a whole recording costs a fraction of a second of it
/// here -- and what it buys is that the splice cannot be seen. Measured
/// against the pictures it replaces, this lands around 46dB.
pub const VC1_DEFAULT_QUANT: u8 = 4;

impl Pictures {
    fn open(
        src: &Source,
        params: &ff::codec::Parameters,
        opts: &CutOptions,
        signalling: &Signalling,
    ) -> Result<Self> {
        if matches!(src.video.codec.as_str(), "vc1" | "wmv3") {
            let shape = src.video.vc1.as_ref().ok_or_else(|| {
                anyhow!(
                    "this recording never states an advanced-profile sequence header, which is \
                     what a picture has to be written against, so the partial GOPs at the ends \
                     of a range cannot be re-encoded. A range whose ends fall on the \
                     recording's own entry points can still be cut"
                )
            })?;
            let step = opts.vc1_quant.unwrap_or(VC1_DEFAULT_QUANT);
            let encoder =
                smartcut_vc1::Encoder::new(shape, src.video.width, src.video.height, step)
                    .map_err(|e| anyhow!("cannot write VC-1 for this recording: {e}"))?;
            return Ok(Pictures::Vc1(Box::new(encoder)));
        }
        Ok(Pictures::Libav(Box::new(open_encoder(
            src, params, opts, signalling,
        )?)))
    }
}

/// Turn a decoded picture into a VC-1 one.
///
/// The pulldown flags come back off the frame the decoder handed over, so
/// the re-encoded pictures are shown for exactly as long as the ones they
/// replace -- which is what keeps a 2:3 pattern intact across the splice.
fn encode_vc1(
    encoder: &smartcut_vc1::Encoder,
    frame: &ff::frame::Video,
    fields: i64,
) -> Result<ff::Packet> {
    if frame.format() != ff::format::Pixel::YUV420P {
        bail!(
            "a VC-1 picture came back as {:?}, which is not 4:2:0",
            frame.format()
        );
    }
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let plane = |i: usize, w: usize, h: usize| smartcut_vc1::Plane {
        data: frame.data(i),
        stride: frame.stride(i),
        width: w,
        height: h,
    };
    let tff =
        unsafe { (*frame.as_ptr()).flags & ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST as i32 != 0 };
    // A picture shown for three fields is one whose first field is repeated;
    // anything longer is a whole frame shown again, which only a progressive
    // stream says.
    let (rff, rptfrm) = match fields {
        3 => (true, 0),
        n if n > 3 => (false, ((n - 2) / 2) as u8),
        _ => (false, 0),
    };
    let picture = smartcut_vc1::Frame {
        y: plane(0, width, height),
        u: plane(1, cw, ch),
        v: plane(2, cw, ch),
        tff,
        rff,
        rptfrm,
    };
    let mut packet = ff::Packet::copy(&encoder.encode(&picture));
    packet.set_flags(ff::packet::Flags::KEY);
    Ok(packet)
}

/// What a recording says about the display it was mastered on, and about the
/// brightest thing in it.
///
/// **HDR10 is two SEI messages, and they travel in the pictures.** Not in the
/// container -- the files measured here have nothing at stream level -- so a
/// stretch written by this program's encoder arrives without them while the
/// copied pictures on either side still carry theirs. A player reads them to
/// decide how to fit the picture to the screen in front of it, so what that
/// costs is the tone mapping changing partway through: on a 20 second cut of
/// a 4K HDR clip, the first 2.8 seconds were the re-encoded ones and the only
/// ones with nothing to say.
///
/// Read by decoding one picture, which is what turns the SEI into something
/// that can be handed to an encoder. Done once per cut, and only when there
/// is a stretch to re-encode at all.
type Mastering = Vec<(ff::ffi::AVFrameSideDataType, Vec<u8>)>;

/// Everything a re-encoded picture has to be told so that it describes itself
/// the way the copied pictures around it do.
///
/// Three separate things travel in the pictures rather than the container,
/// and every one of them is a way for a cut to change how a player fits the
/// picture to the screen partway through.
#[derive(Default)]
struct Signalling {
    /// Mastering display, content light level, and one picture's Dolby
    /// Vision metadata, handed to the encoder as `decoded_side_data` --
    /// which is where libx265 looks. Empty for everything that is not HDR.
    side: Mastering,
    /// The transfer characteristic the recording's own sequence header
    /// writes, where a decoder resolves a different one. See
    /// [`crate::bitstream::coded_transfer`].
    coded_transfer: Option<u8>,
    /// Whether the re-encoded pictures can carry the recording's Dolby
    /// Vision. False where it has none, and where the encoder refuses it.
    dovi: bool,
    /// Whether the recording carries Dolby Vision at all, which is what
    /// decides whether `dovi` being false is worth saying out loud.
    has_dovi: bool,
    /// The rate and the decoder buffer the recording's own MPEG-2 sequence
    /// header states, in the units it states them in. See
    /// [`crate::bitstream::mpeg2_rate`].
    mpeg2_rate: Option<(u32, u32)>,
}

/// Does this stream declare Dolby Vision?
///
/// The configuration record is what makes a player treat the pictures as
/// Dolby Vision at all, and it rides at stream level -- so it survives a cut
/// whether or not the RPUs inside the pictures do. That is the trap this is
/// asked about: a stream that says Dolby Vision and then hands a player no
/// RPU to drive it is worse off than one that never said so.
fn declares_dovi(params: &ff::codec::Parameters) -> bool {
    unsafe {
        let p = params.as_ptr();
        !ff::ffi::av_packet_side_data_get(
            (*p).coded_side_data,
            (*p).nb_coded_side_data,
            ff::ffi::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF,
        )
        .is_null()
    }
}

/// Take that record off a stream, so it stops claiming what it cannot back up.
unsafe fn drop_dovi(params: *mut ff::ffi::AVCodecParameters) {
    ff::ffi::av_packet_side_data_remove(
        (*params).coded_side_data,
        &mut (*params).nb_coded_side_data,
        ff::ffi::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF,
    );
}

fn signalling_of(src: &Source, opts: &CutOptions) -> Signalling {
    const WANTED: [ff::ffi::AVFrameSideDataType; 3] = [
        ff::ffi::AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
        ff::ffi::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
        // What one picture's RPU works out to, which is what libx265 is
        // configured from. The RPUs themselves ride on the frames handed to
        // it, one per picture.
        ff::ffi::AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA,
    ];
    let mut out = Signalling::default();
    let Ok((mut ictx, ist)) = open_input(&src.input.url) else {
        return out;
    };
    let Some(params) = ictx.stream(ist).map(|s| s.parameters()) else {
        return out;
    };
    out.has_dovi = declares_dovi(&params);
    // Nothing to look for outside HDR, and a picture not decoded is a picture
    // not paid for. `bt2020-10` is Blu-ray's wide-gamut SDR and carries none
    // of this; PQ and HLG are the two transfers that do. A Dolby Vision
    // recording states no transfer at all -- its RPU carries the colour --
    // so the record it declares is the other way in.
    let resolved = unsafe { (*params.as_ptr()).color_trc };
    let hdr = matches!(
        resolved,
        ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084
            | ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67
    );
    if !hdr && !out.has_dovi {
        // **Before the way out, not after it.** What the recording's own
        // sequence header states about its rate is wanted for every
        // re-encode, and an ordinary broadcast is exactly the case this
        // return sends home. It costs no decoding: the header is in the
        // bytes, restated at every entry point of a transport stream.
        if matches!(src.video.codec.as_str(), "mpeg2video" | "mpeg1video") {
            for (stream, packet) in ictx.packets().take(64) {
                if stream.index() != ist {
                    continue;
                }
                out.mpeg2_rate = packet.data().and_then(crate::bitstream::mpeg2_rate);
                if out.mpeg2_rate.is_some() {
                    break;
                }
            }
        }
        return out;
    }
    let Ok(mut decoder) = ff::codec::context::Context::from_parameters(params.clone())
        .and_then(|c| c.decoder().video())
    else {
        return out;
    };
    let mut frame = ff::frame::Video::empty();
    let mut picture = false;
    // However many pictures it takes to get one out, which for a stream that
    // reorders is the reorder depth and for anything else is one. Bounded so
    // a stream that never decodes does not turn this into a second pass. The
    // sequence header is looked for over the same packets: on a transport
    // stream it is restated at every entry point, and on an MP4 it is in the
    // extradata instead, which is read first.
    let extradata = unsafe {
        let p = params.as_ptr();
        if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec()
        }
    };
    // Only HEVC: `coded_transfer` reads an HEVC sequence header, and an
    // H.264 one handed to it would be read as though it were one.
    if src.video.codec == "hevc" {
        out.coded_transfer = crate::bitstream::parameter_sets(&src.video.codec, &extradata)
            .iter()
            .find_map(|set| crate::bitstream::coded_transfer(set));
    }
    for (stream, packet) in ictx.packets().take(64) {
        if stream.index() != ist {
            continue;
        }
        if out.coded_transfer.is_none() {
            out.coded_transfer = packet
                .data()
                .and_then(|d| {
                    crate::bitstream::sequence_header(&src.video.codec, d, src.video.framing)
                })
                .and_then(crate::bitstream::coded_transfer);
        }
        if picture || decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut frame).is_err() {
            continue;
        }
        unsafe {
            let f = frame.as_ptr();
            for i in 0..(*f).nb_side_data {
                let sd = *(*f).side_data.add(i as usize);
                if sd.is_null() || !WANTED.contains(&(*sd).type_) {
                    continue;
                }
                let bytes = std::slice::from_raw_parts((*sd).data, (*sd).size).to_vec();
                out.side.push(((*sd).type_, bytes));
            }
        }
        picture = true;
        // The header may still be ahead of the first picture on a stream
        // that keeps it out of band, so the walk goes on until both are had.
        if out.coded_transfer.is_some() {
            break;
        }
    }
    // Only worth carrying where it says something different from what a
    // decoder already worked out; anywhere else it is the same answer twice.
    // And only where libav has a name for it: the field is an enumeration,
    // and a recording is free to write a number that is not one of its
    // members.
    out.coded_transfer = out.coded_transfer.filter(|coded| {
        u32::from(*coded) != resolved as u32
            && u32::from(*coded) < ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_NB as u32
    });
    // Whether the encoder will take the Dolby Vision is settled here, once,
    // by asking it -- rather than at the first seam, by which time the
    // output's stream header has been written and cannot be taken back.
    if out.has_dovi {
        out.dovi = true;
        if let Err(e) = open_encoder(src, &params, opts, &out) {
            out.dovi = false;
            // And it is not left in the side data to be found again: the
            // encoder's own default is to decide for itself whether to write
            // Dolby Vision from what it is handed, and it has just said it
            // cannot. Every seam after this asks a plain question.
            out.side.retain(|(kind, _)| {
                *kind != ff::ffi::AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA
            });
            eprintln!(
                "note: this recording is Dolby Vision, and the encoder here will not write it \
                 ({e}). The pictures rewritten at each seam carry no RPU, so the Dolby Vision \
                 metadata stops where they begin; the copied pictures keep theirs. Where the \
                 recording declares Dolby Vision at stream level that claim is taken off the \
                 output too, so nothing is left saying what the pictures cannot back up. A \
                 range whose ends fall on the recording's own entry points is copied whole and \
                 comes through intact."
            );
        }
    }
    out
}

/// Which encoders may write a codec's partial GOPs, best first.
///
/// Empty for everything libavcodec has one sensible answer for, where
/// [`ff::encoder::find`] is asked instead. Two codecs are named outright.
///
/// **AV1.** What `find` returns is libaom-av1, whose default `cpu-used` is 0:
/// measured here at 348 seconds for two seconds of 1080p24, which is not a
/// seam being written but an export that looks hung. SVT-AV1 writes the same
/// two seconds in 3.97 and lands within 0.002 dB of it, so which encoder is
/// used is settled here rather than by whichever a build happens to register
/// first. A build with none of the three still reaches `find`.
///
/// **VP9.** There is only one either way. It is named so that the speed set
/// below does not depend on what `find` answered.
fn encoders_for(id: ff::codec::Id) -> &'static [&'static str] {
    match id {
        ff::codec::Id::AV1 => &["libsvtav1", "librav1e", "libaom-av1"],
        ff::codec::Id::VP9 => &["libvpx-vp9"],
        _ => &[],
    }
}

/// Build an encoder whose output splices onto the copied pictures.
///
/// Each candidate from [`encoders_for`] is tried in turn, because an encoder
/// being present is not the same as its being able to write *this* recording:
/// SVT-AV1 has no 4:2:2 and refuses one when it is opened, not when it is
/// looked up. The last complaint is kept so that a build where none of them
/// will open says why rather than naming the codec.
fn open_encoder(
    src: &Source,
    params: &ff::codec::Parameters,
    opts: &CutOptions,
    signalling: &Signalling,
) -> Result<ff::encoder::video::Encoder> {
    let id = params.id();
    let mut refused: Option<anyhow::Error> = None;
    for name in encoders_for(id) {
        let Some(codec) = ff::encoder::find_by_name(name) else {
            continue;
        };
        match open_encoder_as(codec, src, params, opts, signalling) {
            Ok(enc) => return Ok(enc),
            Err(e) => refused = Some(anyhow!("{name}: {e}")),
        }
    }
    if let Some(e) = refused {
        return Err(e);
    }
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no encoder for {id:?}"))?;
    open_encoder_as(codec, src, params, opts, signalling)
}

fn open_encoder_as(
    codec: ff::codec::codec::Codec,
    src: &Source,
    params: &ff::codec::Parameters,
    opts: &CutOptions,
    signalling: &Signalling,
) -> Result<ff::encoder::video::Encoder> {
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;

    let v = &src.video;
    enc.set_width(v.width);
    enc.set_height(v.height);
    // One tick per picture. MPEG-2 writes a frame rate code into its sequence
    // header, and the encoder reads that off the time base -- so a
    // field-granularity time base here made the re-encoded opening announce
    // 59.94 for a 29.97 recording. An indexer believes the first sequence
    // header it meets, which is exactly that one.
    //
    // The output timeline is still counted in fields; the pictures handed to
    // the encoder simply carry their own index, and where each belongs is
    // remembered alongside.
    let (num, den) = frame_rate_parts(v.frame_rate);
    enc.set_time_base(ff::Rational::new(den as i32, num as i32));
    enc.set_frame_rate(Some(ff::Rational::new(num as i32, den as i32)));

    unsafe {
        let p = params.as_ptr();
        let e = enc.as_mut_ptr();
        // codecpar carries the pixel format as a plain int
        (*e).pix_fmt = std::mem::transmute::<i32, ff::ffi::AVPixelFormat>((*p).format);
        (*e).sample_aspect_ratio = (*p).sample_aspect_ratio;
        (*e).color_primaries = (*p).color_primaries;
        // What the recording writes, not what a decoder worked out from it.
        // A broadcast carrying HLG the backward-compatible way writes 14 and
        // says 18 in an SEI beside it; write 18 here and the pictures
        // spliced in describe themselves differently from the copied ones on
        // either side. See [`crate::bitstream::coded_transfer`].
        (*e).color_trc = match signalling.coded_transfer {
            Some(coded) => {
                std::mem::transmute::<u32, ff::ffi::AVColorTransferCharacteristic>(u32::from(coded))
            }
            None => (*p).color_trc,
        };
        (*e).colorspace = (*p).color_space;
        (*e).color_range = (*p).color_range;
        (*e).profile = (*p).profile;
        // **A level is not one number across codecs, and two of these count
        // it differently at each end.** What a decoder puts in `level` for
        // AV1 is the stream's own `seq_level_idx`, 0 for 2.0 and counting
        // up; what SVT-AV1 wants there is 20 for the same level, and handed
        // an 8 it says `Level must be in the range of [2.0-7.3]` at every
        // seam. VP9 numbers its own the third way again. Nothing is lost by
        // leaving it: a level describes what a decoder must be able to keep
        // up with, the pictures written here are the size and rate of the
        // ones they sit among, and an encoder left to work it out writes a
        // level those pictures actually need. The codecs whose two ends
        // agree are still told, because there the recording's own answer is
        // better than a fresh one.
        if !matches!(params.id(), ff::codec::Id::AV1 | ff::codec::Id::VP9) {
            (*e).level = (*p).level;
        }
        (*e).max_b_frames = v.has_b_frames.max(0);
        if v.interlaced() {
            // Encode fields, not frames. Without this the partial GOPs come
            // out progressive and comb against the copied pictures.
            (*e).flags |= (ff::ffi::AV_CODEC_FLAG_INTERLACED_DCT
                | ff::ffi::AV_CODEC_FLAG_INTERLACED_ME) as i32;
            (*e).field_order = (*p).field_order;
        }
        // A partial GOP is re-encoded whole and spliced, so it must not
        // depend on anything outside itself.
        (*e).flags |= ff::ffi::AV_CODEC_FLAG_CLOSED_GOP as i32;
        (*e).gop_size = 600;
        // **Every core, the same as the decoder feeding it.** What
        // `avcodec_alloc_context3` leaves here is 1, not 0, and an encoder
        // reads it literally: the seams were written on one core while the
        // decode in front of them ran on four. It never showed on the codecs
        // this program started with, because a seam of H.264 is a second of
        // work either way -- but libvpx spent 6.56 s on two seconds of
        // 1080p24 held to one core and 2.45 s given the machine. Zero is how
        // libav is told to take what there is. See
        // [`crate::video_decoder_with`], which says the same thing at the
        // other end.
        (*e).thread_count = 0;
        // What the recording says about its own mastering, handed over the
        // way an encoder expects to be told: libx265 reads these and writes
        // the SEI back, so the pictures spliced in describe themselves the
        // same way the ones around them do. See [`signalling_of`].
        for (kind, bytes) in &signalling.side {
            let sd = ff::ffi::av_frame_side_data_new(
                &mut (*e).decoded_side_data,
                &mut (*e).nb_decoded_side_data,
                *kind,
                bytes.len(),
                0,
            );
            if !sd.is_null() && !(*sd).data.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*sd).data, bytes.len());
            }
        }
    }
    // A re-encoded seam is written at what the pictures around it are worth,
    // and where the run is being made smaller those pictures are worth less.
    // Otherwise the few frames this program writes at a seam would be the
    // only fine ones on the disc, and paid for by the rest.
    let bit_rate = opts.bit_rate.unwrap_or_else(|| {
        let plain = default_bit_rate(src) as f64;
        (plain * opts.video_share.unwrap_or(1.0).clamp(0.05, 1.0)) as usize
    });
    enc.set_bit_rate(bit_rate);
    // And what the re-encoded pictures are to *say* about the rate, which is
    // not the same question as what to spend.
    //
    // **A sequence header that disagrees with the one beside it is a file a
    // tool refuses.** libavcodec writes the rate and the decoder buffer of an
    // MPEG-2 sequence header out of the rate control it was given, and given
    // none it writes the value that means unspecified -- eighteen bits of
    // ones, which reads back as 104.857 Mbit/s. Every cut this program made
    // of a broadcast carried one of those at the head of each kept range,
    // beside four hundred of the recording's own saying 20. A smart renderer
    // handed such a file says `ビットレート（サポートしていません）` and will
    // not touch it; what a recorder does with it was never going to be
    // better. So the recording's own two numbers go back in, and the header
    // the encoder writes says what the copied pictures around it say.
    //
    // The rate control is told, rather than the header patched afterwards,
    // because the encoder is entitled to believe what it was told: a buffer
    // it has been given is a buffer it may fill.
    if let Some((rate, vbv)) = signalling.mpeg2_rate {
        let ceiling = i64::from(rate) * 400;
        let buffer = i64::from(vbv) * 16 * 1024;
        // libavcodec refuses two things here, and both have to be answered
        // before it will open: a target above the ceiling it was handed, and
        // a buffer that cannot hold one frame at that target. The target is
        // what this *spends*, and a fifth over the measured average is
        // routinely more than the recording's own ceiling -- so it comes
        // down to the ceiling, which is the most the pictures around it were
        // allowed either. A recording whose stated buffer will not hold one
        // of its own frames is describing something that cannot happen, and
        // there the numbers are left alone rather than argued with.
        let spend = (bit_rate as i64).min(ceiling);
        if buffer * i64::from(num) >= spend * i64::from(den) {
            enc.set_bit_rate(spend as usize);
            unsafe {
                let e = enc.as_mut_ptr();
                (*e).rc_max_rate = ceiling;
                (*e).rc_buffer_size = buffer as i32;
            }
        }
    }

    let mut eopts = ff::Dictionary::new();
    // mpeg2video and mpeg4 refuse a closed GOP while scene-change detection
    // is live; a partial GOP is short enough that losing it costs nothing.
    if matches!(v.codec.as_str(), "mpeg2video" | "mpeg4") {
        eopts.set("sc_threshold", "1000000000");
    }
    // What the three open encoders are worth being asked to spend.
    //
    // **Their defaults are set for encoding a film, and this is writing a
    // second of one.** Measured on two seconds of 1080p24, against the
    // pictures being replaced: libaom-av1 spends 348 s at its default and
    // 13.3 s at `cpu-used 8`, for 0.0015 dB; SVT-AV1 spends 9.5 s at preset
    // 6 and 3.97 s at preset 8, for 0.0014 dB; libvpx-vp9 spends 6.52 s at
    // `cpu-used 0` and 2.54 s at 2, for 0.001 dB. Nothing in a seam is worth
    // the difference, and what a person notices is an export that sits
    // still. The figures chosen put a seam at roughly what libx264 costs for
    // the same seconds, which is the speed the rest of this program is
    // already judged at.
    match codec.name() {
        "libvpx-vp9" => {
            eopts.set("deadline", "good");
            eopts.set("cpu-used", "2");
            eopts.set("row-mt", "1");
        }
        "libsvtav1" => eopts.set("preset", "8"),
        "librav1e" => eopts.set("speed", "8"),
        "libaom-av1" => {
            eopts.set("cpu-used", "8");
            eopts.set("row-mt", "1");
        }
        _ => {}
    }
    if codec.name() == "libx265" {
        let mut x265: Vec<String> = Vec::new();
        // x265 writes its banner and its statistics to the terminal itself,
        // rather than through libav's logging, so quietening libav does not
        // quieten it -- forty-odd lines about an encoder the person running
        // this never asked for. Told here instead, and told nothing when the
        // logging was asked for, so the two stay in step. See [`crate::init`].
        if std::env::var("SMARTCUT_FFMPEG_LOG").is_err() {
            x265.push("log-level=none".into());
        }
        // The sequence header now says what the recording's says, so the SEI
        // that says what the pictures really are has to go back beside it --
        // otherwise the transfer is written twice and neither says HLG.
        if let Some(coded) = signalling.coded_transfer {
            let resolved = unsafe { (*params.as_ptr()).color_trc } as u32;
            if u32::from(coded) != resolved {
                x265.push(format!("atc-sei={resolved}"));
            }
        }
        if signalling.dovi {
            // x265 will not write Dolby Vision without an HRD to hang it on,
            // and an HRD needs the buffer described. The rate is the one this
            // encoder was just given; the buffer is a second of it, which is
            // longer than any partial GOP a cut re-encodes.
            let kbit = (bit_rate / 1000).max(1);
            x265.push(format!("vbv-maxrate={kbit}"));
            x265.push(format!("vbv-bufsize={kbit}"));
            eopts.set("dolbyvision", "1");
        }
        if !x265.is_empty() {
            eopts.set("x265-params", &x265.join(":"));
        }
    }
    enc.open_as_with(codec, eopts)
        .map_err(|e| anyhow!("cannot open encoder: {e}"))
}

/// How finely the output timeline is divided where a recording needs it.
///
/// **32 sub-fields, which at 24 fps is two thirds of a millisecond.** The
/// recordings this was measured on state their times in milliseconds, so
/// nothing they can say lands more than a third of one away from a place the
/// timeline has. It is only ever spent on a variable-rate recording; see
/// [`Grid`].
const FINE: i64 = 32;

/// The units the output timeline is counted in.
///
/// **A field of the rate the recording is built on**, which for everything
/// constant is the recording's own rate and one unit per field.
///
/// A variable-rate recording has no such rate to take. Its average is not a
/// rate anything in it was ever coded at -- a 23.976 recording with a second
/// of 59.94 in it averages 24.93 -- and a timeline built on that puts every
/// picture in the recording on a grid nothing was authored to. Measured on a
/// 50-second range of one: **1093 pictures of 1251 more than 5 ms from where
/// the recording had them, a median of 12.4 ms**, which is a third of a
/// frame. Worse, a field of it is 20 ms and the pictures of the fast stretch
/// are 16.7 ms apart, so two of them land on the same place and the second
/// has nowhere to go: **8 were dropped outright**, and the note about it
/// blamed a damaged recording.
///
/// So there the timeline is built on the rate the container says the
/// material was authored at -- `r_frame_rate`, the commonest duration in the
/// sample table -- and each field of it is divided into [`FINE`]. Both
/// numbers are then round: the base rate is what the pictures actually come
/// at nearly all of the time, and a fine division holds whatever the rest of
/// them do.
#[derive(Debug, Clone, Copy)]
struct Grid {
    /// Numerator and denominator of the rate the fields are counted at.
    num: i64,
    den: i64,
    /// Units per field.
    sub: i64,
}

impl Grid {
    fn of(src: &Source) -> Grid {
        let v = &src.video;
        if !v.variable_rate {
            let (num, den) = frame_rate_parts(v.frame_rate);
            return Grid { num, den, sub: 1 };
        }
        // **A declared rate above the average is not a frame rate.** For an
        // interlaced recording `r_frame_rate` is the rate of its *fields*,
        // so a Blu-ray averaging 29.97 declares 59.94 -- and a timeline
        // counting fields of 59.94 makes every picture half as long as it
        // is. It cannot arrive here by the container's own answer, which
        // only calls a recording variable when the average runs *above* the
        // declared rate, but the walk has an answer of its own and there is
        // no reason to let the two of them meet in the one place it would be
        // wrong. Where the declared rate is not usable the average stands,
        // which is what a recording that varies by holding a picture wanted
        // anyway: there the two agree, and all that is needed is the
        // finer division.
        let usable = v.base_rate > 0.0 && v.base_rate <= v.frame_rate * 1.01;
        let (num, den) = frame_rate_parts(if usable { v.base_rate } else { v.frame_rate });
        Grid { num, den, sub: FINE }
    }

    /// Ticks per second in the output's own time base.
    fn timescale(self) -> i64 {
        2 * self.num * self.sub
    }

    fn time_base(self) -> ff::Rational {
        ff::Rational::new(1, self.timescale() as i32)
    }

    /// How long one unit lasts, in seconds.
    fn unit(self) -> f64 {
        self.den as f64 / self.timescale() as f64
    }

    /// What a picture shown for `fields` fields occupies.
    fn fields(self, fields: i64) -> i64 {
        fields * self.sub
    }
}

/// Recover the exact rational frame rate, so 29.97 comes back as 30000/1001
/// rather than something that rounds.
fn frame_rate_parts(fps: f64) -> (i64, i64) {
    let r = ff::Rational::from(fps).reduce();
    (r.numerator().max(1) as i64, r.denominator().max(1) as i64)
}

/// Bits per second for the pictures written across a splice.
///
/// **What the recording came in at, and a fifth again.** The pictures being
/// replaced were coded by whatever encoder made the disc or the broadcast,
/// with as long as it liked to spend; these are coded once, quickly, and at
/// the same rate they would come out visibly softer than the copied pictures
/// beside them. The headroom is what buys the splice back.
///
/// Falling back to the frame size is only for a source nobody counted -- an
/// index taken from a container's seek table, or one held from before the
/// count existed. It was what every re-encode used to get, and on this
/// material it was wrong by a factor of five or six: a 27 Mbit/s Blu-ray was
/// re-encoded at 4.5, and 70 Mbit/s of UHD at 15.9. Where the count is there,
/// it is the answer.
fn default_bit_rate(src: &Source) -> usize {
    let v = &src.video;
    if let Some(measured) = v.bit_rate.filter(|r| r.is_finite() && *r > 0.0) {
        return (measured * 1.2) as usize;
    }
    if let Some(rest) = pictures_less_the_sound(src) {
        return (rest * 1.2) as usize;
    }
    let px = v.width as f64 * v.height as f64 * v.frame_rate.max(1.0);
    (px * 0.08) as usize
}

/// Whether this run writes its pictures back smaller, and what with.
///
/// Only MPEG-2, and only where a share was asked for. A recording in
/// anything else says so rather than quietly coming out the size it always
/// was: the caller worked out that share because something has to fit, and a
/// cut that ignores it leaves them to find out from the disc.
fn shrink_for(src: &Source, opts: &CutOptions) -> Option<Shrink> {
    let share = opts.video_share.filter(|s| *s > 0.0 && *s < 1.0)?;
    if src.video.codec != "mpeg2video" {
        crate::note_once(format!(
            "note: this recording's pictures are {} and only MPEG-2 can be written back smaller \
             without decoding it, so they are copied as they are. The cut will be its full size.",
            src.video.codec
        ));
        return None;
    }
    Some(Shrink {
        rater: smartcut_mpeg2::Transrater::default(),
        share,
        declined: None,
    })
}

/// What is left of the file's own bit rate once the sound is taken off it.
///
/// For an index that did not read the pictures -- a disc's entry-point map,
/// a container's seek table -- there is no count of what they weigh, and the
/// file is still the best witness there is: its bytes over its length, less
/// the tracks whose rate is known. On a Blu-ray that is most of the
/// difference, since uncompressed sound is the loudest thing in the file
/// after the pictures.
///
/// `None` where the file's size or length is not known, or where the sound
/// would account for nearly all of it -- an answer that says the pictures
/// weigh nothing is not an answer.
fn pictures_less_the_sound(src: &Source) -> Option<f64> {
    if src.duration <= 0.0 {
        return None;
    }
    let bytes = match &src.input.range {
        Some(r) => r.len,
        None => std::fs::metadata(&src.input.file).ok()?.len(),
    };
    let whole = bytes as f64 * 8.0 / src.duration;
    let sound: f64 = src
        .audios
        .iter()
        .filter_map(|a| a.bit_rate)
        .map(|b| b as f64)
        .sum();
    let left = whole - sound;
    (left > whole * 0.2).then_some(left)
}

/// Restate the source's field order on a decoded frame.
///
/// The decoder sets these, but they are cheap to reassert and a frame that
/// reaches an interlaced encoder without them is coded as progressive.
fn mark_interlacing(frame: &mut ff::frame::Video, video: &crate::VideoInfo) {
    if !video.interlaced() {
        return;
    }
    unsafe {
        let f = frame.as_mut_ptr();
        (*f).flags |= ff::ffi::AV_FRAME_FLAG_INTERLACED;
        if video.top_field_first() {
            (*f).flags |= ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
        } else {
            (*f).flags &= !ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
        }
    }
}

/// Pull everything the encoder is ready to hand over.
///
/// This has to happen between sends, not just at the end: an encoder that
/// has filled its output queue refuses further frames with EAGAIN.
fn drain_encoder(
    encoder: &mut ff::encoder::video::Encoder,
    reframe: Option<&Reframe>,
    placed: &std::collections::HashMap<i64, (i64, i64)>,
    writer: &mut Writer,
) -> Result<()> {
    loop {
        let mut packet = ff::Packet::empty();
        if encoder.receive_packet(&mut packet).is_err() {
            return Ok(());
        }
        // The frame went in labelled with its own index, so the packet comes
        // back saying which picture it is; where that picture sits on the
        // output timeline, and how long it lasts, were noted at the time.
        let index = packet.pts().unwrap_or(0);
        let (display, fields) = placed.get(&index).copied().unwrap_or((index, 2));
        let packet = match reframe {
            // The encoder writes start codes; the container wants lengths.
            Some(r) if packet.data().map(is_annexb).unwrap_or(false) => {
                let data = annexb_to_length(packet.data().unwrap_or(&[]), r.nal_length);
                let mut out = ff::Packet::copy(&data);
                if packet.is_key() {
                    out.set_flags(ff::packet::Flags::KEY);
                }
                out
            }
            _ => packet,
        };
        writer.push(Emitted {
            packet,
            display,
            fields,
        })?;
    }
}

/// Decode the pictures this segment covers and encode them afresh.
fn reencode_segment(
    src: &Source,
    seg: &Segment,
    ctx: &SegmentCtx,
    opts: &CutOptions,
    writer: &mut Writer,
) -> Result<Span> {
    let (display_base, reframe, first_segment) = (ctx.display_base, ctx.reframe, ctx.first);
    let (mut ictx, ist_index) = open_input(&src.input.url)?;
    let stream = ictx.stream(ist_index).unwrap();
    let in_tb = f64::from(stream.time_base());
    let params = stream.parameters();
    let fd = src.video.frame_duration();
    let field = ctx.grid.unit();
    // A range bound often lands exactly on a picture's timestamp. Compare
    // with a hair of slack so float representation alone cannot decide
    // whether that picture is in or out.
    let tol = fd * 1e-3;

    let mut decoder = ff::codec::context::Context::from_parameters(params.clone())?
        .decoder()
        .video()?;
    let mut encoder = Pictures::open(src, &params, opts, ctx.signalling)?;

    let (floor, wall) = stretch_bytes(src, seg, tol);
    let mut read_at: Option<u64> = None;
    seek_into(&mut ictx, src, seg.seek_from, floor)?;

    let mut frame = ff::frame::Video::empty();
    let mut audio_done = vec![false; ctx.audio.len()];
    let mut caption_done = vec![false; ctx.captions.len()];
    let mut graphics_done = vec![false; ctx.graphics.len()];
    // A recording with no subtitles of this kind is done with them before it
    // begins, which is what keeps the read from waiting on packets that are
    // never coming.
    let mut sub_done =
        src.subpictures.is_empty() || (writer.subpictures.is_none() && writer.converted.is_empty());
    let mut anchor: Option<f64> = None;
    let mut span = Span::default();
    // The encoder hands packets back in decode order, labelled only with the
    // PTS they went in with; this remembers how long each of those pictures
    // is meant to be shown.
    let mut placed: std::collections::HashMap<i64, (i64, i64)> = Default::default();
    let mut fed = 0i64;
    let mut past_end = false;
    // Packets libavcodec would not take. See where they are counted.
    let mut damaged = 0usize;
    // **The decoder is shown one stretch of a joined clip and no more.**
    //
    // A seam is where the recorder stopped and started again, and the
    // pictures on either side of one were made minutes apart. Handed both,
    // libavcodec throws the first lot away: the stretch after a seam opens on
    // an IDR that says the pictures still waiting to be reordered are not to
    // be shown, and the last second of the stretch before it -- pictures that
    // are complete and only waiting their turn -- goes unshown with them.
    // That second is exactly what a range ending at a seam asks to be
    // re-encoded, so a cut of a recorder's clip lost it quietly, and where
    // nothing else fell in the window it stopped with nothing decoded.
    //
    // So the far side is never fed: reaching it drains the decoder instead,
    // which is what hands back the pictures it was holding. See
    // [`stretch_bytes`] for why this is asked of the position rather than the
    // time, and [`crate::plan::plan_on`], which cuts at seams for the same
    // reason this stops at them.
    let mut drained = false;

    // **What is on screen when a range opens need not have arrived in it.**
    //
    // A picture stays up until the next one replaces it. Where pictures come
    // at a constant rate the next one is always a frame away, so a range
    // begins within a frame of a picture of its own and the one before it is
    // of no interest. A variable-rate recording holds a picture for as long
    // as nothing changed -- seconds, on a screen capture -- and a range
    // opening inside such a hold has no picture of its own at all: measured
    // on a WebM whose pictures sit up to 2.4 s apart, a range inside one of
    // the holds stopped the whole run with `no pictures decoded`, and a
    // range that merely began inside one opened on whatever came next.
    //
    // So the last picture before the range is kept, and put in at the
    // range's own start where nothing arrives within a frame of it. That
    // test is what keeps this from touching constant-rate material, where
    // the gap to the previous picture is a frame by construction and the
    // range is answered by the picture it already had.
    let mut still_on_screen: Option<ff::frame::Video> = None;

    macro_rules! place {
        ($f:expr, $t:expr) => {{
            let t = $t;
            let a = *anchor.get_or_insert(t);
            let display = display_base + ((t - a) / field).round() as i64;
            let fields = 2 + unsafe { (*$f.as_ptr()).repeat_pict.max(0) as i64 };
            placed.insert(fed, (display, fields));
            span.fields = span.fields.max(display - display_base + fields);
            span.pictures += 1;
            $f.set_pts(Some(fed));
            fed += 1;
            $f.set_kind(ff::picture::Type::None);
            mark_interlacing(&mut $f, &src.video);
            match &mut encoder {
                Pictures::Libav(enc) => {
                    enc.send_frame(&$f)?;
                    drain_encoder(enc, reframe, &placed, writer)?;
                }
                // Nothing is held back: a picture that references
                // nothing is finished the moment it is written, so it
                // goes straight out with the timing worked out above.
                Pictures::Vc1(enc) => {
                    let packet = encode_vc1(enc, &$f, fields)?;
                    writer.push(Emitted {
                        packet,
                        display,
                        fields,
                    })?;
                }
            }
        }};
    }

    macro_rules! feed {
        () => {
            while decoder.receive_frame(&mut frame).is_ok() {
                let Some(pts) = frame.pts() else { continue };
                let t = pts as f64 * in_tb - src.start_time;
                if t >= seg.end - tol {
                    past_end = true;
                    continue;
                }
                if t < seg.start - tol {
                    if anchor.is_none() {
                        still_on_screen = Some(frame.clone());
                    }
                    continue;
                }
                // Nothing arrived within a frame of the range's start, so
                // what belongs there is the picture that was already up --
                // shown from the range's own start rather than from its own,
                // which is before the range.
                if anchor.is_none() && t - seg.start >= fd {
                    if let Some(mut held) = still_on_screen.take() {
                        place!(held, seg.start);
                    }
                }
                still_on_screen = None;
                place!(frame, t);
            }
        };
    }

    for (stream, packet) in ictx.packets() {
        let index = stream.index();
        // Where the read has got to, in bytes into the clip. Not every packet
        // says -- the second field of a pair does not -- so the last one that
        // did stands in for those that do not, which is what the position
        // means anyway: the read goes one way through the file.
        if packet.position() >= 0 {
            read_at = Some(packet.position() as u64);
        }
        if index != ist_index {
            if let Some(k) = ctx.audio.iter().position(|a| a.in_index == index) {
                if !audio_done[k] {
                    audio_done[k] =
                        take_audio(&ctx.audio[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.captions.iter().position(|c| c.in_index == index) {
                if !caption_done[k] {
                    caption_done[k] =
                        take_caption(&ctx.captions[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.graphics.iter().position(|g| g.in_index == index) {
                if !graphics_done[k] {
                    graphics_done[k] = take_graphics(&ctx.graphics[k], src, seg, packet, writer)?;
                }
            } else if !sub_done && stream.parameters().id() == ff::codec::Id::DVD_SUBTITLE {
                // Matched on what the stream *is* rather than on an index
                // settled in advance: a DVD's subtitles may not exist as
                // streams until the demuxer has read a long way into the
                // recording. See [`crate::SubpictureInfo`].
                let id = stream.id();
                let tb = f64::from(stream.time_base());
                sub_done = take_subpicture(seg, ctx.offset, id, tb, src, &packet, writer);
            }
            continue;
        }
        // See [`TRAIL`]: past the end and still decoding only to wait for a
        // stream that is not coming.
        if past_end {
            if audio_done.iter().all(|&d| d)
                && caption_done.iter().all(|&d| d)
                && graphics_done.iter().all(|&d| d)
                && sub_done
            {
                break;
            }
            if packet
                .pts()
                .is_some_and(|p| p as f64 * in_tb - src.start_time > seg.end + TRAIL)
            {
                break;
            }
        }
        // The stretch this segment belongs to, and nothing either side of it.
        // See `stretch_bytes`.
        if read_at.zip(floor).is_some_and(|(at, f)| at < f.at) {
            continue;
        }
        if drained || read_at.zip(wall).is_some_and(|(at, w)| at >= w) {
            if !drained {
                drained = true;
                // Past the end in the only sense that matters here: there are
                // no more pictures this segment can be given.
                past_end = true;
                decoder.send_eof()?;
                feed!();
            }
            continue;
        }
        // A packet the decoder will not take is not a reason to stop.
        //
        // A recording of a broadcast has holes in it -- a burst of noise, a
        // dish that lost the satellite for a moment -- and libavcodec refuses
        // the packets those fall inside. Refusing the cut with them meant a
        // fault a few frames wide made a whole recording uncuttable, while
        // ffmpeg reading the same file drops the packet and carries on
        // decoding from the next one. So does this: what is lost is the
        // pictures inside the damage, which were never going to decode, and
        // how many were lost is said at the end.
        if decoder.send_packet(&packet).is_err() {
            damaged += 1;
            continue;
        }
        // Decoded frames arrive in display order, so a simple window test
        // picks out exactly the pictures this segment owns.
        feed!();
    }
    if !drained {
        decoder.send_eof()?;
        feed!();
    }
    let _ = past_end; // only the loop above acts on it
    // Nothing arrived in the window at all: the whole of it lies inside one
    // picture's hold, and that picture is the whole of what it has to show.
    if anchor.is_none() {
        if let Some(mut held) = still_on_screen.take() {
            place!(held, seg.start);
        }
    }
    let _ = fed; // the count only matters while pictures are still going in
    if let Pictures::Libav(enc) = &mut encoder {
        enc.send_eof()?;
        drain_encoder(enc, reframe, &placed, writer)?;
    }

    if span.pictures == 0 {
        if damaged > 0 {
            bail!(
                "segment {:.3}-{:.3}: no pictures decoded -- {damaged} packet(s) here are \
                 damaged and none of what is left is a picture this can re-encode",
                seg.start,
                seg.end
            );
        }
        bail!(
            "segment {:.3}-{:.3}: no pictures decoded",
            seg.start,
            seg.end
        );
    }
    if damaged > 0 {
        eprintln!(
            "note: {damaged} packet(s) between {:.3}s and {:.3}s are damaged and could not be \
             decoded, so the pictures they carried are missing from the {} written there. \
             What the cut copies is untouched by this.",
            seg.start, seg.end, span.pictures,
        );
    }
    Ok(held_to_the_end(span, anchor, seg, src, field))
}

/// Where the streams go on the way out.
///
/// A cut of a broadcast puts every stream back on the PID it arrived on. The
/// tools downstream of a recording look for the sound and the captions where
/// the broadcast put them, and a fresh numbering makes the output look like
/// something else entirely.
///
/// A cut written in Blu-ray's own framing cannot do that. The map of a
/// `.m2ts` is on PID 0x0100 whatever else is on it, and a Japanese broadcast
/// puts its pictures there often enough for the two to meet -- at which
/// point the muxer stops the cut outright:
///
/// ```text
/// [mpegts] PID 256 cannot be both elementary and PMT PID
/// ```
///
/// So a `.m2ts` is numbered the way a Blu-ray is numbered: the clock on
/// 0x1001, the pictures on 0x1011, the sound from 0x1100, a broadcast's own
/// captions from 0x1110, and the subtitles a disc draws from 0x1200. That is
/// where a disc player looks, it is what the disc's index will say the clip
/// carries ([`crate::bdav`]), and the recording's own tables still describe
/// each stream -- they are written over the muxer's with the streams named
/// by where they went rather than where they came from. The clock is the one
/// of those the muxer will not do; see [`crate::si::graft`].
///
/// **The two kinds of subtitle do not share a run.** 0x1200 upwards is the
/// range Blu-ray keeps for the graphics it draws itself, and that is what
/// goes there: a presentation graphics stream, declared as one. A
/// broadcast's own captions are not that -- they are a data stream in a
/// private format, and numbering them into the graphics range had them
/// announced as something nothing can read them as. A recorder's own disc
/// puts its captions at 0x1110, inside the range the sound is numbered from,
/// and this does the same. Written into one run, as this did before, a
/// recording carrying both had the second kind numbered after the first.
/// **A stream is asked about by what it is and which one it is, not by the
/// number it arrived on.** This was a map from the recording's own PID to
/// the one it was written on, which is exactly right for a recording that
/// has PIDs and answers nothing for one that has not. A Matroska file has
/// none -- libavformat leaves every stream's id at nought -- so the
/// pictures and the sound both asked the map about stream 0, both were
/// given 0x1011, and the muxer refused two streams on one PID with
/// `Invalid argument` and nothing else. Every `.m2ts` and every disc
/// written out of a `.mkv` failed that way, while a `.ts` out of the same
/// file was fine, because a `.ts` keeps what the recording had and a
/// recording with nothing has nothing to keep.
struct Pids {
    /// Whether the streams are being numbered the way a disc numbers them.
    bluray: bool,
}

impl Pids {
    /// The recording's own numbering, which is what a `.ts` keeps.
    fn kept() -> Pids {
        Pids { bluray: false }
    }

    /// Blu-ray's numbering, by the order the streams are written in.
    fn bluray() -> Pids {
        Pids { bluray: true }
    }

    /// Where the pictures go.
    fn video(&self, was: i32) -> i32 {
        if self.bluray {
            0x1011
        } else {
            was
        }
    }

    /// Where the `nth` sound track goes, counting from nought in the order
    /// the tracks are written.
    fn audio(&self, nth: usize, was: i32) -> i32 {
        self.nth(0x1100, nth, was)
    }

    /// And the `nth` of a broadcast's own captions.
    fn caption(&self, nth: usize, was: i32) -> i32 {
        self.nth(0x1110, nth, was)
    }

    /// And the `nth` stream of subtitles a disc draws, which the converted
    /// ones are numbered on after the carried ones.
    fn graphics(&self, nth: usize, was: i32) -> i32 {
        self.nth(0x1200, nth, was)
    }

    fn nth(&self, first: i32, nth: usize, was: i32) -> i32 {
        if self.bluray {
            first + nth as i32
        } else {
            was
        }
    }
}

/// Ask the muxer to put a stream on a particular PID.
///
/// The MPEG-TS muxer reads a stream's id as the PID to write it on, and only
/// numbers from `mpegts_start_pid` the streams that do not name one. Which
/// is what lets a recording come back out on its own PIDs -- the sound where
/// the tools downstream expect sound, the captions where a caption decoder
/// looks -- and what lets a cut written for a disc be numbered the way a
/// disc is numbered instead; see [`Pids`]. Anything below 16 is not a PID a
/// stream can sit on and is how libav says it has no opinion.
unsafe fn set_pid(ost: &mut ff::format::stream::StreamMut, to_ts: bool, pid: i32) {
    if to_ts && (0x0010..=0x1FFA).contains(&pid) {
        (*ost.as_mut_ptr()).id = pid;
    }
}

/// What is to become of one sound track, settled before anything is written.
struct AudioSetup {
    info: crate::AudioInfo,
    /// How the track is produced, which is not always how it was asked for:
    /// a downmix is a whole-track re-encode however it was requested.
    mode: AudioMode,
    /// The codec the track is written as. The recording's own, except where
    /// one was asked for by name or the container has no box for it -- see
    /// [`carriage`].
    target: ff::codec::Id,
    /// Whether that is a different codec from the one that arrived. What
    /// the output says about the track then has to be said afresh: the
    /// recording's own map describes a stream this file does not contain.
    recoded: bool,
    /// What to ask the encoder to take, where the recording's own answer is
    /// not the one meant. `None` follows the recording -- see `like` in
    /// [`crate::audio`].
    like: Option<ff::format::Sample>,
    /// Channels out.
    channels: u16,
    /// The rate out, which is the encoder's last word rather than the
    /// caller's: the stream is declared at it and its packets are timed
    /// against it.
    sample_rate: u32,
    /// The fold, when there is one: what it was, and what it became.
    downmix: Option<(u16, u16)>,
    /// How the frames this cut encodes for the track are framed, where they
    /// are framed at all. Only an answer for AAC -- ADTS from an HD
    /// broadcast, LATM from a 4K one; anything else leaves the frames this
    /// tool encodes unframed, exactly as the packets they sit among are.
    frame_as: Option<crate::aac::Framing>,
    bit_rate: usize,
    /// What one of the recording's own frames of this track is worth in
    /// time, where its packets do not carry a duration. See
    /// [`assumed_frame`].
    frame_secs: Option<f64>,
}

/// What a track is written as, given where it is going.
///
/// A cut copies the recording's own frames, so the codec on the way out is
/// the codec on the way in -- with one exception, and it comes off a Blu-ray.
/// **Blu-ray LPCM** (`pcm_bluray`, and DVD's `pcm_dvd` beside it) is carried
/// in a private stream that only a transport stream describes: an MP4 asked
/// to declare one stops the whole cut with *"could not find tag for codec
/// pcm_bluray"*, and an MKV does no better.
///
/// What it holds, though, is plain PCM samples, and every container has a box
/// for those. So the track is written as big-endian PCM -- `ipcm` in an MP4,
/// `twos` or `in24` in a QuickTime file -- at the width the recording's own
/// samples have. Nothing is lost on the way: the samples pass through a
/// 32 bit float, whose 24 bit mantissa holds every value a 24 bit recording
/// can carry, and Blu-ray LPCM goes no deeper than 24.
///
/// A transport stream is the exception only for the Blu-ray kind. DVD LPCM
/// is a different private stream with a different header, and no transport
/// stream declares it: the muxer takes the frames, writes them under the
/// stream type that means "some private data", and a player finds a track it
/// cannot name. A cut of a DVD title written as `.ts` came out with its
/// sound reading as `bin_data` -- carried, declared, and silent -- and onto a
/// BDAV disc the same way. So it becomes Blu-ray LPCM, which is the shape a
/// transport stream has for exactly these samples.
///
/// Everything else a Blu-ray carries goes out as it came in. DTS and TrueHD
/// have boxes of their own in MP4; TrueHD's is one libavformat will write
/// only when asked to write outside the standard, which is done -- and said
/// -- where the muxer is opened.
///
/// A codec asked for by name answers before any of that: it is the whole
/// point of the setting, and the only question left is which box the
/// container has for it. Only LPCM has two -- see [`AudioCodec::Lpcm`].
/// `bits` is how wide the recording's own samples are -- 24 or 16, as
/// [`crate::audio::pcm_bits`] settles it -- which is the width an LPCM track
/// is written at whether the recording's own or a caller's choice put it
/// there.
fn carriage(source: ff::codec::Id, to_ts: bool, bits: u8, want: AudioCodec) -> ff::codec::Id {
    let pcm = || {
        if to_ts {
            // The one shape a transport stream can declare, and the reason
            // the note above exists in the first place.
            ff::codec::Id::PCM_BLURAY
        } else if bits > 16 {
            ff::codec::Id::PCM_S24BE
        } else {
            ff::codec::Id::PCM_S16BE
        }
    };
    match want {
        AudioCodec::Aac => ff::codec::Id::AAC,
        AudioCodec::Ac3 => ff::codec::Id::AC3,
        AudioCodec::Dts => ff::codec::Id::DTS,
        AudioCodec::Lpcm => pcm(),
        // Into a transport stream `pcm()` is Blu-ray LPCM, which is what a
        // Blu-ray track already was and what a DVD's becomes; everywhere
        // else it is the plain big-endian samples.
        AudioCodec::Source => match source {
            ff::codec::Id::PCM_BLURAY | ff::codec::Id::PCM_DVD => pcm(),
            _ => source,
        },
    }
}

/// What a codec is worth at this many channels, in bits per second.
///
/// Only consulted where the recording's own rate says nothing about the
/// track being written -- a 192 kbit/s AAC broadcast asked for as DTS is not
/// a 192 kbit/s DTS track, it is a DTS track that would not decode. Each
/// figure is what the format is ordinarily carried at: AAC as a broadcast
/// sends it, AC-3 as a disc holds it, DTS at its own two rates.
///
/// LPCM has no rate to choose. Its size is the samples' own -- channels
/// times bits times the sample rate -- and the encoder ignores what it is
/// handed, which is why the window greys the control out.
pub(crate) fn derived_bit_rate(target: ff::codec::Id, channels: u16) -> usize {
    use ff::codec::Id::*;
    match target {
        AC3 => match channels {
            0 | 1 => 96_000,
            2 => 192_000,
            _ => 448_000,
        },
        DTS => {
            if channels > 2 {
                1_536_000
            } else {
                768_000
            }
        }
        MP2 | MP3 => match channels {
            0 | 1 => 128_000,
            _ => 256_000,
        },
        _ if crate::audio::uncompressed(target) => 0,
        // AAC and anything else that reaches here.
        _ => match channels {
            0 | 1 => 96_000,
            2 => 192_000,
            n => 64_000 * n as usize,
        },
    }
}

/// One sound track as it stands, for [`writable_sound`].
///
/// What a screen knows about a recording's sound before anything is asked of
/// it, which is everything the answer below turns on.
#[derive(Debug, Clone, Default)]
pub struct SoundAsIs {
    /// The codec the recording carries, by libav's own name for it -- `aac`,
    /// `ac3`, `truehd`. Empty, or a name libav does not know, leaves the
    /// track unjudged: what cannot be named cannot be answered for, and a
    /// silence is a better answer there than a list greyed out on a guess.
    pub codec: String,
    pub channels: u16,
    pub sample_rate: u32,
    /// How wide its samples are once they are written as linear PCM. See
    /// [`crate::audio::pcm_bits`].
    pub bits: u8,
    /// Whether this track's cut is going into a transport stream, which is
    /// the one thing about the container that decides a codec. See
    /// [`carriage`].
    pub to_ts: bool,
}

/// Answers about the sound: the ones a screen offers, and the ones it may.
///
/// A zero stands for the recording's own -- the `入力と同じ` at the top of
/// every one of these lists -- which is [`CutOptions`]'s `None` written as a
/// number, since these are lists rather than single answers.
#[derive(Debug, Clone, Default)]
pub struct SoundChoices {
    pub codecs: Vec<AudioCodec>,
    pub channels: Vec<u16>,
    pub sample_rates: Vec<u32>,
    pub bits: Vec<u8>,
    pub bit_rates: Vec<usize>,
}

/// Whether one set of answers is one every track could be written with.
///
/// The same reasoning [`plan_audio`] runs, with nothing opened and no file
/// read: what codec the track would come out as, at what rate, how many
/// channels, and how much to spend -- and then whether an encoder for that
/// exists and will open. Anything not asked for follows the recording, which
/// is what the cut does.
///
/// A track nobody can name, one whose count or rate the recording never
/// said, and lossless sound that is carried through rather than encoded are
/// all true here: none of them is a question about an encoder.
fn sound_writes(tracks: &[SoundAsIs], opts: &CutOptions) -> bool {
    tracks.iter().all(|t| {
        let Some(source_id) = ff::decoder::find_by_name(&t.codec).map(|c| c.id()) else {
            return true;
        };
        // See `plan_audio`: an encoder is only reached here where the codec
        // was named, because otherwise the recording's own frames go out as
        // they came in.
        if crate::audio::carried_whole(source_id) && opts.audio_codec == AudioCodec::Source {
            return true;
        }
        let bits = opts.audio_bits.unwrap_or(t.bits);
        let target = carriage(source_id, t.to_ts, bits, opts.audio_codec);
        let channels = opts.audio_channels.unwrap_or(t.channels);
        let asked_rate = opts.audio_sample_rate.unwrap_or(t.sample_rate);
        if channels == 0 || asked_rate == 0 {
            return true;
        }
        let rate = crate::audio::writable_rate(target, asked_rate);
        // The one thing here the cut does not refuse: a rate the codec does
        // not speak is written at the nearest it does, with a note. Asked
        // for outright it is still not an answer a screen should offer --
        // 44.1 kHz chosen and 48 written is the screen saying something
        // untrue -- while a recording's own rate that has to be moved is
        // nobody's choice to withdraw.
        if opts.audio_sample_rate.is_some_and(|_| rate != asked_rate) {
            return false;
        }
        let bit_rate = opts
            .audio_bit_rate
            .unwrap_or_else(|| derived_bit_rate(target, channels));
        crate::audio::opens_at(target, rate, channels, bit_rate)
    })
}

/// The answers to judge the lists against.
///
/// Ordinarily the ones being held. Where those cannot be written between
/// them -- a project written before a floor was known of, a recording added
/// to the list that the settings do not suit -- judging every list against
/// them would grey out every list at once and leave nothing to choose. So
/// they are let go of one at a time until what is left can be written, in
/// the order they are worth least: a bitrate before a width, a width before
/// a rate, and the codec last of all, since it is the choice the rest of
/// them hang off.
fn settled(tracks: &[SoundAsIs], opts: &CutOptions) -> CutOptions {
    let mut o = opts.clone();
    for give_up in 0..5 {
        if sound_writes(tracks, &o) {
            break;
        }
        match give_up {
            0 => o.audio_bit_rate = None,
            1 => o.audio_bits = None,
            2 => o.audio_sample_rate = None,
            3 => o.audio_channels = None,
            _ => o.audio_codec = AudioCodec::Source,
        }
    }
    o
}

/// Which of the answers a screen offers a cut could actually be given.
///
/// Some of them it could not. Blu-ray LPCM is written at 48, 96 and 192 kHz
/// and nowhere between, so 44.1 asked of it in a transport stream is a rate
/// that will not be written; DTS is written in five channel arrangements and
/// no others, and has a bitrate floor that moves with the channels and the
/// rate, so 384 kbit/s of 5.1 is a cut that would stop where the encoder is
/// opened. None of that is worth discovering at the end of an export, and
/// none of it is worth a table in the window either -- the answers are the
/// encoders' own, and the encoders are whatever FFmpeg this build was linked
/// against. So the window sends what it offers and is told what of it can be
/// written, one list at a time, each judged with the other answers as they
/// stand.
pub fn writable_sound(
    tracks: &[SoundAsIs],
    opts: &CutOptions,
    offered: &SoundChoices,
) -> SoundChoices {
    let base = settled(tracks, opts);
    let asked = |f: &dyn Fn(&mut CutOptions)| {
        let mut o = base.clone();
        f(&mut o);
        sound_writes(tracks, &o)
    };
    SoundChoices {
        codecs: offered
            .codecs
            .iter()
            .copied()
            .filter(|&c| asked(&|o| o.audio_codec = c))
            .collect(),
        channels: offered
            .channels
            .iter()
            .copied()
            .filter(|&n| asked(&|o| o.audio_channels = (n > 0).then_some(n)))
            .collect(),
        sample_rates: offered
            .sample_rates
            .iter()
            .copied()
            .filter(|&r| asked(&|o| o.audio_sample_rate = (r > 0).then_some(r)))
            .collect(),
        bits: offered
            .bits
            .iter()
            .copied()
            .filter(|&b| asked(&|o| o.audio_bits = (b > 0).then_some(b)))
            .collect(),
        bit_rates: offered
            .bit_rates
            .iter()
            .copied()
            .filter(|&b| asked(&|o| o.audio_bit_rate = (b > 0).then_some(b)))
            .collect(),
    }
}

/// How long one frame of a sound track lasts, where its own packets do not
/// say.
///
/// Most containers write a duration on every audio packet and there is
/// nothing to work out. Matroska does not always: a TrueHD track's access
/// units are 1/1200 of a second each, and it stores them with no length at
/// all, leaving a reader to take that from the spacing of the timestamps.
/// Which is what this does -- and it matters because a frame with no length
/// is a frame the writer has nowhere to put, so a track of them was written
/// as no sound at all while the cut said it was carrying one through byte
/// for byte.
///
/// **Across the whole run rather than gap by gap.** The timestamps are the
/// container's and are kept at the container's resolution: Matroska counts
/// in milliseconds, so the gaps between frames of 1/1200 of a second come
/// out as a ragged mixture of one millisecond and none at all. Every way of
/// picking one gap -- the middle one, the commonest -- answers 1 ms, which
/// is a fifth long. The first timestamp and the last, over the number of
/// frames between them, answer 0.8333 ms, and the longer the run the finer
/// the answer.
///
/// `None` where the packets carry their own durations, which wants nothing
/// assumed, where one arrives without a timestamp, where the run is too
/// short to measure over, and where the timestamps do not climb: a run read
/// across a discontinuity says nothing about the spacing of anything.
///
/// Reading stops at the first packet with a duration on it, so the ordinary
/// case costs one packet.
fn assumed_frame(probe: &mut ff::format::context::Input, info: &crate::AudioInfo) -> Option<f64> {
    const ENOUGH: usize = 256;
    let mut times: Vec<i64> = Vec::new();
    for (stream, packet) in probe.packets().take(8192) {
        if stream.index() != info.stream_index {
            continue;
        }
        if packet.duration() > 0 {
            return None;
        }
        times.push(packet.pts()?);
        if times.len() >= ENOUGH {
            break;
        }
    }
    if times.len() < 32 || times.windows(2).any(|pair| pair[1] < pair[0]) {
        return None;
    }
    let span = (times[times.len() - 1] - times[0]) as f64 * info.time_base;
    let each = span / (times.len() - 1) as f64;
    (each > 0.0).then_some(each)
}

/// Decide what will be done to one sound track.
///
/// Every question here used to be asked once, of the one track there was.
/// Asked per track they are the same questions, and the answers may differ
/// inside one recording: a bilingual broadcast can carry stereo beside dual
/// mono, and folding one of them says nothing about the other.
///
/// `many` only decides whether the notes name which track they are about.
fn plan_audio(
    path: &str,
    info: &crate::AudioInfo,
    opts: &CutOptions,
    to_ts: bool,
    on_a_ts: bool,
    many: bool,
) -> Result<AudioSetup> {
    // Which track these notes are about, said the way the recording names its
    // tracks. See [`crate::track_name`].
    let named = if many {
        format!(
            " on {}",
            crate::track_name(on_a_ts, info.pid, info.stream_index)
        )
    } else {
        String::new()
    };
    let mut probe = crate::input::demux(&path)?;
    // What the recording's own frames are, and how wide their samples come
    // out -- read off the probe that is opened here anyway.
    let (source_id, source_bits) = {
        let params = probe
            .stream(info.stream_index)
            .ok_or_else(|| anyhow!("audio stream {} vanished", info.stream_index))?
            .parameters();
        (params.id(), crate::audio::pcm_bits(&params))
    };
    // How wide the samples are written. The recording's own unless a width
    // was asked for -- and `carriage` is the only thing below that reads it
    // before the codec is settled, which is exactly where it means
    // something: an LPCM track is 16 bit or it is 24, and that is the choice.
    let bits = opts.audio_bits.unwrap_or(source_bits);
    // Asked before the framing below, and off the same probe: it answers on
    // the first packet of a track whose packets carry their own durations,
    // which is every ordinary one, and so reads nothing the framing was
    // going to read.
    let frame_secs = assumed_frame(&mut probe, info);
    let source_adts = crate::aac::framing(&mut probe, info.stream_index);
    drop(probe);
    // What the track is written as. Settled before the mode, because it can
    // decide it: there is no copying a frame into a codec it is not in.
    let target = carriage(source_id, to_ts, bits, opts.audio_codec);
    // Whether the codec on the way out is one the caller named rather than
    // the one that came in. `carriage` can arrive at the same answer either
    // way -- AAC asked of an AAC recording is the recording's own codec --
    // and where it does there is nothing here to explain or force.
    let recoded = target != source_id;
    let asked_for = recoded && opts.audio_codec != AudioCodec::Source;
    // Lossless sound is never re-encoded here -- see `carried_whole` in
    // [`crate::audio`]. A whole-track re-encode of one, or a downmix, which
    // is a re-encode by another name, is not something to fail the cut over
    // ("no encoder for TRUEHD"): it is something to decline out loud and
    // carry the recording's own frames instead.
    //
    // Naming a codec is the one thing that overrules it. Declining to take
    // a recording's last bit away is a kindness only while nobody has asked
    // for AAC by name; asked, it is a program refusing to do what it was
    // told, and the encoder it would have refused to open is not the
    // recording's own but the one that was named.
    let lossless = crate::audio::carried_whole(source_id) && !asked_for;
    let asked_channels = opts.audio_channels.unwrap_or(info.channels);
    // Everything about the samples themselves that was asked for and cannot
    // be given, said in one breath: a lossless track is carried through as
    // it is, and each of these is a way of asking for it not to be.
    let declined: Vec<String> = if lossless {
        [
            (asked_channels != info.channels).then(|| {
                format!(
                    "{} channels, not the {asked_channels} asked for",
                    info.channels
                )
            }),
            opts.audio_sample_rate
                .filter(|&r| r != info.sample_rate)
                .map(|r| format!("{} Hz, not the {r} asked for", info.sample_rate)),
            opts.audio_bits
                .filter(|&b| b != source_bits)
                .map(|b| format!("{source_bits} bit samples, not the {b} bit asked for")),
            // A rate is what an encoder is told to spend, and there is no
            // encoder here. Said with the rest rather than left to the
            // encoder-rate check below, which no longer looks at a track
            // that is carried through.
            opts.audio_bit_rate
                .map(|r| format!("its own frames, not the {r} bit/s asked for")),
        ]
        .into_iter()
        .flatten()
        .collect()
    } else {
        Vec::new()
    };
    if lossless && (!declined.is_empty() || opts.audio_mode == AudioMode::Reencode) {
        eprintln!(
            "note: {source_id:?}{named} is lossless sound and every encoder here would take \
             that away from it, so it is carried through as it is{}. Its boundaries land on \
             whole frames.",
            if declined.is_empty() {
                String::new()
            } else {
                format!(" -- {}", declined.join(", "))
            },
        );
    }
    // Channels out, and whether that is a downmix. A downmix is the one audio
    // setting that decides the mode rather than living under it: there is no
    // copying a 5.1 frame into a stereo track, so it is a whole-track
    // re-encode or it is nothing.
    let channels = if lossless {
        info.channels
    } else {
        asked_channels
    };
    let downmix = (channels != info.channels).then_some((info.channels, channels));
    // The rate the track is written at. What was asked for, taken to the
    // nearest the codec being written can actually speak -- AC-3 has three
    // and MP2 six, and an encoder handed a rate it does not list refuses to
    // open at all.
    let asked_rate = if lossless {
        info.sample_rate
    } else {
        opts.audio_sample_rate.unwrap_or(info.sample_rate)
    };
    let sample_rate = crate::audio::writable_rate(target, asked_rate);
    if sample_rate != asked_rate {
        eprintln!(
            "note: {asked_rate} Hz was asked for{named} and {target:?} is not written at that \
             rate, so the track is written at {sample_rate} Hz, which is the nearest it can be."
        );
    }
    let resampled = (sample_rate != info.sample_rate).then_some((info.sample_rate, sample_rate));
    // A width asked for only reaches a codec that carries samples. Every
    // lossy encoder here takes a float and writes a description of the
    // sound; how many bits the sound had before it is not a number one of
    // them has anywhere to put.
    let requantised = if crate::audio::uncompressed(target) {
        // Only where the recording carries samples of its own for the new
        // width to be a change *from*, and only where the codec was not
        // named: a codec asked for by name is already a whole-track
        // re-encode and already says so below, and the width it is written
        // at is part of that one answer rather than a second one.
        (!lossless && !asked_for && bits != source_bits).then_some((source_bits, bits))
    } else {
        if let Some(b) = opts.audio_bits.filter(|_| !lossless) {
            eprintln!(
                "note: {b} bit samples were asked for{named} and {target:?} does not carry \
                 samples but a description of them, so there is nowhere in it to put a width. \
                 The setting is left aside; --audio-bitrate is what decides the size of a \
                 lossy track."
            );
        }
        None
    };
    // The three ways of asking for a sample that is not the recording's
    // sample. Each one leaves nothing to copy -- there is no putting a
    // stereo frame among 5.1 ones, and no more putting a 44.1 kHz frame
    // among 48 kHz ones -- so any of them is a whole-track re-encode or it
    // is nothing.
    let rebuild = downmix
        .map(|(from, to)| format!("{from} channels were asked for as {to}"))
        .or_else(|| resampled.map(|(from, to)| format!("{from} Hz was asked for as {to} Hz")))
        .or_else(|| {
            requantised.map(|(from, to)| format!("{from} bit samples were asked for as {to} bit"))
        });
    let mode = match rebuild {
        Some(what) if opts.audio_mode != AudioMode::Reencode => {
            eprintln!(
                "note: {what}{named}, which no frame of the recording's own can be copied \
                 through, so the whole track is re-encoded rather than {}.",
                opts.audio_mode.as_str(),
            );
            AudioMode::Reencode
        }
        Some(_) => AudioMode::Reencode,
        // The one mode a lossless track cannot be given. The other two copy
        // its frames already, and smart mode says for itself why it did.
        _ if lossless && opts.audio_mode == AudioMode::Reencode => AudioMode::Copy,
        _ => opts.audio_mode,
    };
    // Where the codec on the way out is not the codec on the way in there is
    // nothing to copy: every frame is written afresh, whatever was asked for.
    let mode = match (recoded, asked_for) {
        (false, _) => mode,
        // Asked for by name. Worth a note only where it overrules something
        // the caller also said -- a mode that copies frames cannot be run
        // on frames that have to be built.
        (true, true) => {
            if opts.audio_mode != AudioMode::Reencode {
                eprintln!(
                    "note: {target:?} was asked for{named} and the recording carries \
                     {source_id:?}, which no frame of can be copied through, so the whole \
                     track is re-encoded rather than {}.",
                    opts.audio_mode.as_str(),
                );
            }
            AudioMode::Reencode
        }
        // Not asked for: the container has no box for what the recording
        // carries, which is a thing to say out loud.
        (true, false) => {
            // Two ways in and one sentence for both. A Blu-ray's LPCM sits
            // in a private stream only a transport stream declares, so an
            // MP4 takes the samples as plain PCM instead -- and there a
            // transport stream really would keep the track as it was. A
            // DVD's sits in a private stream nothing declares, so it becomes
            // the Blu-ray's on the way into a transport stream and plain PCM
            // anywhere else; there is no container that keeps that one, and
            // saying otherwise would send someone to write a `.ts` for it.
            let keep = if source_id == ff::codec::Id::PCM_BLURAY {
                " Write a .ts to keep it as it was."
            } else {
                ""
            };
            eprintln!(
                "note: {source_id:?}{named} is written as {target:?}, which is the box this \
                 container has for linear PCM. The samples are the recording's own; what \
                 changes is what is written around them.{keep}"
            );
            AudioMode::Reencode
        }
    };
    // Asking for the AAC the recording does not carry only reaches the frames
    // written here -- the copied ones keep the headers they came with -- so
    // honouring it would leave the output two kinds of AAC at once, which is
    // the very thing this framing exists to prevent. A whole-track re-encode
    // is the exception: nothing is copied there, so there is nothing to
    // disagree with.
    let aac = match (opts.aac.forced(), source_adts) {
        (Some(want), Some(crate::aac::Framing::Adts(f)))
            if f.mpeg2 != want && mode != AudioMode::Reencode =>
        {
            eprintln!(
                "note: --aac {} was asked for, but this recording carries MPEG-{} AAC{} and \
                 its own frames are copied unchanged. Writing the few frames this cut \
                 encodes as MPEG-{} would leave the audio two kinds of AAC at once, so the \
                 recording's own is followed instead. --audio-mode reencode writes every \
                 frame, and can be asked for either.",
                opts.aac.as_str(),
                if f.mpeg2 { 2 } else { 4 },
                named,
                if want { 2 } else { 4 },
            );
            AacVersion::Auto
        }
        _ => opts.aac,
    };
    // Whether the frames this cut encodes are written framed.
    //
    // In smart mode they must be: they are spliced in among the recording's
    // own frames, which are framed, and a track has to be one thing or the
    // other for a muxer to handle it -- MPEG-TS passes a framed packet
    // through untouched, MP4 runs the whole track through `aac_adtstoasc`.
    // A whole-track re-encode has no copied frames to match, so it keeps to
    // raw AAC and the encoder's own parameters, except into a transport
    // stream, where framing them here is the only way to say MPEG-2.
    //
    // A downmixed frame is also a frame with a different channel count, and
    // a resampled one a frame at a different rate; the header is where a
    // transport stream says both, so both have to be said again there.
    //
    // And only where AAC is what is being written. ADTS is AAC's framing and
    // nothing else's: a header in front of an AC-3 frame is six bytes of
    // nonsense that a decoder will try to read as a frame.
    let frame_as = match (mode, source_adts) {
        // Either AAC: `aac` is what an HD recording's frames are declared as
        // and `aac_latm` what a 4K recording's are, and a frame written for
        // one of those tracks is framed the way the track's own frames are.
        _ if !matches!(target, ff::codec::Id::AAC | ff::codec::Id::AAC_LATM) => None,
        (AudioMode::Smart, Some(f)) | (AudioMode::Reencode, Some(f))
            if mode == AudioMode::Smart || to_ts =>
        {
            // The header says what the frame behind it is, and the frames
            // behind these are the ones written here rather than the ones
            // the recording opened with. Both numbers are restated whether
            // or not anything asked them to change: a fold and a resample
            // are the two ways a caller moves them, and a recording that
            // opens on the programme before it can have had either wrong
            // from the start -- see [`crate::audio::settled_shape`]. Where
            // nothing has moved, restating them writes the same header.
            Some(
                f.as_version(aac)
                    .with_channels(channels)
                    .with_rate(sample_rate),
            )
        }
        _ => None,
    };
    // Following the recording's own rate is right until the channels stop
    // being the recording's: 384 kbit/s is what 5.1 cost, not what the stereo
    // it was folded into is worth, so the derived rate comes down with the
    // channel count. An explicit rate is taken as given.
    //
    // Following it says nothing at all once the codec is not the recording's:
    // what a broadcast spent on AAC is not what the same programme is worth
    // as AC-3, and as DTS it is a rate the format does not have. There the
    // codec's own figure for the channel count is the derived one.
    let bit_rate = opts.audio_bit_rate.unwrap_or_else(|| {
        if recoded {
            return derived_bit_rate(target, channels);
        }
        // What the recording spent, where it said. Where it did not -- a
        // transport stream states a bitrate for almost nothing it carries --
        // a figure has to stand in, and the 192 kbit/s that stood in is a
        // stereo figure. On 5.1 it is half what the recording itself spent,
        // and the frames written at a seam come back at half the size of the
        // ones they are spliced between. So the stand-in follows the channel
        // count upward. Never downward: writing a mono track's few
        // re-encoded frames at the stereo figure costs a few bytes and loses
        // nothing, and quietly halving what a track is written at on a guess
        // is not the same kind of act.
        let src_rate = info
            .bit_rate
            .unwrap_or_else(|| derived_bit_rate(target, channels).max(192_000));
        match downmix {
            Some((from, to)) if from > 0 => (src_rate * to as usize / from as usize).max(128_000),
            _ => src_rate,
        }
    });
    // A figure the encoder will not open at is not a cut to stop over. DTS
    // has a floor -- its frame carries a fixed number of samples and has to
    // be long enough to describe every channel in it -- and the floor moves
    // with the channels and the rate, so a rate that was right for the
    // recording can be under it once the channels are the output's: 5.1 at
    // 48 kHz is not written under about 670 kbit/s. What the codec is
    // ordinarily carried at is above every such floor, and is a figure the
    // format actually has, so that is written instead and said. The window
    // does not reach this -- it is told what will open before anyone
    // chooses, by [`writable_sound`] -- but a command line and a project
    // written before any of this both can.
    //
    // And none of it is a question about a track no encoder is opened for.
    // A lossless track is carried through frame by frame, so the rate above
    // is a number nothing reads -- and where the recording never said what
    // its own rate was, that number is whatever stood in for it, which the
    // encoder named beside it need never have accepted. A cut of a Blu-ray
    // said so twice per track, in the same breath as saying the track was
    // being carried through untouched.
    let ordinary = derived_bit_rate(target, channels);
    let refused = bit_rate > 0
        && !lossless
        && !crate::audio::opens_at(target, sample_rate, channels, bit_rate);
    let bit_rate = if refused && crate::audio::opens_at(target, sample_rate, channels, ordinary) {
        eprintln!(
            "note: {bit_rate} bit/s was asked for{named} and {target:?} is not written at that \
             rate with {channels} channels at {sample_rate} Hz, so the track is written at \
             {ordinary} bit/s, which is what it is ordinarily carried at."
        );
        ordinary
    } else {
        bit_rate
    };
    // What the encoder is asked to take. Ordinarily the recording's own
    // samples, which is what keeps a Blu-ray's frames the length they were.
    // Two things are the exception, and a width asked for outright is the
    // plainer of them. The other is a codec asked for by name: a lossy
    // recording decodes to a float, which says nothing about how many bits
    // it had, and the LPCM encoder handed one reaches for the widest width
    // it has -- 24 bits of a recording that never had 16, half again the
    // size and not one sample better. The width settled on above is the one
    // meant in both cases.
    let width = if bits > 16 {
        ff::format::Sample::I32(ff::format::sample::Type::Packed)
    } else {
        ff::format::Sample::I16(ff::format::sample::Type::Packed)
    };
    let chose_width = asked_for || opts.audio_bits.is_some();
    let like = (chose_width && crate::audio::uncompressed(target)).then_some(width);
    Ok(AudioSetup {
        info: info.clone(),
        mode,
        target,
        recoded,
        like,
        channels,
        sample_rate,
        downmix,
        frame_as,
        bit_rate,
        frame_secs,
    })
}

/// How a transport stream declares a codec, for the map the graft writes.
///
/// The same answers the muxer gives -- these are read off what it wrote,
/// codec by codec -- except for LPCM, where the muxer has two answers and
/// only one of them can be read back. See [`crate::si::Declared`].
///
/// `None` for a codec that is not written into a transport stream at all,
/// which is most of them: the big-endian PCM a cut writes into an MP4 has no
/// business in this table, because nothing that reaches this function is
/// going anywhere but a transport stream.
fn declared_as(target: ff::codec::Id) -> Option<crate::si::Declared> {
    let plain = |stream_type| {
        Some(crate::si::Declared {
            stream_type,
            descriptors: Vec::new(),
            program_info: Vec::new(),
        })
    };
    match target {
        // ADTS AAC, which is what everything here frames it as.
        ff::codec::Id::AAC => plain(0x0F),
        // 0x81 and a registration descriptor saying AC-3, which is what a
        // receiver looks for.
        ff::codec::Id::AC3 => Some(crate::si::Declared {
            stream_type: 0x81,
            descriptors: vec![0x05, 0x04, b'A', b'C', b'-', b'3'],
            program_info: Vec::new(),
        }),
        ff::codec::Id::EAC3 => plain(0x87),
        ff::codec::Id::DTS => plain(0x82),
        // 0x80 is HDMV LPCM only in a stream that has registered itself as
        // HDMV, and means other things in one that has not -- so the
        // registration goes in with it or neither does.
        ff::codec::Id::PCM_BLURAY => Some(crate::si::Declared {
            stream_type: 0x80,
            descriptors: Vec::new(),
            program_info: vec![0x05, 0x04, b'H', b'D', b'M', b'V'],
        }),
        ff::codec::Id::MP2 => plain(0x04),
        _ => None,
    }
}

/// Put the recording's own tables into the finished cut.
///
/// Reading what was on the air is done per kept range rather than once,
/// because a recording can span a programme boundary and a cut across one
/// should describe both sides of it. The read is a windowed one at the byte
/// the range's opening picture arrives at -- the access-point index knows
/// that byte, which is what makes this cheap on a file of several gigabytes.
#[allow(clippy::too_many_arguments)]
fn graft_tables(
    src: &Source,
    service: &crate::si::Service,
    setups: &[AudioSetup],
    captions: &[crate::CaptionInfo],
    graphics: &[crate::GraphicsInfo],
    converted: &[crate::SubpictureInfo],
    data: &[u16],
    video_pid: i32,
    pids: &Pids,
    range_starts: &[f64],
    plans: &[RangePlan],
    output: &str,
    on: Option<&(dyn Fn(f64) + Sync)>,
    tables: crate::si::Tables,
) -> Result<crate::si::Stats> {
    // A disc's own stream says once, on the pictures, that the stream types
    // in its map are Blu-ray's. A recorder writes it; a broadcast has no
    // reason to, so where the cut is a disc's it is added here. The same two
    // bytes the clip index will carry about the same stream go in with it,
    // which is what a recorder's own disc puts there.
    let registration = if !pids.bluray {
        Vec::new()
    } else {
        crate::si::hdmv_registration(
            service
                .stream(pids.video(video_pid) as u16)
                .or_else(|| service.stream(video_pid as u16))
                .map_or_else(
                    || crate::bdav::video_coding(&src.video.codec),
                    |es| es.stream_type,
                ),
            &crate::bdav::video_attributes(&src.video),
        )
    };
    let mut streams = vec![crate::si::GraftStream {
        pid: pids.video(video_pid) as u16,
        was: video_pid as u16,
        faithful: true,
        declared: None,
        language: None,
        extra: registration,
    }];
    for (nth, setup) in setups.iter().enumerate() {
        streams.push(crate::si::GraftStream {
            pid: pids.audio(nth, setup.info.pid) as u16,
            was: setup.info.pid as u16,
            // A folded track no longer has the channels the recording's own
            // audio component descriptor names, and saying it does is worse
            // than saying nothing. A track in another codec is further from
            // the description again.
            faithful: setup.downmix.is_none() && !setup.recoded,
            declared: setup.recoded.then(|| declared_as(setup.target)).flatten(),
            language: setup.info.language.clone(),
            extra: Vec::new(),
        });
    }
    for (nth, c) in captions.iter().enumerate() {
        streams.push(crate::si::GraftStream {
            pid: pids.caption(nth, c.pid) as u16,
            was: c.pid as u16,
            faithful: true,
            declared: None,
            language: c.language.clone(),
            extra: Vec::new(),
        });
    }
    // The disc's own subtitles are declared here rather than left to
    // whatever the map said, because there are two answers and only one of
    // them can be read back. Asked for Blu-ray's own framing the muxer says
    // 0x90 and everything understands it; asked for a plain `.ts` it says
    // private data of no stated kind, and everything reads that back as
    // `bin_data` -- carried, declared, and invisible. The bytes do not
    // differ, so the first declaration is the true one either way. It means
    // 0x90 only in a stream that has registered itself as HDMV, so the
    // registration goes in with it; where the recording's own map already
    // carried one, this adds nothing. See [`crate::si::Declared`].
    for (nth, g) in graphics.iter().enumerate() {
        streams.push(crate::si::GraftStream {
            pid: pids.graphics(nth, g.pid) as u16,
            was: g.pid as u16,
            faithful: true,
            declared: Some(crate::si::Declared {
                stream_type: 0x90,
                descriptors: Vec::new(),
                program_info: vec![0x05, 0x04, b'H', b'D', b'M', b'V'],
            }),
            language: g.language.clone(),
            extra: Vec::new(),
        });
    }
    // A DVD's subtitles converted into that same kind, which the map has to
    // name the same way -- and here there is no recording's own entry to
    // fall back on, since the stream is this program's own. See
    // [`Subtitles`].
    for (nth, c) in converted.iter().enumerate() {
        streams.push(crate::si::GraftStream {
            pid: pids.graphics(graphics.len() + nth, c.id) as u16,
            was: c.id as u16,
            faithful: true,
            declared: Some(crate::si::Declared {
                stream_type: 0x90,
                descriptors: Vec::new(),
                program_info: vec![0x05, 0x04, b'H', b'D', b'M', b'V'],
            }),
            language: c.language.clone(),
            extra: Vec::new(),
        });
    }

    // The data broadcast is announced the way the recording announced it:
    // the map's own entry, with the data component descriptor beside it that
    // says which kind of carousel this is. There is nothing to correct --
    // what goes into the file is the packets that came out of the recording,
    // byte for byte -- and a stream named as faithful is one whose
    // description in the programme and the event still stands.
    for pid in data {
        streams.push(crate::si::GraftStream {
            pid: *pid,
            was: *pid,
            faithful: true,
            declared: None,
            language: None,
            extra: Vec::new(),
        });
    }

    let ranges: Vec<crate::si::GraftRange> = plans
        .iter()
        .zip(range_starts)
        .map(|(plan, &start)| {
            let anchor = src
                .points
                .iter()
                .rfind(|p| p.time <= plan.t_in + 1e-6 && p.pos >= 0);
            let pos = anchor.map_or(0, |p| p.pos);
            let snapshot =
                crate::si::snapshot_at(&src.input, pos, service.service_id).unwrap_or_default();
            // Where this range sits in the recording, for the streams read
            // out of it directly. Only where the index knows the byte: the
            // alternative is reading from the head of the file to find a
            // range forty minutes in, which is gigabytes to carry a stream
            // that is a hundredth of them. See [`crate::carousel::Span`].
            let source = anchor.map(|p| crate::carousel::Span {
                pos: p.pos,
                skip: plan.t_in - p.time,
                length: plan.t_out - plan.t_in,
            });
            crate::si::GraftRange {
                start,
                snapshot,
                source,
            }
        })
        .collect();
    if !data.is_empty() && ranges.iter().any(|r| r.source.is_none()) {
        eprintln!(
            "note: the index does not know which byte every kept range opens on, so the data \
             broadcast is carried only for those it does know."
        );
    }

    crate::si::graft(
        output,
        on,
        &crate::si::Graft {
            service,
            streams,
            pcr_pid: pids.video(video_pid) as u16,
            ranges,
            tables,
            input: (!data.is_empty()).then_some(&src.input),
            carry: data.to_vec(),
            // The recording's own clock, which is not always on its
            // pictures: a satellite service in the sample keeps it on a PID
            // of its own, and the cut puts it back in the pictures.
            source_pcr_pid: if service.pcr_pid > 0 {
                service.pcr_pid
            } else {
                video_pid as u16
            },
        },
    )
}

pub fn cut(src: &Source, plans: &[RangePlan], output: &str, opts: &CutOptions) -> Result<()> {
    cut_with_progress(src, plans, output, opts, None)
}

/// Which of a cut's two passes a report is about.
///
/// A transport stream is written twice: once by the muxer, and once again by
/// the pass that puts the recording's own tables back over what the muxer
/// wrote -- which is a read and a write of the whole finished file. See
/// [`crate::si::graft`]. Until this said so, the second pass reported nothing
/// at all: the bar reached the end of the first one and the window then sat
/// still for as long again.
///
/// A report carries two figures, because a caller has two things to show and
/// they are not the same number. **A bar is about the job** and runs once
/// from nought to one across both passes. **Anything that follows the
/// writing head is about the pass**, and the second pass has no writing head
/// to follow: it is copying a finished file and putting tables and the data
/// broadcast into it, with nothing left to encode. Given the job's figure,
/// the output screen's stage reached seven tenths of the way through the cut
/// while the pictures were being written and then walked the rest of it
/// during the pass that writes the data broadcast -- pictures moving under a
/// line that says the tables are going in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pass {
    /// Copying and re-encoding into the output.
    Writing,
    /// Putting the recording's own tables back over the muxer's.
    Tables,
}

/// How much of the whole the writing pass is, where the tables are going back
/// in as well.
///
/// Measured on a 700 MB broadcast recording copied whole: 3.16 s to write it
/// and 1.29 s to visit it again. It is only an estimate for other material --
/// a cut that re-encodes a great deal spends longer in the first pass than
/// this says -- but an estimate is what a bar is, and standing still at the
/// end of the first pass was not an estimate, it was wrong.
const WRITING_SHARE: f64 = 0.7;

/// How a cut says where it has got to: which pass it is in, how far through
/// the whole job that is, and how far through the pass itself.
pub type Report = Box<dyn Fn(Pass, f64, f64) + Send + Sync>;

/// As [`cut`], reporting how far along it is.
pub fn cut_with_progress(
    src: &Source,
    plans: &[RangePlan],
    output: &str,
    opts: &CutOptions,
    progress: Option<Report>,
) -> Result<()> {
    crate::init()?;

    // Nothing kept is not a short cut, it is a job with no output in it.
    // Refused before the file is created rather than after: a plan of no
    // ranges used to run the whole way through and leave a nought-byte file
    // behind, reported as written. Every caller can reach this -- the window
    // by cutting a clip away entirely, the command line by `--cut` covering
    // the recording -- so it is answered here rather than in each of them.
    if plans.is_empty() {
        anyhow::bail!("nothing is kept: every part of this recording has been cut away");
    }

    let (ictx, ist_index) = open_input(&src.input.url)?;
    let params = ictx.stream(ist_index).unwrap().parameters();
    let extradata = unsafe {
        let p = params.as_ptr();
        if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec()
        }
    };
    let grid = Grid::of(src);
    let den = grid.den;

    // Output timebase: one tick per 1/(2*num*sub) second, so a *unit* is
    // exactly `den` ticks, a field is `sub` of them and a normal frame twice
    // that. 30000/1001 lands on 1/60000 with 1001 ticks per field -- integer
    // arithmetic throughout, no rounding anywhere on the timeline, and
    // 3-field pictures are representable. See [`Grid`] for the recording
    // that needs the units smaller than a field.
    let mut muxer_opts = ff::Dictionary::new();
    let timescale = grid.timescale().to_string();
    let to_ts = writing_ts(output);
    // Writing another transport stream means keeping the one the recording
    // already had. Its PIDs, its service number, the language on its audio --
    // the tools downstream of a broadcast recording are built around finding
    // those where broadcasts put them, and numbering a fresh set from 0x100
    // makes the output look like something else entirely.
    let layout = ts_layout(&ictx, src.video.stream_index);
    let video_pid = ictx.stream(src.video.stream_index).map_or(0, |s| s.id());
    // Which of the recording's streams are being written. Everything it
    // carries that a cut can carry, less whatever the caller named.
    let kept = |i: usize| !opts.drop_streams.contains(&i);
    let audios: Vec<crate::AudioInfo> = src
        .audios
        .iter()
        .filter(|a| kept(a.stream_index))
        .cloned()
        .collect();
    // Captions go into a transport stream and nowhere else. MP4 has no
    // sample entry for an ARIB caption stream -- there is nothing to declare
    // it as, and no format to turn it into that is still what it was.
    let captions: Vec<crate::CaptionInfo> = if to_ts {
        src.captions
            .iter()
            .filter(|c| kept(c.stream_index))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if !to_ts && !src.captions.is_empty() {
        eprintln!(
            "note: this recording carries {} caption stream(s), which only a transport \
             stream can hold. Write a .ts to keep them.",
            src.captions.len(),
        );
    }
    // What a DVD draws, less whatever the caller named. Where these go is
    // [`Subtitles`]; that they are kept at all is decided here, with the
    // rest.
    let subpictures: Vec<crate::SubpictureInfo> = src
        .subpictures
        .iter()
        .filter(|s| !opts.drop_subpictures.contains(&s.id))
        .cloned()
        .collect();

    // A disc's own subtitles, on the same terms and for the same reason: MP4
    // has no box for a graphics stream either. Where they go is [`Subtitles`]:
    // carried inside the cut, which only a transport stream can do, or read
    // back and written beside it as the pair a DVD's subtitles travel in,
    // which any output can be given.
    let inside = opts.subtitles == Subtitles::Pgs;
    let beside = opts.subtitles == Subtitles::Beside;
    let sup = opts.subtitles == Subtitles::Sup;
    let graphics: Vec<crate::GraphicsInfo> = if to_ts && inside {
        src.graphics
            .iter()
            .filter(|g| kept(g.stream_index))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let aside: Vec<crate::GraphicsInfo> = if beside || sup {
        src.graphics
            .iter()
            .filter(|g| kept(g.stream_index))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if inside && !to_ts && !src.subpictures.is_empty() {
        eprintln!(
            "note: the subtitles a DVD draws can only be converted into a transport \
             stream, which this output is not. They are written beside the cut instead."
        );
    }
    if !to_ts && inside && !src.graphics.is_empty() {
        eprintln!(
            "note: this recording carries {} subtitle stream(s) drawn the way a disc draws \
             them, which only a transport stream can hold. Write a .ts or a .m2ts to keep \
             them, or ask for them beside the cut -- as an .idx and .sub pair, or as a \
             .sup -- and they are written whatever this is.",
            src.graphics.len(),
        );
    }
    // And where each of them goes. The recording's own PIDs, unless what is
    // being written is a Blu-ray's own framing; see [`Pids`].
    // A converted subtitle stream is drawn the way a disc draws one, so it
    // is numbered with those, after the ones the recording carried. See
    // [`Subtitles`].
    let pids = if writing_m2ts(output) {
        Pids::bluray()
    } else {
        Pids::kept()
    };
    // What each sound track is and what will become of it. Settled before
    // anything is declared, because a stream has to be declared as the thing
    // it will contain and a downmixed track is not what the recording's own
    // parameters describe.
    let setups: Vec<AudioSetup> = audios
        .iter()
        .map(|a| {
            plan_audio(
                &src.input.url,
                a,
                opts,
                to_ts,
                src.on_a_ts,
                audios.len() > 1,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    // Whether the container this is going into can hold what is going into
    // it. Answered before the output is created rather than at the end of
    // the cut, and answered here rather than left to the muxer, because one
    // of them does not answer: a transport stream handed a codec it has no
    // stream type for declares it as private data and writes it anyway, so
    // the cut finishes and the file plays as nothing at all. See
    // [`crate::carry`].
    {
        let family = crate::carry::family(output);
        let ext = std::path::Path::new(output)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("file")
            .to_ascii_lowercase();
        let refuse = |codec: &str, what: &str| -> anyhow::Error {
            let how = if family == "ts" {
                "would be written as private data of no stated kind, which is a stream every \
                 player carries and none can read"
            } else {
                "has no box for it"
            };
            anyhow!(
                "a .{ext} cannot carry {codec} {what}: it {how}. A .mkv holds everything this \
                 program writes, and the output settings screen greys out the ones that cannot \
                 hold a given recording"
            )
        };
        if !crate::carry::holds(family, &src.video.codec) {
            return Err(refuse(&src.video.codec, "pictures"));
        }
        for setup in &setups {
            let codec = crate::carry::name_of(setup.target);
            if !crate::carry::holds(family, &codec) {
                return Err(refuse(&codec, "sound"));
            }
        }
    }

    // What the recording says about itself. Read here rather than after the
    // cut because the muxer's own idea of the transport stream has to agree
    // with it: an event information section names its service by transport
    // stream and by network, and a player that finds those disagreeing with
    // the tables around them is right to believe neither.
    // Which of the three shapes this cut is being written in, settled once
    // here so that what is read off the recording and what is written back
    // are answering the same question. See [`tables_for`].
    let want = tables_for(output, opts.tables);
    let wants_tables = to_ts && want != crate::si::Tables::Muxer;
    let ours = u16::try_from(video_pid).unwrap_or(0);
    // The data broadcast, which is carried by the table pass rather than by
    // the muxer and so is only possible in the shapes that pass writes; see
    // [`can_carry_data_broadcast`]. What is asked for here is what the
    // demuxer called data; which of those are really a carousel is settled
    // against the recording's own map below, where the map has been read.
    // See [`crate::carousel`].
    let wants_data = opts.data_broadcast.unwrap_or(true);
    let asked_for_data = wants_data && can_carry_data_broadcast(output, opts.tables);
    let maybe_data: Vec<u16> = if asked_for_data {
        src.dropped
            .iter()
            .filter(|d| d.what == "data" && d.pid > 0)
            .map(|d| d.pid as u16)
            .collect()
    } else {
        Vec::new()
    };
    // Only where it was asked for outright. The default is an answer about
    // what suits the output, and an output that cannot hold one is not a
    // disappointment to report.
    if opts.data_broadcast == Some(true)
        && !asked_for_data
        && src.dropped.iter().any(|d| d.what == "data")
    {
        eprintln!(
            "note: a data broadcast can only be carried into a plain .ts that keeps the \
             broadcast's own tables -- it is written by the pass that puts those back, and \
             nothing else can write it. It is left out of this one."
        );
    }
    // Every stream the cut is going to carry, so the map that comes back is
    // one that describes them all rather than whichever arrived first. See
    // [`crate::si::read_service`].
    let carried: Vec<u16> = std::iter::once(ours)
        .chain(audios.iter().map(|a| a.pid as u16))
        .chain(captions.iter().map(|c| c.pid as u16))
        .chain(graphics.iter().map(|g| g.pid as u16))
        .chain(maybe_data.iter().copied())
        .filter(|pid| *pid != 0)
        .collect();
    let tables = match wants_tables.then(|| crate::si::read_service(&src.input, ours, &carried)) {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            eprintln!("note: {e}. The streams are kept; the broadcast's own tables are not.");
            None
        }
        None => None,
    };
    // Which of those streams is really a data broadcast, as against the
    // other things a demuxer has no name for. The map is what says so, and
    // the map has now been read: a carousel is sent as DSM-CC sections and
    // announces itself as such. Anything else the demuxer could not place
    // stays where it was left.
    let data: Vec<u16> = tables.as_ref().map_or_else(Vec::new, |service| {
        maybe_data
            .iter()
            .copied()
            .filter(|pid| {
                service
                    .stream(*pid)
                    .is_some_and(|es| es.stream_type == crate::carousel::DSMCC_SECTIONS)
            })
            .collect()
    });
    // Said only where it was asked for outright and there was something to
    // carry that could not be placed. A recording with no data streams at
    // all raises no question, and a run taking the default is told what was
    // left behind by the listing, which says `not carried` beside it.
    if opts.data_broadcast == Some(true) && data.is_empty() && !maybe_data.is_empty() {
        eprintln!(
            "note: the map this recording opens on does not name its data broadcast as one -- \
             a station takes it out of the map between programmes, and a recording that \
             begins before one starts can open on a map without it. Nothing is carried."
        );
    }

    // The muxer's options are strings, and have to outlive the dictionary
    // they go into.
    let first_pid;
    let pmt;
    let service;
    let tsid;
    let onid;
    let service_type;
    if to_ts {
        if let Some(l) = layout {
            first_pid = l.first_pid.to_string();
            pmt = l.pmt_pid.to_string();
            service = l.service_id.to_string();
            // Which service this is, always: the table this program writes
            // over the muxer's names the recording's own service, and a
            // program number in the map that the list of programmes does not
            // have is a file a player cannot follow from one to the other.
            if l.service_id > 0 {
                muxer_opts.set("mpegts_service_id", &service);
            }
            // Where the streams go, only where the recording's own numbering
            // is being kept. Blu-ray's numbering is asked for stream by
            // stream, and its map has a PID of its own that the muxer
            // decides; see [`Pids`].
            if !pids.bluray {
                if l.first_pid > 0 {
                    muxer_opts.set("mpegts_start_pid", &first_pid);
                }
                if l.pmt_pid > 0 {
                    muxer_opts.set("mpegts_pmt_start_pid", &pmt);
                }
            }
        }
        // Where this service sits in its network, which only the recording's
        // own tables can say. Left at the muxer's defaults when they are not
        // being kept -- a made-up network number is no worse than the
        // default one, and nothing downstream will be looking for it.
        if let Some(t) = tables.as_ref() {
            tsid = t.transport_stream_id.to_string();
            onid = t.original_network_id.to_string();
            service_type = t.service_type.to_string();
            if t.transport_stream_id > 0 {
                muxer_opts.set("mpegts_transport_stream_id", &tsid);
            }
            if t.original_network_id > 0 {
                muxer_opts.set("mpegts_original_network_id", &onid);
            }
            if t.service_type > 0 {
                muxer_opts.set("mpegts_service_type", &service_type);
            }
        }
        // How often the list of programmes and the map go out, on a disc.
        //
        // **A tenth of a second is the ceiling, not the target.**
        // libavformat's default is the ceiling itself, and asking for the
        // ceiling is how it gets missed: the muxer writes the pair when the
        // next *frame* falls due after the interval, and the arrival times a
        // disc is written at stretch the wait further. Measured on two discs
        // written that way, the map came every 100 milliseconds on average
        // and as much as 147 apart, which is over. The recorder's own disc
        // runs at 36 and never reaches 100; the authoring tool's at 80 and
        // never reaches 81.
        //
        // Asking for a fiftieth is asking for "every frame", which on the
        // same material comes to 33 and never reaches 79 -- between the two
        // discs, and the recorder's own rate almost exactly. It costs the
        // pair twice over per frame, which is 0.4% of a disc.
        //
        // Only on a disc's own stream: a `.ts` cut is meant to be the
        // recording it came from, and a broadcast repeats its tables at the
        // rate a broadcast repeats them.
        if writing_m2ts(output) {
            muxer_opts.set("pat_period", "0.02");
        }
    }
    // Note: muxer options belong to `write_header`, not to opening the file --
    // `output_with` hands its dictionary to the *protocol*, so anything meant
    // for the muxer is quietly dropped there. This one goes below.
    let mut octx = ff::format::output(&output)?;
    // The lead a disc gives a decoder: how far the first picture is shown
    // after the clock that has to be running to show it arrives.
    //
    // The mpegts muxer adds this to every presentation time and takes it off
    // again for the clock reference it writes, so it is exactly that lead and
    // nothing else. Left at its default of none, a cut came out with its
    // first picture 0.05 seconds after its first clock -- which is the
    // reorder delay and nothing more, and is not time enough to fill the
    // buffer the picture comes out of. Both reference discs open 0.44 and
    // 0.46 seconds in. Only on a disc's own stream: a `.ts` cut is meant to
    // be the recording it came from, and this would be a shift the recording
    // does not have.
    if to_ts && output.to_ascii_lowercase().ends_with(".m2ts") {
        unsafe {
            (*octx.as_mut_ptr()).max_delay = (LEAD_IN * 1e6) as i32;
        }
    }
    let mp4ish = {
        let name = octx.format().name().to_string();
        name.contains("mp4") || name.contains("mov")
    };
    // The timescale worked out above, which is the MP4 family's own option
    // and nobody else's: Matroska counts in milliseconds and has nothing to
    // set. Asked of the muxer rather than of the file name, and asked here
    // rather than with the rest, because an option the muxer does not
    // recognise is one that comes back out of `write_header_with` below --
    // where a `.mkv` was leaving this one behind on every cut.
    if mp4ish {
        muxer_opts.set("video_track_timescale", &timescale);
    }
    // TrueHD in an MP4 is a box libavformat will write but will not vouch
    // for: it is outside the standard, and asked for one without being told
    // that is wanted the muxer stops the cut outright -- "truehd in MP4
    // support is experimental". A Blu-ray's lossless sound is worth more than
    // the refusal, so it is asked for, and said out loud.
    let outside = setups
        .iter()
        .any(|s| matches!(s.target, ff::codec::Id::TRUEHD | ff::codec::Id::MLP));
    if mp4ish && outside {
        unsafe {
            (*octx.as_mut_ptr()).strict_std_compliance = ff::ffi::FF_COMPLIANCE_EXPERIMENTAL;
        }
        eprintln!(
            "note: TrueHD in an MP4 is outside the standard, not every player will find it, \
             and the track has to open on one of the stream's own sync points -- so its \
             sound starts up to a sync interval after the pictures, which is about 13 ms in \
             the streams measured. It is written all the same; a .ts carries it as it was."
        );
    }
    // Only MP4-family containers need the reframing dance; Annex-B containers
    // already carry parameter sets in-band, and their muxers convert as needed.
    let reframe = match (mp4ish, src.video.framing, src.video.codec.as_str()) {
        (true, NalFraming::Length(n), "h264" | "hevc") => {
            let sets = parameter_sets(&src.video.codec, &extradata);
            if sets.is_empty() {
                None
            } else {
                Some(Reframe {
                    nal_length: n,
                    sets,
                })
            }
        }
        _ => None,
    };
    // And the reverse, for the copied pictures of an MP4 being written as a
    // transport stream. The sets ride in front of every key picture, because
    // the container this is going into has nowhere else to keep them.
    let unframe = match (mp4ish, src.video.framing, src.video.codec.as_str()) {
        (false, NalFraming::Length(n), "h264" | "hevc") => Some(Unframe {
            nal_length: n,
            sets: parameter_sets(&src.video.codec, &extradata),
        }),
        _ => None,
    };
    // What the re-encoded pictures have to be told to say. Read once, and
    // only where there is something to write: a cut that copies every
    // picture never opens a decoder for this. Read *here*, before the output
    // declares its streams, because one of the answers -- whether the Dolby
    // Vision can be carried -- decides what the video stream may claim, and
    // by the first seam the header has been written and cannot be taken back.
    let signalling = if plans
        .iter()
        .flat_map(|p| &p.segments)
        .any(|s| s.kind == SegmentKind::Reencode)
    {
        signalling_of(src, opts)
    } else {
        Signalling::default()
    };
    {
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(params);
        ost.set_time_base(grid.time_base());
        unsafe {
            // A stream that says Dolby Vision and hands a player no RPU to
            // drive it is worse off than one that never said so. Where the
            // pictures rewritten at a seam cannot carry it, the claim comes
            // off with them.
            if signalling.has_dovi && !signalling.dovi {
                drop_dovi(ost.parameters().as_mut_ptr());
            }
            // `avc3`/`hev1` say the parameter sets may live in the samples,
            // which is what lets copied and re-encoded pictures carry
            // different ones in the same track.
            (*ost.parameters().as_mut_ptr()).codec_tag = match (&reframe, src.video.codec.as_str())
            {
                (Some(_), "h264") => u32::from_le_bytes(*b"avc3"),
                (Some(_), "hevc") => u32::from_le_bytes(*b"hev1"),
                _ => 0,
            };
            set_pid(&mut ost, to_ts, pids.video(video_pid));
        }
    }
    // A fade needs sound this program is writing. A track copied through is
    // copied through -- that is what copying is -- and a fade asked for on
    // one is a fade that silently does not happen unless it is said.
    if opts.audio_fade > 0.0 {
        for setup in &setups {
            if setup.mode == AudioMode::Copy {
                crate::note_once(format!(
                    "note: the {} audio is copied through as it is, so the {:.2}s fade asked \
                     for at the seams is not on it. Smart rendering or a whole re-encode is what \
                     writes the sound, and only sound this program writes can be faded.",
                    crate::track_name(src.on_a_ts, setup.info.pid, setup.info.stream_index),
                    opts.audio_fade,
                ));
            }
        }
    }
    // Sound rides along beside the pictures: one output track for each the
    // recording carries, each copied packet for packet.
    let mut audio_pending: Vec<(usize, Option<crate::audio::Reencoder>)> = Vec::new();
    for (nth, setup) in setups.iter().enumerate() {
        let params = ictx
            .stream(setup.info.stream_index)
            .ok_or_else(|| anyhow!("audio stream {} vanished", setup.info.stream_index))?
            .parameters();
        // Built before the stream is declared, because when it exists the
        // stream must describe *it* rather than the source.
        let reencoder = match setup.mode {
            AudioMode::Reencode => Some(crate::audio::Reencoder::new(
                params.clone(),
                setup.target,
                setup.like,
                &setup.info,
                setup.channels,
                setup.sample_rate,
                setup.bit_rate,
                setup.frame_as,
            )?),
            _ => None,
        };
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        match reencoder.as_ref() {
            // Framed frames are the same shape the recording's were, so the
            // recording's parameters describe them and the extradata that
            // would otherwise reframe them is not wanted -- unless the
            // channels have changed underneath, in which case the recording's
            // parameters describe a track this file does not contain. A
            // packet that already begins with a sync word is passed through
            // whatever the extradata says, so the encoder's parameters cost
            // the framing nothing.
            Some(re) if setup.frame_as.is_none() || setup.downmix.is_some() => {
                ost.set_parameters(re.parameters())
            }
            _ => ost.set_parameters(params),
        }
        // The recording's parameters describe the frames at the head of the
        // file, and on a broadcast those are the end of the programme before
        // this one: a recording whose every frame is stereo can be described
        // mono by three seconds of the bulletin that ran ahead of it. What
        // the stream declares is what the frames are. See
        // [`crate::audio::settled_shape`]; where nothing was corrected the
        // two already agree and this does nothing.
        unsafe {
            let p = ost.parameters().as_mut_ptr();
            if (*p).ch_layout.nb_channels != i32::from(setup.channels) {
                ff::ffi::av_channel_layout_uninit(&mut (*p).ch_layout);
                ff::ffi::av_channel_layout_default(&mut (*p).ch_layout, i32::from(setup.channels));
            }
            if (*p).sample_rate != setup.sample_rate as i32 {
                (*p).sample_rate = setup.sample_rate as i32;
            }
        }
        // The rate the track is written at, which is the recording's own
        // unless one was asked for.
        ost.set_time_base(ff::Rational::new(1, setup.sample_rate as i32));
        // Carried across so the muxer writes the language descriptor the
        // recording had; without it the audio arrives anonymous -- and on a
        // bilingual recording, anonymous twice over.
        if let Some(lang) = &setup.info.language {
            let mut meta = ff::Dictionary::new();
            meta.set("language", lang);
            ost.set_metadata(meta);
        }
        let out_index = ost.index();
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            set_pid(&mut ost, to_ts, pids.audio(nth, setup.info.pid));
        }
        audio_pending.push((out_index, reencoder));
    }

    // Captions, likewise, and more simply: nothing about them is re-encoded
    // and nothing about them is spliced, so the stream is declared exactly
    // as it arrived. The muxer knows this codec and writes the descriptors
    // that say a Japanese player should look here for subtitles.
    let mut caption_pending: Vec<usize> = Vec::new();
    for (nth, info) in captions.iter().enumerate() {
        let params = ictx
            .stream(info.stream_index)
            .ok_or_else(|| anyhow!("caption stream {} vanished", info.stream_index))?
            .parameters();
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(params);
        ost.set_time_base(ff::Rational::new(1, 90_000));
        if let Some(lang) = &info.language {
            let mut meta = ff::Dictionary::new();
            meta.set("language", lang);
            ost.set_metadata(meta);
        }
        let out_index = ost.index();
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            set_pid(&mut ost, to_ts, pids.caption(nth, info.pid));
        }
        caption_pending.push(out_index);
    }

    // The disc's own subtitles. Declared exactly as they arrived, like the
    // captions -- but where the captions land in a container that has a name
    // for them, these land in one that has a name for them only in Blu-ray's
    // own framing. Asked for a plain `.ts` the muxer writes them as private
    // data of no stated kind, and everything reads that back as `bin_data`:
    // carried, declared, and invisible. The map written over the muxer's own
    // says what they are; see [`declared_as`] and [`crate::si::Declared`].
    let mut graphics_pending: Vec<usize> = Vec::new();
    for (nth, info) in graphics.iter().enumerate() {
        let params = ictx
            .stream(info.stream_index)
            .ok_or_else(|| anyhow!("graphics stream {} vanished", info.stream_index))?
            .parameters();
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(params);
        ost.set_time_base(ff::Rational::new(1, 90_000));
        if let Some(lang) = &info.language {
            let mut meta = ff::Dictionary::new();
            meta.set("language", lang);
            ost.set_metadata(meta);
        }
        let out_index = ost.index();
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            set_pid(&mut ost, to_ts, pids.graphics(nth, info.pid));
        }
        graphics_pending.push(out_index);
    }

    // A DVD's subtitles, converted into the kind a Blu-ray draws: into the
    // file where the caller asked for them inside it, and into a `.sup`
    // beside it where they asked for that -- which needs no stream at all,
    // and is why the index here is an option.
    //
    // The stream is this program's own -- the recording has nothing of the
    // kind in it -- so it is declared from nothing rather than copied from
    // an input stream, and it goes on the number the disc knew the subtitles
    // by (renumbered like everything else where a `.m2ts` is being written).
    let mut converting: Vec<(crate::SubpictureInfo, Option<usize>)> = Vec::new();
    if sup {
        converting.extend(subpictures.iter().map(|info| (info.clone(), None)));
    }
    if inside && to_ts {
        for (nth, info) in subpictures.iter().enumerate() {
            let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
            ost.set_time_base(ff::Rational::new(1, 90_000));
            if let Some(lang) = &info.language {
                let mut meta = ff::Dictionary::new();
                meta.set("language", lang);
                ost.set_metadata(meta);
            }
            let out_index = ost.index();
            unsafe {
                let p = ost.parameters().as_mut_ptr();
                (*p).codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
                (*p).codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE;
                (*p).width = src.video.width as i32;
                (*p).height = src.video.height as i32;
                (*p).codec_tag = 0;
                set_pid(&mut ost, to_ts, pids.graphics(graphics.len() + nth, info.id));
            }
            converting.push((info.clone(), Some(out_index)));
        }
    }

    {
        // Scoped: the leftovers borrow the context, and everything below
        // needs it back. Anything still in here is an option this muxer did
        // not recognise -- worth tripping over while developing.
        let left = octx.write_header_with(muxer_opts)?;
        debug_assert!(
            left.iter().next().is_none(),
            "muxer ignored an option: {:?}",
            left.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>()
        );
    }

    // write_header is free to replace the stream's time base with whatever the
    // container actually uses, so every packet has to be rescaled from the
    // tick scale we built timestamps in into the one that got written.
    let out_tb = octx
        .stream(0)
        .ok_or_else(|| anyhow!("no output stream"))?
        .time_base();

    // Smart mode re-encodes the frames the boundaries fall inside, before any
    // of them is written, so that the pass below can stay a copy with a
    // lookup in it. Two frames per range edge, in a file of tens of
    // thousands -- per track, since where a boundary falls inside a frame is
    // a fact about one track's framing and not about the recording.
    let mut patches: Vec<std::collections::HashMap<i64, crate::audio::Patch>> = Vec::new();
    for setup in &setups {
        let a = &setup.info;
        patches.push(match setup.mode {
            AudioMode::Smart => {
                let windows: Vec<(i64, i64)> = plans
                    .iter()
                    .map(|p| {
                        (
                            (p.t_in * a.sample_rate as f64).round() as i64,
                            (p.t_out * a.sample_rate as f64).round() as i64,
                        )
                    })
                    .collect();
                // The rate the track is written at, settled once in
                // [`plan_audio`]: the frames at a seam are spliced in among
                // the recording's own and have to have been written at what
                // the rest of it was.
                crate::audio::boundary_patches(
                    src,
                    a,
                    &windows,
                    setup.bit_rate,
                    setup.frame_as,
                    opts.audio_fade,
                )?
            }
            _ => Default::default(),
        });
    }
    if std::env::var("SMARTCUT_DEBUG").is_ok() {
        for (setup, p) in setups.iter().zip(&patches) {
            if setup.mode == AudioMode::Smart {
                eprintln!(
                    "  audio 0x{:04x}: {} frame(s) prepared for the boundaries",
                    setup.info.pid,
                    p.len()
                );
            }
        }
    }

    // The muxer is free to replace the time base of every stream it was
    // given: MPEG-TS keeps 90 kHz whatever it is handed, and MP4 counts in
    // the sample rate.
    let audio_tracks: Vec<AudioTrack> = setups
        .iter()
        .zip(audio_pending)
        .zip(patches)
        .map(|((setup, (out_index, reencoder)), patches)| AudioTrack {
            out_index,
            out_tb: octx
                .stream(out_index)
                .map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            in_index: setup.info.stream_index,
            info: setup.info.clone(),
            out_rate: setup.sample_rate,
            mode: setup.mode,
            written: 0,
            prev: None,
            end: None,
            reencoder,
            patches,
            need_sync: mp4ish && matches!(setup.target, ff::codec::Id::TRUEHD | ff::codec::Id::MLP),
            joins_at_sync: matches!(setup.target, ff::codec::Id::TRUEHD | ff::codec::Id::MLP),
            last_out: None,
            dropped: 0,
            frame_secs: setup.frame_secs.unwrap_or(0.0),
            no_length: 0,
        })
        .collect();
    let caption_tracks: Vec<CaptionTrack> = captions
        .iter()
        .zip(caption_pending)
        .map(|(info, out_index)| CaptionTrack {
            out_index,
            out_tb: octx
                .stream(out_index)
                .map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            in_index: info.stream_index,
            in_tb: info.time_base,
            ttml: info.format == crate::TextFormat::Ttml,
            base: info.base,
            written: 0,
            last_out: None,
        })
        .collect();

    // Only the ones that went into the file: what this names is what the map
    // written over the muxer's own has to describe.
    let converted_streams: Vec<crate::SubpictureInfo> = converting
        .iter()
        .filter(|(_, out_index)| out_index.is_some())
        .map(|(info, _)| info.clone())
        .collect();

    let graphics_tracks: Vec<GraphicsTrack> = graphics
        .iter()
        .zip(graphics_pending)
        .map(|(info, out_index)| GraphicsTrack {
            out_index,
            out_tb: octx
                .stream(out_index)
                .map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            in_index: info.stream_index,
            in_tb: info.time_base,
            pid: info.pid,
            written: 0,
            mended: 0,
            plane: Default::default(),
            last_out: None,
            aside: None,
        })
        .collect();
    // The converted streams take a track each, at the end of the same list:
    // what is written on one is a display set either way.
    let mut graphics_tracks = graphics_tracks;
    let mut converted: Vec<Converted> = Vec::new();
    // What each `.sup` written beside the cut is called. Settled over all of
    // them at once because the answer for one depends on the others: the only
    // one takes the cut's own name and cannot where there is a second.
    let sup_langs: Vec<Option<String>> = converting
        .iter()
        .filter(|(_, out_index)| out_index.is_none())
        .map(|(info, _)| info.language.clone())
        .chain(aside.iter().map(|info| info.language.clone()))
        .collect();
    let mut sup_at = 0usize;
    for (info, out_index) in &converting {
        let palette = crate::dvd::subtitles_of(&src.path)
            .map(|s| s.palette)
            .unwrap_or_else(crate::vobsub::Palette::grey);
        let screen = (src.video.width as u16, src.video.height as u16);
        converted.push(Converted {
            track: graphics_tracks.len(),
            id: info.id,
            screen,
            reader: crate::vobsub::Reader::open(&palette, screen)?,
            composer: Default::default(),
            shown: 0,
        });
        // Where it has no stream of the output to be written to, it has a
        // file of its own beside it instead.
        let destination = out_index.is_none().then(|| {
            let tag = sup_tag(&sup_langs, sup_at, info.id);
            sup_at += 1;
            Aside::Sup(crate::pgs::Sup::new(tag))
        });
        graphics_tracks.push(GraphicsTrack {
            out_index: out_index.unwrap_or(usize::MAX),
            out_tb: out_index
                .and_then(|at| octx.stream(at))
                .map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            // No stream to read it off: this one is written, not carried.
            in_index: usize::MAX,
            in_tb: 1.0 / 90_000.0,
            pid: info.id,
            written: 0,
            mended: 0,
            plane: Default::default(),
            last_out: None,
            aside: destination,
        });
    }

    // And the streams going the other way: a Blu-ray's graphics, read back
    // and written beside the cut. A track each, like the rest, and the only
    // ones with no stream in the file to be written to. See [`Aside`].
    //
    // Numbered from 0x20, which is where a DVD numbers its own subtitles and
    // the only convention there is to follow. A recording never has both
    // kinds, so nothing collides.
    let mut aside_streams: Vec<(crate::GraphicsInfo, i32)> = Vec::new();
    for (k, info) in aside.iter().enumerate() {
        let id = 0x20 + k as i32;
        let destination = if sup {
            let tag = sup_tag(&sup_langs, sup_at, info.pid);
            sup_at += 1;
            Aside::Sup(crate::pgs::Sup::new(tag))
        } else {
            aside_streams.push((info.clone(), id));
            Aside::Redrawn(Redrawn {
                id,
                reader: crate::pgs::read::Reader::open((
                    src.video.width as u16,
                    src.video.height as u16,
                ))?,
                building: Vec::new(),
                at: None,
                pending: None,
            })
        };
        graphics_tracks.push(GraphicsTrack {
            // No stream: this one is turned aside before it reaches the file.
            out_index: usize::MAX,
            out_tb: 1.0 / 90_000.0,
            in_index: info.stream_index,
            in_tb: info.time_base,
            pid: info.pid,
            written: 0,
            mended: 0,
            plane: Default::default(),
            last_out: None,
            aside: Some(destination),
        });
    }

    // The pair beside the cut, if anything is going into it: a DVD's own
    // subtitles, or a Blu-ray's read back into the same shape.
    //
    // A DVD's palette is not in the stream -- see [`crate::vobsub`] -- so the
    // disc is asked for it here, which is one small read of an index. A
    // Blu-ray's is not anywhere: every display set carries its own colours,
    // as many as 256 of them, so one of sixteen is built as the subtitles go
    // past and written over the top of this at the end. See
    // [`crate::vobsub::Ink`].
    // The one reporter, shared by the two passes that a transport stream is
    // written in, and throttled once for both of them: what a caller is shown
    // is a single bar over the whole job, so it has to be a single sequence.
    // See [`Pass`] and [`WRITING_SHARE`].
    let share = if wants_tables { WRITING_SHARE } else { 1.0 };
    let progress = progress.map(std::sync::Arc::new);
    let told = std::sync::Arc::new(std::sync::Mutex::new(crate::Told::new()));
    let say = |pass: Pass, base: f64, span: f64| {
        let (p, told) = (progress.clone(), told.clone());
        p.map(move |p| {
            Box::new(move |f: f64| {
                let done = base + f * span;
                // The throttle is on the job's figure, since that is what a
                // bar is drawn from; the pass's own is worked back out of it
                // so that both describe the same moment. See [`Pass`].
                told.lock().unwrap().at(
                    Some(&|d: f64| p(pass, d, ((d - base) / span).clamp(0.0, 1.0))),
                    done,
                );
            }) as Box<dyn Fn(f64) + Send + Sync>
        })
    };
    let writing_report = say(Pass::Writing, 0.0, share);
    let tables_report = say(Pass::Tables, share, 1.0 - share);

    let wanted = (!subpictures.is_empty() && converted.is_empty()) || !aside_streams.is_empty();
    let subpictures = wanted.then(|| {
        let from_disc = !subpictures.is_empty() && converted.is_empty();
        let palette = if from_disc {
            crate::dvd::subtitles_of(&src.path)
                .map(|s| s.palette)
                .unwrap_or_else(crate::vobsub::Palette::grey)
        } else {
            crate::vobsub::Palette::grey()
        };
        let mut side =
            crate::vobsub::Sidecar::new(src.video.width as u16, src.video.height as u16, palette);
        if from_disc {
            for s in &subpictures {
                side.declare(s.id as u8, s.language.clone());
            }
        }
        for (info, id) in &aside_streams {
            side.declare(*id as u8, info.language.clone());
        }
        Subpictures {
            side,
            ink: (!aside_streams.is_empty()).then(crate::vobsub::Ink::default),
            standing: Vec::new(),
            refused: 0,
        }
    });

    let mut writer = Writer {
        on_a_ts: src.on_a_ts,
        into_ts: octx.format().name().contains("mpegts"),
        octx,
        field_ticks: den,
        sub: grid.sub,
        our_tb: grid.time_base(),
        out_tb,
        // DTS trails PTS by the stream's reorder depth. Being generous costs
        // nothing: the muxer writes an edit list for the negative lead-in,
        // just as it would for any encoder's output.
        depth: opts
            .reorder_depth
            .unwrap_or(src.video.has_b_frames.max(0) as i64),
        pending: Default::default(),
        seen: Default::default(),
        last_dts: None,
        written: 0,
        skipped: 0,
        halves: 0,
        audio: audio_tracks,
        captions: caption_tracks,
        graphics: graphics_tracks,
        subpictures,
        converted,
        progress: writing_report,
        expected: plans
            .iter()
            .flat_map(|p| &p.segments)
            .map(|s| s.frames as i64)
            .sum(),
        shrink: shrink_for(src, opts),
    };

    let mut display_base: i64 = 0;
    let mut pictures: i64 = 0;
    // Where each kept range began in the output, which is what the tables
    // grafted on afterwards are placed against.
    let mut range_starts: Vec<f64> = Vec::with_capacity(plans.len());
    for (nth, plan) in plans.iter().enumerate() {
        // Every range after the first drops a decoder that is already reading
        // into a stream from somewhere else. A TrueHD frame is read against
        // the last major sync seen, so one joined anywhere else is read
        // against a header belonging to another part of the recording: the
        // restart header no longer matches, the matrix count runs past what
        // the format allows, and the frames come out as noise or are thrown
        // away. So the track waits for a sync at every seam, exactly as it
        // does when the file is opened. What that costs is up to a sync
        // interval of sound -- 16 access units, about 13 ms, on the discs
        // measured here.
        if nth > 0 {
            for track in writer.audio.iter_mut() {
                track.need_sync = track.joins_at_sync;
            }
        }
        // Anchor this range's audio to the output time its video starts at.
        // display_base counts fields, so two per frame.
        let target_start = display_base as f64 * grid.unit();
        range_starts.push(target_start);
        // How far each track actually laid down runs ahead of or behind where
        // this range's video starts. Zero for the first range, which is
        // positioned by the track's own start offset instead. Kept per track:
        // two tracks of the same recording drift by different amounts,
        // because their frames fall at different instants.
        let audio_ctx: Vec<AudioCtx> = writer
            .audio
            .iter()
            .enumerate()
            .map(|(track, t)| {
                let drift = t.end.map_or(0.0, |end| end - target_start);
                let window = (
                    (plan.t_in * t.info.sample_rate as f64).round() as i64,
                    (plan.t_out * t.info.sample_rate as f64).round() as i64,
                );
                if std::env::var("SMARTCUT_DEBUG").is_ok() {
                    eprintln!(
                        "  range t_in={:.4} target_start={:.4} track=0x{:04x} end={:?} \
                         drift={:+.4}",
                        plan.t_in,
                        target_start,
                        t.info.pid,
                        t.end.map(|v| (v * 1e4).round() / 1e4),
                        drift
                    );
                }
                AudioCtx {
                    track,
                    in_index: t.in_index,
                    in_tb: t.info.time_base,
                    offset: target_start - plan.t_in,
                    pick_from: plan.t_in + drift,
                    min_start: if t.end.is_none() {
                        plan.t_in
                    } else {
                        f64::NEG_INFINITY
                    },
                    window,
                    // The same answer the frames rewritten at a seam were
                    // shaped with, from the same function and the same range
                    // list: a track re-encoded whole and one spliced have to
                    // fade alike, or two tracks of one recording would.
                    fades: crate::audio::fades_for(
                        opts.audio_fade,
                        t.info.sample_rate,
                        nth,
                        plans.len(),
                        window,
                    ),
                    range_in: plan.t_in,
                    mode: t.mode,
                }
            })
            .collect();
        let caption_ctx: Vec<CaptionCtx> = writer
            .captions
            .iter()
            .enumerate()
            .map(|(track, t)| CaptionCtx {
                track,
                in_index: t.in_index,
                in_tb: t.in_tb,
                offset: target_start - plan.t_in,
                ttml: t.ttml,
                base: t.base,
                range: (plan.t_in, plan.t_out),
            })
            .collect();
        let graphics_ctx: Vec<GraphicsCtx> = writer
            .graphics
            .iter()
            .enumerate()
            .map(|(track, t)| GraphicsCtx {
                track,
                in_index: t.in_index,
                in_tb: t.in_tb,
                offset: target_start - plan.t_in,
            })
            .collect();
        // The same, for a DVD's subtitles: what was on screen when the range
        // opens is written again at its first frame. A unit that had already
        // taken itself down before the cut is not on screen and is not
        // written. See [`crate::vobsub`].
        if !src.subpictures.is_empty()
            && (writer.subpictures.is_some() || !writer.converted.is_empty())
        {
            for (id, shown, unit) in read_subpictures_before(src, plan.t_in)? {
                let over = crate::vobsub::stops_after(&unit)
                    .is_some_and(|after| shown + after <= plan.t_in);
                if over {
                    continue;
                }
                if let Some(subs) = writer.subpictures.as_mut() {
                    subs.take(id, target_start, &unit);
                }
                convert_subpicture(id, target_start, &unit, true, &mut writer)?;
            }
        }
        // What the recording had on screen at the instant this range opens
        // was put there by a display set the cut left behind, so it is put
        // up again here. See [`crate::pgs`].
        if !writer.graphics.is_empty() {
            read_graphics_before(src, plan.t_in, &mut writer)?;
            for track in 0..writer.graphics.len() {
                for held in writer.graphics[track].plane.replay(target_start) {
                    writer.push_graphics(track, &held, 0.0, true)?;
                }
            }
        }
        for (n, seg) in plan.segments.iter().enumerate() {
            let first_segment = n == 0;
            let ctx = SegmentCtx {
                display_base,
                grid,
                reframe: reframe.as_ref(),
                unframe: unframe.as_ref(),
                audio: &audio_ctx,
                captions: &caption_ctx,
                graphics: &graphics_ctx,
                offset: target_start - plan.t_in,
                first: first_segment,
                signalling: &signalling,
            };
            // Each segment reports the span it actually occupied, so the
            // next one starts exactly where it ended -- no reliance on the
            // planner's frame arithmetic, which cannot see either the
            // stream's frame phase or its pulldown.
            let span = match seg.kind {
                SegmentKind::Copy => copy_segment(src, seg, &ctx, &mut writer)?,
                SegmentKind::Reencode => reencode_segment(src, seg, &ctx, opts, &mut writer)?,
            };
            display_base += span.fields;
            pictures += span.pictures;
        }
        // And what it has on screen as the range ends is taken down, since
        // the display set that would have done it belongs to the material
        // after the cut. Left undone, a subtitle stands into the next range
        // -- or, at the end of the file, to the end of the file.
        if !writer.graphics.is_empty() {
            let ends_at = display_base as f64 * grid.unit();
            for track in 0..writer.graphics.len() {
                for held in writer.graphics[track].plane.clear(ends_at) {
                    writer.push_graphics(track, &held, 0.0, true)?;
                }
            }
        }
        // And a DVD's subtitles are taken down at the same instant, by a
        // unit that says stop and nothing else.
        let ends_at = display_base as f64 * grid.unit();
        if let Some(subs) = writer.subpictures.as_mut() {
            subs.end_range(ends_at);
        }
        for which in 0..writer.converted.len() {
            let c = &mut writer.converted[which];
            let (track, screen) = (c.track, c.screen);
            let sets = c.composer.take_down(screen, ends_at);
            for held in &sets {
                writer.push_graphics(track, held, 0.0, true)?;
            }
        }
    }
    // Flush whatever each audio encoder still holds before closing the file.
    for track in 0..writer.audio.len() {
        let mut tail = Vec::new();
        if let Some(re) = writer.audio[track].reencoder.as_mut() {
            re.finish(&mut tail)?;
        }
        for (p, pts) in tail {
            writer.push_audio_encoded(track, p, pts)?;
        }
    }
    writer.flush()?;
    writer.octx.write_trailer()?;
    if let Some(shrink) = &writer.shrink {
        shrink.say();
    }

    if writer.written + writer.skipped != pictures {
        bail!(
            "segments reported {pictures} pictures, wrote {}",
            writer.written
        );
    }
    // A track the output declares and holds not one frame of. Everything
    // else that can lose a track ends in an error the caller can read -- a
    // codec the container has no box for stops the header, an encoder that
    // will not open stops the setup -- and this was the one way of losing
    // one that said nothing: the file plays, the map lists the sound, and
    // there is no sound. It is worth the whole cut, because a cut with a
    // silent track in it is a cut that has to be made again anyway.
    for t in &writer.audio {
        if t.written > 0 {
            continue;
        }
        let name = crate::track_name(writer.on_a_ts, t.info.pid, t.info.stream_index);
        let why = if t.no_length > 0 {
            format!(
                ": {} frame(s) of it carry no length of their own, and the spacing of this \
                 track's timestamps could not be measured either",
                t.no_length
            )
        } else {
            ", and the kept ranges hold none of it".to_string()
        };
        bail!(
            "the sound on {name} is declared in the cut and not one frame of it was written{why}. \
             Leaving the track out of the cut is how to write the rest of it"
        );
    }
    if writer.skipped > 0 {
        // Two things hand pictures over in an order the output timeline cannot
        // take, and which one it was is known here: a recording read as
        // several stretches joined says so, and nothing else does.
        eprintln!(
            "note: {} picture(s) could not be placed on the output timeline -- {} -- and were \
             left out of the {} written.",
            writer.skipped,
            if src.joins.is_empty() {
                "a damaged recording hands them over out of any order a decoder could restore"
            } else {
                "where a recorder stopped and started, the stretch on one side of the join \
                 presents a little past the place the disc's index gives the next, and a \
                 picture landing behind one already written has nowhere to go"
            },
            writer.written,
        );
    }

    if std::env::var("SMARTCUT_DEBUG").is_ok() {
        for t in &writer.graphics {
            eprintln!(
                "  graphics on {}: {} packet(s) carried, {} written to open and close the \
                 kept ranges",
                crate::track_name(writer.on_a_ts, t.pid, t.in_index),
                t.written,
                t.mended,
            );
        }
    }

    // What a damaged recording -- or a join -- cost the sound, said once per
    // track rather than once per frame.
    for t in &writer.audio {
        if t.dropped == 0 {
            continue;
        }
        let (why, cost) = if src.joins.is_empty() {
            (
                "which is what damage does to a recording's timestamps",
                "The sound there was lost with the packets that carried it.",
            )
        } else {
            (
                "which is what a join does, where the sound of one stretch runs past the \
                 place the disc's index gives the next",
                "Those instants are covered by the stretch in front of them; what was left \
                 out is the second account of them.",
            )
        };
        eprintln!(
            "note: {} frame(s) of the sound on {} do not follow the frame before them -- {why} \
             -- and were left out. {cost}",
            t.dropped,
            crate::track_name(writer.on_a_ts, t.info.pid, t.info.stream_index),
        );
    }

    // And what a recording whose packets say nothing about their own length
    // cost, where the spacing of the timestamps did not answer for all of
    // them either. A track that lost every frame this way never reaches
    // here; it is the whole cut, above.
    for t in &writer.audio {
        if t.no_length == 0 {
            continue;
        }
        eprintln!(
            "note: {} frame(s) of the sound on {} carry no length and none could be worked out \
             for them, and were left out. The output lays its sound end to end, so a frame of \
             no length would be written where the next one belongs.",
            t.no_length,
            crate::track_name(writer.on_a_ts, t.info.pid, t.info.stream_index),
        );
    }

    // What the conversion came to, where one was asked for. Where it went is
    // said by the note above for a `.sup` and by the stream itself for a cut.
    for c in &writer.converted {
        let into = if writer.graphics[c.track].aside.is_some() {
            "written beside the cut"
        } else {
            "written into the cut"
        };
        eprintln!(
            "note: {} subtitle(s) the disc draws were {into} as the kind a transport \
             stream carries, on {}. The pixels and the colours are the disc's; what \
             changed is how they are spelled.",
            c.shown,
            crate::track_name(true, pids.graphics(c.track.saturating_sub(graphics.len()), c.id), 0),
        );
    }

    // The display sets that travelled beside the cut rather than into it,
    // written now that the cut is closed. Said out loud, as the pair below
    // is: a file nobody asked for by name is a file worth naming.
    for track in &writer.graphics {
        let Some(Aside::Sup(sup)) = &track.aside else {
            continue;
        };
        // A converted stream has no input stream to be named by -- it is
        // written, not carried -- so it is named by the number the disc knew
        // its subtitles by, which is where it would have gone.
        let name = crate::track_name(
            writer.on_a_ts || track.in_index == usize::MAX,
            track.pid,
            track.in_index,
        );
        if sup.is_empty() {
            eprintln!(
                "note: the kept ranges hold none of the subtitles on {name}. Nothing was \
                 written beside the cut for it."
            );
            continue;
        }
        match sup.write(output) {
            Ok(at) => eprintln!(
                "note: the subtitles on {name} are beside the cut, not in it, as asked. \
                 {at} carries {} segment(s), on the cut's own clock.",
                sup.count(),
            ),
            // A cut that came out right is not worth failing over the files
            // beside it.
            Err(e) => eprintln!("note: the subtitles could not be written beside {output}: {e}"),
        }
    }

    // The subtitles that travelled beside the cut, written now that it is
    // known how long it turned out to be. Said out loud: two files nobody
    // asked for by name are two files worth naming.
    if let Some(mut subs) = writer.subpictures.take() {
        // The palette invented on the way, where one was: it is not finished
        // until the last subtitle has gone past. See [`crate::vobsub::Ink`].
        let drawn_back = subs.ink.is_some();
        if let Some(ink) = subs.ink.as_ref() {
            subs.side.recolour(ink.palette());
        }
        if subs.refused > 0 {
            eprintln!(
                "note: {} subtitle(s) were too big to be written as the kind a DVD draws -- \
                 a unit states its own length in two bytes -- and are not in the pair \
                 beside the cut.",
                subs.refused,
            );
        }
        if subs.side.is_empty() {
            eprintln!(
                "note: this recording declares subtitles the disc draws, and the kept \
                 ranges hold none of them. Nothing was written beside the cut."
            );
        } else {
            match subs.side.write(output) {
                Ok((idx, sub)) => {
                    let counts: Vec<String> = subs
                        .side
                        .counts()
                        .iter()
                        .map(|(id, n)| format!("{n} on 0x{id:02x}"))
                        .collect();
                    let why = if drawn_back {
                        "the subtitles are beside the cut rather than in it, as asked -- read \
                         back out of the display sets the disc draws them with and written as \
                         the kind a DVD draws, palette and all"
                    } else {
                        "the subtitles are beside the cut, not in it -- a transport stream has \
                         no place for the kind a DVD draws"
                    };
                    eprintln!(
                        "note: {why}. {idx} and {sub} carry {}. A player opening the cut \
                         finds them by name.",
                        counts.join(", "),
                    );
                }
                // A cut that came out right is not worth failing over the
                // files beside it.
                Err(e) => {
                    eprintln!("note: the subtitles could not be written beside {output}: {e}")
                }
            }
        }
    }

    // Close the output before the tables go in. `write_trailer` flushes the
    // muxer, but the file handle is the output context's and only dropping it
    // gives it up -- and the graft finishes by renaming its rewritten copy
    // over this file, which Windows refuses to do while the file is open.
    // The writing pass's own reporter goes with it; what is left to report is
    // the tables, which have their own.
    drop(writer.progress.take());
    drop(writer);

    // A recording that never was a broadcast has no account of itself to put
    // back -- but it can still have been written with a track the muxer
    // cannot name. Asked for a plain `.ts` libavformat declares Blu-ray LPCM
    // as private data of no stated kind, and everything reads that back as
    // `bin_data`: a cut of a DVD title arrived with its sound carried,
    // declared, and silent. A disc's subtitles come off the same edge of the
    // same problem. (Asked for a `.m2ts` the same muxer gets both right,
    // which is why only this shape needs the visit.) So the map is rebuilt
    // from the one the muxer itself wrote, with those corrections and
    // nothing else added -- no selection table, no service, no event: there
    // was never a broadcast here to describe.
    let unnamed = to_ts
        && tables.is_none()
        && !writing_m2ts(output)
        && (setups
            .iter()
            .any(|s| s.recoded && s.target == ff::codec::Id::PCM_BLURAY)
            || !graphics.is_empty()
            || !converted_streams.is_empty());
    let own_map = unnamed
        .then(|| {
            let at = crate::input::Input::plain(output);
            crate::si::read_service(&at, pids.video(video_pid) as u16, &[])
                .map_err(|e| eprintln!("note: {output} cannot be read back to name its sound: {e}"))
                .ok()
        })
        .flatten();
    // The file is complete and correct as a file; what it does not yet have
    // is the broadcast's own account of itself. See [`crate::si`].
    if let Some(service) = tables.as_ref().or(own_map.as_ref()) {
        match graft_tables(
            src,
            service,
            &setups,
            &captions,
            &graphics,
            &converted_streams,
            // Only where the broadcast's own tables are going in: the map
            // that names the carousel is written by the same pass that
            // carries it, and a file with the packets and no entry for them
            // is a file nothing can find them in.
            if tables.is_some() { data.as_slice() } else { &[] },
            video_pid,
            &pids,
            &range_starts,
            plans,
            output,
            tables_report.as_ref().map(|f| &**f as &(dyn Fn(f64) + Sync)),
            // The recording's own tables where it had some; where it had
            // none, only the map is being corrected.
            if tables.is_some() {
                want
            } else {
                crate::si::Tables::Muxer
            },
        ) {
            Ok(stats) if std::env::var("SMARTCUT_DEBUG").is_ok() => {
                eprintln!(
                    "  tables: {} list, {} map, {} service, {} event, {} clock, \
                     {} selection; {} clock references given a PID of their own; \
                     {} data broadcast packets carried",
                    stats.pat,
                    stats.pmt,
                    stats.sdt,
                    stats.eit,
                    stats.tot,
                    stats.sit,
                    stats.pcr,
                    stats.data
                );
            }
            Ok(_) => {}
            // A cut that came out right is not worth failing over a table.
            // Say what was lost and leave the file alone.
            Err(e) => eprintln!(
                "note: the cut is written, but the broadcast's own tables could not be put \
                 back: {e}"
            ),
        }
    }

    // Whichever pass was the last one, ending on the end of it. A cut whose
    // tables were not going back in never left the first.
    if let Some(report) = &progress {
        report(
            if share < 1.0 { Pass::Tables } else { Pass::Writing },
            1.0,
            1.0,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two streams of a recording that has no PIDs of its own -- a Matroska
    /// file, where libavformat leaves every stream's id at nought -- have to
    /// come out on two PIDs all the same. Asked by the number they arrived
    /// on, as this was, both were given the pictures' 0x1011 and the muxer
    /// refused the pair with `Invalid argument`.
    #[test]
    fn a_disc_numbers_its_streams_by_position() {
        let disc = Pids::bluray();
        assert_eq!(disc.video(0), 0x1011);
        assert_eq!((disc.audio(0, 0), disc.audio(1, 0)), (0x1100, 0x1101));
        assert_eq!(disc.caption(0, 0), 0x1110);
        // The subtitles converted from a DVD's are numbered after the ones
        // the recording carried, in the same run.
        assert_eq!((disc.graphics(0, 0), disc.graphics(1, 0)), (0x1200, 0x1201));
        // And a `.ts` keeps whatever the recording had, which for that same
        // Matroska file is nothing at all: the muxer numbers them itself.
        let kept = Pids::kept();
        assert_eq!(kept.video(0x100), 0x100);
        assert_eq!(kept.audio(1, 0x101), 0x101);
        assert_eq!(kept.graphics(0, 0), 0);
    }

    /// A grid from the two rates and whether the walk called the recording
    /// variable, without a whole `Source` to hang them off.
    fn grid(frame_rate: f64, base_rate: f64, variable: bool) -> Grid {
        if !variable {
            let (num, den) = frame_rate_parts(frame_rate);
            return Grid { num, den, sub: 1 };
        }
        let usable = base_rate > 0.0 && base_rate <= frame_rate * 1.01;
        let (num, den) = frame_rate_parts(if usable { base_rate } else { frame_rate });
        Grid { num, den, sub: FINE }
    }

    /// A constant recording is counted in whole fields of its own rate, and
    /// the output it has always had comes out unchanged.
    #[test]
    fn a_constant_recording_is_counted_in_fields() {
        let g = grid(30000.0 / 1001.0, 30000.0 / 1001.0, false);
        assert_eq!(g.sub, 1);
        assert_eq!(g.timescale(), 60000);
        assert!((g.unit() - 1001.0 / 60000.0).abs() < 1e-12);
    }

    /// A recording whose pictures average out faster than it declares is
    /// counted on the rate it declares, finely enough for the fast ones.
    #[test]
    fn a_recording_faster_than_it_declares_takes_the_declared_rate() {
        let g = grid(24.926, 24000.0 / 1001.0, true);
        assert_eq!(g.sub, FINE);
        assert_eq!(g.timescale(), 2 * 24000 * FINE);
        // Two pictures a sixtieth of a second apart have to land on places
        // of their own, which is what a field of 23.976 could not give them.
        assert!(1.0 / 60.0 / g.unit() > 2.0);
    }

    /// **An interlaced recording declares the rate of its fields**, which is
    /// twice its pictures'. Taken for a frame rate it would make every
    /// picture half as long as it is, so it is not taken: the average
    /// stands, and only the division gets finer.
    #[test]
    fn a_field_rate_is_never_mistaken_for_a_frame_rate() {
        let avg = 30000.0 / 1001.0;
        let g = grid(avg, 60000.0 / 1001.0, true);
        let plain = grid(avg, avg, false);
        assert!(
            (g.unit() * FINE as f64 - plain.unit()).abs() < 1e-9,
            "a field of the fine grid is a field of the plain one"
        );
    }

    /// And a container that would not say leaves the average standing too.
    #[test]
    fn no_declared_rate_leaves_the_average() {
        let avg = 30000.0 / 1001.0;
        let g = grid(avg, 0.0, true);
        assert_eq!(g.timescale(), 2 * 30000 * FINE);
    }

    /// What the output settings screen offers, which is what it asks about.
    /// A zero is its `入力と同じ`.
    fn offered() -> SoundChoices {
        SoundChoices {
            codecs: vec![
                AudioCodec::Source,
                AudioCodec::Aac,
                AudioCodec::Ac3,
                AudioCodec::Dts,
                AudioCodec::Lpcm,
            ],
            channels: vec![0, 1, 2, 6],
            sample_rates: vec![0, 96_000, 48_000, 44_100, 32_000],
            bits: vec![0, 16, 24],
            bit_rates: vec![384_000, 512_000, 768_000, 1_536_000],
        }
    }

    /// A 5.1 broadcast, going where the container argument says.
    fn surround(to_ts: bool) -> SoundAsIs {
        SoundAsIs {
            codec: "aac".into(),
            channels: 6,
            sample_rate: 48_000,
            bits: 16,
            to_ts,
        }
    }

    #[test]
    fn offers_only_the_rates_the_codec_is_written_at() {
        let opts = CutOptions {
            audio_codec: AudioCodec::Lpcm,
            ..Default::default()
        };
        // Blu-ray LPCM -- the only linear PCM a transport stream can declare
        // -- has 48, 96 and 192 kHz and nothing between.
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert_eq!(can.sample_rates, vec![0, 96_000, 48_000]);
        // Into an MP4 the same samples go in as plain big-endian PCM, which
        // lists no rates at all and takes whatever it is handed.
        let can = writable_sound(&[surround(false)], &opts, &offered());
        assert_eq!(can.sample_rates, vec![0, 96_000, 48_000, 44_100, 32_000]);
        // The window asks about every rate on its list, the ceiling that
        // decides which of them a given recording may be offered being its
        // own arithmetic and not this. So which codecs have 96 kHz is a
        // question that reaches here: AAC does; AC-3 has 32, 44.1 and 48 and
        // nothing above.
        let opts = CutOptions {
            audio_codec: AudioCodec::Aac,
            ..Default::default()
        };
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert!(can.sample_rates.contains(&96_000));
        let opts = CutOptions {
            audio_codec: AudioCodec::Ac3,
            ..Default::default()
        };
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert_eq!(can.sample_rates, vec![0, 48_000, 44_100, 32_000]);
    }

    #[test]
    fn offers_only_the_rungs_above_the_codecs_floor() {
        let opts = CutOptions {
            audio_codec: AudioCodec::Dts,
            ..Default::default()
        };
        // A DTS frame carries a fixed number of samples and has to be long
        // enough to describe every channel in it, so 5.1 at 48 kHz has a
        // floor between 640 and 768 kbit/s.
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert_eq!(can.bit_rates, vec![768_000, 1_536_000]);
        // The floor comes down with the rate, since the same frame then
        // covers more of a second.
        let opts = CutOptions {
            audio_sample_rate: Some(32_000),
            ..opts
        };
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert_eq!(can.bit_rates, vec![512_000, 768_000, 1_536_000]);
        // And AAC has no floor at all.
        let opts = CutOptions {
            audio_codec: AudioCodec::Aac,
            ..Default::default()
        };
        let can = writable_sound(&[surround(true)], &opts, &offered());
        assert_eq!(can.bit_rates, offered().bit_rates);
    }

    #[test]
    fn withholds_a_codec_the_channels_cannot_be_written_in() {
        // DTS is written mono, stereo, quad, 5.0 or 5.1 and in no other
        // count, so a three channel recording carried through as it is has
        // nowhere to put its middle channel.
        let three = SoundAsIs {
            channels: 3,
            ..surround(true)
        };
        let can = writable_sound(&[three], &CutOptions::default(), &offered());
        assert!(!can.codecs.contains(&AudioCodec::Dts));
        assert!(can.codecs.contains(&AudioCodec::Ac3));
        // Folded to stereo on the way it is a count DTS does have, so the
        // codec is on offer again the moment the channels are chosen.
        let three = SoundAsIs {
            channels: 3,
            ..surround(true)
        };
        let opts = CutOptions {
            audio_channels: Some(2),
            ..Default::default()
        };
        let can = writable_sound(&[three], &opts, &offered());
        assert!(can.codecs.contains(&AudioCodec::Dts));
    }

    #[test]
    fn answers_for_every_track_that_will_be_written() {
        // One recording DTS can be written from and one it cannot: the
        // second is enough to take the codec off the list, because the cut
        // writes both.
        let tracks = [
            surround(true),
            SoundAsIs {
                channels: 3,
                ..surround(true)
            },
        ];
        let can = writable_sound(&tracks, &CutOptions::default(), &offered());
        assert!(!can.codecs.contains(&AudioCodec::Dts));
    }

    #[test]
    fn judges_the_lists_against_answers_that_could_themselves_be_written() {
        // A bitrate under the floor -- out of a project written before there
        // was a floor to know of. Judged against it, every rate and every
        // channel count would come back refused and the screen would have
        // nothing on it to choose.
        let opts = CutOptions {
            audio_codec: AudioCodec::Dts,
            audio_bit_rate: Some(384_000),
            ..Default::default()
        };
        let can = writable_sound(&[surround(true)], &opts, &offered());
        // 96 kHz is not among them: DTS is written at 48 and no higher.
        assert_eq!(can.sample_rates, vec![0, 48_000, 44_100, 32_000]);
        assert_eq!(can.channels, vec![0, 1, 2, 6]);
        // And the rung itself is not on offer, which is what puts the
        // setting back inside the list.
        assert_eq!(can.bit_rates, vec![768_000, 1_536_000]);
    }

    #[test]
    fn says_nothing_about_a_track_it_cannot_name() {
        // A recording read by a version that did not send the codec down.
        // Nothing here can say what it could be written as, and a list
        // greyed out on a guess is worse than one that was not.
        let unnamed = SoundAsIs {
            codec: String::new(),
            ..surround(true)
        };
        let can = writable_sound(&[unnamed], &CutOptions::default(), &offered());
        assert_eq!(can.codecs.len(), offered().codecs.len());
        assert_eq!(can.bit_rates, offered().bit_rates);
    }

    #[test]
    fn a_file_carries_the_broadcast_and_a_clip_is_a_partial_stream() {
        use crate::si::Tables;
        // What a player opens is a `.ts`, and what it reads there is the
        // broadcast's own EIT, SDT and TOT.
        assert_eq!(tables_for("cut.ts", None), Tables::Broadcast);
        assert_eq!(tables_for("/tmp/CUT.TS", None), Tables::Broadcast);
        // A Blu-ray clip is a partial transport stream because that is what
        // the format is -- on a disc under `--bdav` or standing on its own.
        assert_eq!(tables_for("BDAV/STREAM/00001.m2ts", None), Tables::Partial);
        assert_eq!(tables_for("cut.M2TS", None), Tables::Partial);
        // Not a transport stream at all: the answer is never looked at, and
        // the one given here is the file-shaped one.
        assert_eq!(tables_for("cut.mp4", None), Tables::Broadcast);
    }

    #[test]
    fn asking_for_a_shape_overrides_the_name() {
        use crate::si::Tables;
        for name in ["cut.ts", "cut.m2ts"] {
            for want in [Tables::Muxer, Tables::Broadcast, Tables::Partial] {
                assert_eq!(tables_for(name, Some(want)), want);
            }
        }
    }

    #[test]
    fn only_a_plain_transport_stream_holds_a_carousel() {
        use crate::si::Tables;
        // The shape it is written in: a `.ts`, keeping the broadcast's own
        // tables, which is what a `.ts` does unasked.
        assert!(can_carry_data_broadcast("cut.ts", None));
        assert!(can_carry_data_broadcast("CUT.TS", Some(Tables::Partial)));
        // A disc's stream, whether it is being written onto a disc or
        // standing on its own. A clip index has nowhere to name one and a
        // player has nowhere to show it, so a run that writes one says so
        // rather than promising a carousel it then leaves out.
        assert!(!can_carry_data_broadcast("BDAV/STREAM/00001.m2ts", None));
        assert!(!can_carry_data_broadcast("cut.M2TS", Some(Tables::Broadcast)));
        // Nothing else has a PID to put one on.
        assert!(!can_carry_data_broadcast("cut.mp4", None));
        // And nothing carries it where the pass that writes it does not run.
        assert!(!can_carry_data_broadcast("cut.ts", Some(Tables::Muxer)));
    }
}
