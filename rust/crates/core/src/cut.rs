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
    annexb_to_length, is_annexb, parameter_sets, prepend_parameter_sets, NalFraming,
};
use crate::{RangePlan, Segment, SegmentKind, Source};

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

#[derive(Debug, Clone, Default)]
pub struct CutOptions {
    /// Reorder depth used when deriving DTS from decode order.
    /// `None` takes the source stream's own depth.
    pub reorder_depth: Option<i64>,
    /// Bits per second for re-encoded pictures. `None` derives it from the
    /// source, with a little headroom so the splice does not visibly soften.
    pub bit_rate: Option<usize>,
    pub audio_mode: AudioMode,
    /// Bits per second for re-encoded audio. `None` follows the source.
    pub audio_bit_rate: Option<usize>,
    /// Channels the audio is written with. `None` follows the source.
    ///
    /// A count that is not the source's is a downmix -- 5.1 into stereo, for
    /// the recording whose surround track is a nuisance everywhere it is
    /// played back. Nothing about it is a splice: a stereo frame cannot sit
    /// among the recording's 5.1 ones, so asking for one asks for the whole
    /// track, and [`AudioMode::Reencode`] is what actually runs.
    pub audio_channels: Option<u16>,
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
    /// Which account of itself the output carries.
    ///
    /// Defaults to a partial transport stream, which is what a cut of a
    /// broadcast is: [`crate::si::Tables::Partial`].
    ///
    /// The muxer writes a description of the streams and stops there. What
    /// the other two settings restore is everything else a broadcast says
    /// about itself -- the service and its name, the programme, the times,
    /// and the descriptors that say what each stream is -- either as the one
    /// table a recording is written down in or in the shape the broadcast
    /// sent them. See [`crate::si::Tables`]. Only means anything writing a
    /// transport stream.
    pub tables: crate::si::Tables,
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
}

/// What a segment contributed, so the next one can be placed after it.
///
/// Spans are measured from the pictures that were actually emitted rather
/// than from the planner's arithmetic. The planner works on an idealised
/// grid of whole frames at multiples of the frame duration; real streams put
/// their pictures at an arbitrary phase, and pulldown material shows some of
/// them for three fields. Anchoring each segment on its own first picture
/// keeps the joins exact under both.
#[derive(Debug, Default, Clone, Copy)]
struct Span {
    /// Fields this segment occupies on the output timeline.
    fields: i64,
    /// Pictures emitted.
    pictures: i64,
}

/// Where a segment sits in the output and how its packets must be shaped.
struct SegmentCtx<'a> {
    /// First display index this segment contributes.
    display_base: i64,
    reframe: Option<&'a Reframe>,
    audio: &'a [AudioCtx],
    captions: &'a [CaptionCtx],
    /// Whether this is the opening segment of its keep-range, which is where
    /// the range's audio boundary decision is made.
    first: bool,
}

/// Everything the writer needs that is shared across segments.
struct Writer {
    octx: ff::format::context::Output,
    /// Ticks per *field*. The output timeline is measured in fields, not
    /// frames, because 2:3 pulldown shows some pictures for three fields and
    /// others for two -- a frame grid cannot express that, a field grid can,
    /// and for constant-rate material every picture is simply two fields.
    field_ticks: i64,
    our_tb: ff::Rational,
    out_tb: ff::Rational,
    depth: i64,
    /// Pictures held back so DTS can be derived from display order.
    pending: std::collections::VecDeque<Emitted>,
    /// Display positions seen so far, smallest first.
    seen: std::collections::BinaryHeap<std::cmp::Reverse<i64>>,
    written: i64,
    /// The output's sound tracks, in the order they were added. A broadcast
    /// in two languages has two, and each is cut on its own -- one track's
    /// boundary frame is no business of another's.
    audio: Vec<AudioTrack>,
    /// The output's caption tracks, likewise.
    captions: Vec<CaptionTrack>,
    /// Called as pictures land, with a 0..1 fraction. A long cut is mostly
    /// I/O, so the caller needs something to show.
    progress: Option<Box<dyn Fn(f64) + Send + Sync>>,
    expected: i64,
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
}

/// One caption track being written.
struct CaptionTrack {
    out_index: usize,
    out_tb: f64,
    in_index: usize,
    in_tb: f64,
    written: i64,
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
        while self.pending.len() > self.depth.max(0) as usize {
            self.emit_one()?;
        }
        Ok(())
    }

    fn emit_one(&mut self) -> Result<()> {
        let Some(mut e) = self.pending.pop_front() else { return Ok(()) };
        let Some(std::cmp::Reverse(next_in_display)) = self.seen.pop() else { return Ok(()) };
        // Three fields of lead-in per level of reordering: enough headroom
        // for the longest picture a pulldown stream can contain.
        let dts = next_in_display - self.depth * 3;
        e.packet.set_stream(0);
        e.packet.set_pts(Some(e.display * self.field_ticks));
        e.packet.set_dts(Some(dts * self.field_ticks));
        e.packet.set_duration(e.fields * self.field_ticks);
        e.packet.set_position(-1);
        e.packet.rescale_ts(self.our_tb, self.out_tb);
        e.packet.write_interleaved(&mut self.octx)?;
        self.written += 1;
        if let Some(report) = &self.progress {
            if self.expected > 0 && self.written % 16 == 0 {
                report((self.written as f64 / self.expected as f64).min(1.0));
            }
        }
        Ok(())
    }

    fn push_audio_encoded(&mut self, track: usize, mut packet: ff::Packet, pts: i64) -> Result<()> {
        let Some(t) = self.audio.get(track) else { return Ok(()) };
        let (index, tb, rate) = (t.out_index, t.out_tb, t.info.sample_rate);
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
        let Some(t) = self.audio.get(track) else { return Ok(()) };
        let (index, tb) = (t.out_index, t.out_tb);
        if out_dur <= 0.0 {
            return Ok(());
        }
        packet.set_stream(index);
        let pts = (out_start.max(0.0) / tb).round() as i64;
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_duration((out_dur / tb).round() as i64);
        packet.set_position(-1);
        packet.write_interleaved(&mut self.octx)?;
        let t = &mut self.audio[track];
        t.written += 1;
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
        let Some(t) = self.captions.get(track) else { return Ok(()) };
        let (index, tb) = (t.out_index, t.out_tb);
        packet.set_stream(index);
        let pts = (at.max(0.0) / tb).round() as i64;
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_duration(0);
        packet.set_position(-1);
        packet.write_interleaved(&mut self.octx)?;
        self.captions[track].written += 1;
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
    let Some(pts) = packet.pts() else { return Ok(false) };
    let t = pts as f64 * audio.in_tb - src.start_time;
    let dur = packet.duration() as f64 * audio.in_tb;
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
            let AudioTrack { info, reencoder, .. } = &mut writer.audio[audio.track];
            if let Some(re) = reencoder.as_mut() {
                re.take(&packet, info, src.start_time, audio.window)?;
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
    let track = &writer.audio[audio.track];
    let patch = track.patches.get(&pts).filter(|p| p.after.is_none() || p.after == track.prev);
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
    packet: ff::Packet,
    writer: &mut Writer,
) -> Result<bool> {
    let Some(pts) = packet.pts() else { return Ok(false) };
    let t = pts as f64 * caption.in_tb - src.start_time;
    if t >= seg.end {
        return Ok(true);
    }
    if t >= seg.start {
        writer.push_caption(caption.track, packet, t + caption.offset)?;
    }
    Ok(false)
}

fn open_input(path: &str) -> Result<(ff::format::context::Input, usize)> {
    let ictx = ff::format::input(&path)?;
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
    crate::init()?;
    let mut ictx = ff::format::input(&cut)?;
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
    let out_tb = octx.stream(0).ok_or_else(|| anyhow!("no output stream"))?.time_base();

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

fn writing_ts(path: &str) -> bool {
    matches!(
        std::path::Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ts" | "m2ts" | "mts" | "m2t")
    )
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
        let first_pid = if (PID_MIN..=PID_MAX).contains(&video_pid) { video_pid } else { 0 };
        let (mut pmt_pid, service_id) = if (*ic).nb_programs > 0 {
            let p = *(*ic).programs;
            ((*p).pmt_pid, (*p).id)
        } else {
            (0, 0)
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
            service_id: if (1..=0xFFFF).contains(&service_id) { service_id } else { 0 },
        })
    }
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

/// Take the segment's packets straight from the source, untouched.
fn copy_segment(
    src: &Source,
    seg: &Segment,
    ctx: &SegmentCtx,
    writer: &mut Writer,
) -> Result<Span> {
    let (display_base, reframe, first_segment) = (ctx.display_base, ctx.reframe, ctx.first);
    let (mut ictx, ist_index) = open_input(&src.path)?;
    let in_tb = f64::from(ictx.stream(ist_index).unwrap().time_base());
    let fd = src.video.frame_duration();
    let field = fd / 2.0;
    seek_to(&mut ictx, src, seg.start)?;
    // Anchored on the first picture actually emitted, not on the planner's
    // idealised time for it.
    let mut anchor: Option<f64> = None;
    let mut span = Span::default();

    let mut started = false;
    let mut overshot = false;
    let mut video_done = false;
    // One flag per stream being gathered. The read stops when every one of
    // them has run past this segment, not when the first does: the sound of
    // a bilingual recording arrives on two PIDs that are interleaved but not
    // in step, and stopping on either would truncate the other.
    let mut audio_done = vec![false; ctx.audio.len()];
    let mut caption_done = vec![false; ctx.captions.len()];

    for (stream, packet) in ictx.packets() {
        let index = stream.index();
        if index != ist_index {
            if let Some(k) = ctx.audio.iter().position(|a| a.in_index == index) {
                if !audio_done[k] {
                    audio_done[k] =
                        take_audio(&ctx.audio[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.captions.iter().position(|c| c.in_index == index) {
                if !caption_done[k] {
                    caption_done[k] = take_caption(&ctx.captions[k], src, seg, packet, writer)?;
                }
            }
            if video_done && audio_done.iter().all(|&d| d) && caption_done.iter().all(|&d| d) {
                break;
            }
            continue;
        }
        if video_done {
            if audio_done.iter().all(|&d| d) && caption_done.iter().all(|&d| d) {
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
        let Some(pts) = packet.pts() else { continue };
        let t = pts as f64 * in_tb - src.start_time;

        if !started {
            // Roll forward to the entry point.
            if !(packet.is_key() && (t - seg.start).abs() < fd / 2.0) {
                // A keyframe beyond the target means the seek overshot and
                // the entry point is already behind us.
                if packet.is_key() && t > seg.start + fd {
                    overshot = true;
                    break;
                }
                continue;
            }
            started = true;
        }
        // The copy ends when the terminating access point turns up. Its own
        // leading pictures are decoded after it, so stopping here leaves them
        // out -- which is exactly the display range the planner asked for.
        if let Some(until) = seg.copy_until {
            if (packet.is_key() && (t - until).abs() < fd / 2.0) || t > until + fd {
                video_done = true;
                if audio_done.iter().all(|&d| d) && caption_done.iter().all(|&d| d) {
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
        let display = display_base + ((t - a) / field).round() as i64;
        let fields = packet
            .data()
            .map(|d| crate::bitstream::display_fields(d, &src.video.codec))
            .unwrap_or(2);
        span.fields = span.fields.max(display - display_base + fields);
        span.pictures += 1;
        let packet = match reframe {
            Some(r) if packet.is_key() => {
                let data = packet.data().unwrap_or(&[]);
                let mut out =
                    ff::Packet::copy(&prepend_parameter_sets(data, &r.sets, r.nal_length));
                out.set_flags(ff::packet::Flags::KEY);
                out
            }
            _ => packet,
        };
        writer.push(Emitted { packet, display, fields })?;
    }
    if !started {
        if overshot {
            bail!(
                "seek overshot the entry point at {:.3}s (margin {:.1}s was not enough)",
                seg.start,
                src.seek_margin
            );
        }
        bail!("could not find the entry point at {:.3}s", seg.start);
    }
    Ok(span)
}

/// Build an encoder whose output splices onto the copied pictures.
fn open_encoder(
    src: &Source,
    params: &ff::codec::Parameters,
    opts: &CutOptions,
) -> Result<ff::encoder::video::Encoder> {
    let id = params.id();
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no encoder for {id:?}"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;

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
        (*e).color_trc = (*p).color_trc;
        (*e).colorspace = (*p).color_space;
        (*e).color_range = (*p).color_range;
        (*e).profile = (*p).profile;
        (*e).level = (*p).level;
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
    }
    enc.set_bit_rate(opts.bit_rate.unwrap_or_else(|| default_bit_rate(src)));

    let mut eopts = ff::Dictionary::new();
    // mpeg2video and mpeg4 refuse a closed GOP while scene-change detection
    // is live; a partial GOP is short enough that losing it costs nothing.
    if matches!(v.codec.as_str(), "mpeg2video" | "mpeg4") {
        eopts.set("sc_threshold", "1000000000");
    }
    enc.open_as_with(codec, eopts).map_err(|e| anyhow!("cannot open encoder: {e}"))
}

/// Recover the exact rational frame rate, so 29.97 comes back as 30000/1001
/// rather than something that rounds.
fn frame_rate_parts(fps: f64) -> (i64, i64) {
    let r = ff::Rational::from(fps).reduce();
    (r.numerator().max(1) as i64, r.denominator().max(1) as i64)
}

fn default_bit_rate(src: &Source) -> usize {
    let v = &src.video;
    let px = v.width as f64 * v.height as f64 * v.frame_rate.max(1.0);
    (px * 0.08) as usize
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
        writer.push(Emitted { packet, display, fields })?;
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
    let (mut ictx, ist_index) = open_input(&src.path)?;
    let stream = ictx.stream(ist_index).unwrap();
    let in_tb = f64::from(stream.time_base());
    let params = stream.parameters();
    let fd = src.video.frame_duration();
    let field = fd / 2.0;
    // A range bound often lands exactly on a picture's timestamp. Compare
    // with a hair of slack so float representation alone cannot decide
    // whether that picture is in or out.
    let tol = fd * 1e-3;

    let mut decoder =
        ff::codec::context::Context::from_parameters(params.clone())?.decoder().video()?;
    let mut encoder = open_encoder(src, &params, opts)?;

    seek_to(&mut ictx, src, seg.seek_from)?;

    let mut frame = ff::frame::Video::empty();
    let mut audio_done = vec![false; ctx.audio.len()];
    let mut caption_done = vec![false; ctx.captions.len()];
    let mut anchor: Option<f64> = None;
    let mut span = Span::default();
    // The encoder hands packets back in decode order, labelled only with the
    // PTS they went in with; this remembers how long each of those pictures
    // is meant to be shown.
    let mut placed: std::collections::HashMap<i64, (i64, i64)> = Default::default();
    let mut fed = 0i64;
    let mut past_end = false;

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
                    continue;
                }
                let a = *anchor.get_or_insert(t);
                let display = display_base + ((t - a) / field).round() as i64;
                let fields = 2 + unsafe { (*frame.as_ptr()).repeat_pict.max(0) as i64 };
                placed.insert(fed, (display, fields));
                span.fields = span.fields.max(display - display_base + fields);
                span.pictures += 1;
                frame.set_pts(Some(fed));
                fed += 1;
                frame.set_kind(ff::picture::Type::None);
                mark_interlacing(&mut frame, &src.video);
                encoder.send_frame(&frame)?;
                drain_encoder(&mut encoder, reframe, &placed, writer)?;
            }
        };
    }

    for (stream, packet) in ictx.packets() {
        let index = stream.index();
        if index != ist_index {
            if let Some(k) = ctx.audio.iter().position(|a| a.in_index == index) {
                if !audio_done[k] {
                    audio_done[k] =
                        take_audio(&ctx.audio[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.captions.iter().position(|c| c.in_index == index) {
                if !caption_done[k] {
                    caption_done[k] = take_caption(&ctx.captions[k], src, seg, packet, writer)?;
                }
            }
            continue;
        }
        // See [`TRAIL`]: past the end and still decoding only to wait for a
        // stream that is not coming.
        if past_end {
            if audio_done.iter().all(|&d| d) && caption_done.iter().all(|&d| d) {
                break;
            }
            if packet.pts().is_some_and(|p| p as f64 * in_tb - src.start_time > seg.end + TRAIL) {
                break;
            }
        }
        decoder.send_packet(&packet)?;
        // Decoded frames arrive in display order, so a simple window test
        // picks out exactly the pictures this segment owns.
        feed!();
    }
    decoder.send_eof()?;
    feed!();
    let _ = past_end; // only the loop above acts on it
    encoder.send_eof()?;
    drain_encoder(&mut encoder, reframe, &placed, writer)?;

    if span.pictures == 0 {
        bail!("segment {:.3}-{:.3}: no pictures decoded", seg.start, seg.end);
    }
    Ok(span)
}

/// Ask the muxer to put a stream back on the PID it arrived on.
///
/// The MPEG-TS muxer reads a stream's id as the PID to write it on, and only
/// numbers from `mpegts_start_pid` the streams that do not name one. Which
/// is what lets a recording come back out on its own PIDs: the sound where
/// the tools downstream expect sound, the captions where a caption decoder
/// looks. Anything below 16 is not a PID a stream can sit on and is how
/// libav says it has no opinion.
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
    /// Channels out.
    channels: u16,
    /// The fold, when there is one: what it was, and what it became.
    downmix: Option<(u16, u16)>,
    /// What the recording's own frames of this track look like from outside.
    /// Only an answer for ADTS AAC, which is what a Japanese broadcast
    /// carries; anything else leaves the frames this tool encodes unframed,
    /// exactly as the packets they sit among are.
    source_adts: Option<crate::adts::AdtsFormat>,
    /// How the frames this cut encodes for the track are framed.
    frame_as: Option<crate::adts::AdtsFormat>,
    aac: AacVersion,
    bit_rate: usize,
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
    many: bool,
) -> Result<AudioSetup> {
    let named = if many { format!(" on PID 0x{:04x}", info.pid) } else { String::new() };
    let source_adts = {
        let mut probe = ff::format::input(&path)?;
        crate::adts::framing(&mut probe, info.stream_index)
    };
    // Channels out, and whether that is a downmix. A downmix is the one audio
    // setting that decides the mode rather than living under it: there is no
    // copying a 5.1 frame into a stereo track, so it is a whole-track
    // re-encode or it is nothing.
    let channels = opts.audio_channels.unwrap_or(info.channels);
    let downmix = (channels != info.channels).then_some((info.channels, channels));
    let mode = match downmix {
        Some((from, to)) if opts.audio_mode != AudioMode::Reencode => {
            eprintln!(
                "note: {from} channels were asked for as {to}{named}, which no frame of the \
                 recording's own can be copied through, so the whole track is re-encoded \
                 rather than {}.",
                opts.audio_mode.as_str(),
            );
            AudioMode::Reencode
        }
        _ => opts.audio_mode,
    };
    // Asking for the AAC the recording does not carry only reaches the frames
    // written here -- the copied ones keep the headers they came with -- so
    // honouring it would leave the output two kinds of AAC at once, which is
    // the very thing this framing exists to prevent. A whole-track re-encode
    // is the exception: nothing is copied there, so there is nothing to
    // disagree with.
    let aac = match (opts.aac.forced(), source_adts) {
        (Some(want), Some(f)) if f.mpeg2 != want && mode != AudioMode::Reencode => {
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
    // the header is where a transport stream says so.
    let frame_as = match (mode, source_adts) {
        (AudioMode::Smart, Some(f)) => Some(f.as_version(aac)),
        (AudioMode::Reencode, Some(f)) if to_ts => Some(match downmix {
            Some((_, to)) => f.as_version(aac).with_channels(to),
            None => f.as_version(aac),
        }),
        _ => None,
    };
    // Following the recording's own rate is right until the channels stop
    // being the recording's: 384 kbit/s is what 5.1 cost, not what the stereo
    // it was folded into is worth, so the derived rate comes down with the
    // channel count. An explicit rate is taken as given.
    let bit_rate = opts.audio_bit_rate.unwrap_or_else(|| {
        let src_rate = info.bit_rate.unwrap_or(192_000);
        match downmix {
            Some((from, to)) if from > 0 => (src_rate * to as usize / from as usize).max(128_000),
            _ => src_rate,
        }
    });
    Ok(AudioSetup {
        info: info.clone(),
        mode,
        channels,
        downmix,
        source_adts,
        frame_as,
        aac,
        bit_rate,
    })
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
    video_pid: i32,
    range_starts: &[f64],
    plans: &[RangePlan],
    output: &str,
    tables: crate::si::Tables,
) -> Result<crate::si::Stats> {
    let mut streams = vec![crate::si::GraftStream { pid: video_pid as u16, faithful: true }];
    for setup in setups {
        streams.push(crate::si::GraftStream {
            pid: setup.info.pid as u16,
            // A folded track no longer has the channels the recording's own
            // audio component descriptor names, and saying it does is worse
            // than saying nothing.
            faithful: setup.downmix.is_none(),
        });
    }
    for c in captions {
        streams.push(crate::si::GraftStream { pid: c.pid as u16, faithful: true });
    }

    let ranges = plans
        .iter()
        .zip(range_starts)
        .map(|(plan, &start)| {
            let pos = src
                .points
                .iter()
                .rfind(|p| p.time <= plan.t_in + 1e-6 && p.pos >= 0)
                .map_or(0, |p| p.pos);
            let snapshot =
                crate::si::snapshot_at(&src.path, pos, service.service_id).unwrap_or_default();
            crate::si::GraftRange { start, snapshot }
        })
        .collect();

    crate::si::graft(
        output,
        &crate::si::Graft { service, streams, pcr_pid: video_pid as u16, ranges, tables },
    )
}

pub fn cut(src: &Source, plans: &[RangePlan], output: &str, opts: &CutOptions) -> Result<()> {
    cut_with_progress(src, plans, output, opts, None)
}

/// As [`cut`], reporting how far along it is.
pub fn cut_with_progress(
    src: &Source,
    plans: &[RangePlan],
    output: &str,
    opts: &CutOptions,
    progress: Option<Box<dyn Fn(f64) + Send + Sync>>,
) -> Result<()> {
    crate::init()?;

    let (ictx, ist_index) = open_input(&src.path)?;
    let params = ictx.stream(ist_index).unwrap().parameters();
    let extradata = unsafe {
        let p = params.as_ptr();
        if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec()
        }
    };
    let (num, den) = frame_rate_parts(src.video.frame_rate);

    // Output timebase: one tick per 1/(2*num) second, so a *field* is exactly
    // `den` ticks and a normal frame is `2*den`. 30000/1001 lands on 1/60000
    // with 1001 ticks per field -- integer arithmetic throughout, no rounding
    // anywhere on the timeline, and 3-field pictures are representable.
    let mut muxer_opts = ff::Dictionary::new();
    let timescale = (2 * num).to_string();
    let to_ts = writing_ts(output);
    if !to_ts {
        muxer_opts.set("video_track_timescale", &timescale);
    }
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
    let audios: Vec<crate::AudioInfo> =
        src.audios.iter().filter(|a| kept(a.stream_index)).cloned().collect();
    // Captions go into a transport stream and nowhere else. MP4 has no
    // sample entry for an ARIB caption stream -- there is nothing to declare
    // it as, and no format to turn it into that is still what it was.
    let captions: Vec<crate::CaptionInfo> = if to_ts {
        src.captions.iter().filter(|c| kept(c.stream_index)).cloned().collect()
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
    // What each sound track is and what will become of it. Settled before
    // anything is declared, because a stream has to be declared as the thing
    // it will contain and a downmixed track is not what the recording's own
    // parameters describe.
    let setups: Vec<AudioSetup> = audios
        .iter()
        .map(|a| plan_audio(&src.path, a, opts, to_ts, audios.len() > 1))
        .collect::<Result<Vec<_>>>()?;

    // What the recording says about itself. Read here rather than after the
    // cut because the muxer's own idea of the transport stream has to agree
    // with it: an event information section names its service by transport
    // stream and by network, and a player that finds those disagreeing with
    // the tables around them is right to believe neither.
    let wants_tables = to_ts && opts.tables != crate::si::Tables::Muxer;
    let tables = match wants_tables.then(|| crate::si::read_service(&src.path)) {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            eprintln!("note: {e}. The streams are kept; the broadcast's own tables are not.");
            None
        }
        None => None,
    };

    let pids;
    let pmt;
    let service;
    let tsid;
    let onid;
    let service_type;
    if to_ts {
        if let Some(l) = layout {
            pids = l.first_pid.to_string();
            pmt = l.pmt_pid.to_string();
            service = l.service_id.to_string();
            if l.first_pid > 0 {
                muxer_opts.set("mpegts_start_pid", &pids);
            }
            if l.pmt_pid > 0 {
                muxer_opts.set("mpegts_pmt_start_pid", &pmt);
            }
            if l.service_id > 0 {
                muxer_opts.set("mpegts_service_id", &service);
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
    }
    // Note: muxer options belong to `write_header`, not to opening the file --
    // `output_with` hands its dictionary to the *protocol*, so anything meant
    // for the muxer is quietly dropped there. This one goes below.
    let mut octx = ff::format::output(&output)?;
    let mp4ish = {
        let name = octx.format().name().to_string();
        name.contains("mp4") || name.contains("mov")
    };
    // Only MP4-family containers need the reframing dance; Annex-B containers
    // already carry parameter sets in-band, and their muxers convert as needed.
    let reframe = match (mp4ish, src.video.framing, src.video.codec.as_str()) {
        (true, NalFraming::Length(n), "h264" | "hevc") => {
            let sets = parameter_sets(&src.video.codec, &extradata);
            if sets.is_empty() { None } else { Some(Reframe { nal_length: n, sets }) }
        }
        _ => None,
    };
    {
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(params);
        ost.set_time_base(ff::Rational::new(1, 2 * num as i32));
        unsafe {
            // `avc3`/`hev1` say the parameter sets may live in the samples,
            // which is what lets copied and re-encoded pictures carry
            // different ones in the same track.
            (*ost.parameters().as_mut_ptr()).codec_tag = match (&reframe, src.video.codec.as_str())
            {
                (Some(_), "h264") => u32::from_le_bytes(*b"avc3"),
                (Some(_), "hevc") => u32::from_le_bytes(*b"hev1"),
                _ => 0,
            };
            set_pid(&mut ost, to_ts, video_pid);
        }
    }
    // Sound rides along beside the pictures: one output track for each the
    // recording carries, each copied packet for packet.
    let mut audio_pending: Vec<(usize, Option<crate::audio::Reencoder>)> = Vec::new();
    for setup in &setups {
        let params = ictx
            .stream(setup.info.stream_index)
            .ok_or_else(|| anyhow!("audio stream {} vanished", setup.info.stream_index))?
            .parameters();
        // Built before the stream is declared, because when it exists the
        // stream must describe *it* rather than the source.
        let reencoder = match setup.mode {
            AudioMode::Reencode => Some(crate::audio::Reencoder::new(
                params.clone(),
                &setup.info,
                setup.channels,
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
        ost.set_time_base(ff::Rational::new(1, setup.info.sample_rate as i32));
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
            set_pid(&mut ost, to_ts, setup.info.pid);
        }
        audio_pending.push((out_index, reencoder));
    }

    // Captions, likewise, and more simply: nothing about them is re-encoded
    // and nothing about them is spliced, so the stream is declared exactly
    // as it arrived. The muxer knows this codec and writes the descriptors
    // that say a Japanese player should look here for subtitles.
    let mut caption_pending: Vec<usize> = Vec::new();
    for info in &captions {
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
            set_pid(&mut ost, to_ts, info.pid);
        }
        caption_pending.push(out_index);
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
    let out_tb = octx.stream(0).ok_or_else(|| anyhow!("no output stream"))?.time_base();

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
                let rate = opts.audio_bit_rate.or(a.bit_rate).unwrap_or(192_000);
                crate::audio::boundary_patches(
                    src,
                    a,
                    &windows,
                    rate,
                    setup.source_adts,
                    setup.aac,
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
            out_tb: octx.stream(out_index).map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            in_index: setup.info.stream_index,
            info: setup.info.clone(),
            mode: setup.mode,
            written: 0,
            prev: None,
            end: None,
            reencoder,
            patches,
        })
        .collect();
    let caption_tracks: Vec<CaptionTrack> = captions
        .iter()
        .zip(caption_pending)
        .map(|(info, out_index)| CaptionTrack {
            out_index,
            out_tb: octx.stream(out_index).map_or(1.0 / 90_000.0, |s| f64::from(s.time_base())),
            in_index: info.stream_index,
            in_tb: info.time_base,
            written: 0,
        })
        .collect();

    let mut writer = Writer {
        octx,
        field_ticks: den,
        our_tb: ff::Rational::new(1, 2 * num as i32),
        out_tb,
        // DTS trails PTS by the stream's reorder depth. Being generous costs
        // nothing: the muxer writes an edit list for the negative lead-in,
        // just as it would for any encoder's output.
        depth: opts.reorder_depth.unwrap_or(src.video.has_b_frames.max(0) as i64),
        pending: Default::default(),
        seen: Default::default(),
        written: 0,
        audio: audio_tracks,
        captions: caption_tracks,
        progress,
        expected: plans.iter().flat_map(|p| &p.segments).map(|s| s.frames as i64).sum(),
    };

    let fps = num as f64 / den as f64;
    let mut display_base: i64 = 0;
    let mut pictures: i64 = 0;
    // Where each kept range began in the output, which is what the tables
    // grafted on afterwards are placed against.
    let mut range_starts: Vec<f64> = Vec::with_capacity(plans.len());
    for plan in plans {
        // Anchor this range's audio to the output time its video starts at.
        // display_base counts fields, so two per frame.
        let target_start = display_base as f64 / (2.0 * fps);
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
                    min_start: if t.end.is_none() { plan.t_in } else { f64::NEG_INFINITY },
                    window: (
                        (plan.t_in * t.info.sample_rate as f64).round() as i64,
                        (plan.t_out * t.info.sample_rate as f64).round() as i64,
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
            })
            .collect();
        for (n, seg) in plan.segments.iter().enumerate() {
            let first_segment = n == 0;
            let ctx = SegmentCtx {
                display_base,
                reframe: reframe.as_ref(),
                audio: &audio_ctx,
                captions: &caption_ctx,
                first: first_segment,
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

    if writer.written != pictures {
        bail!("segments reported {pictures} pictures, wrote {}", writer.written);
    }

    // The file is complete and correct as a file; what it does not yet have
    // is the broadcast's own account of itself. See [`crate::si`].
    if let Some(service) = tables.as_ref() {
        match graft_tables(
            src,
            service,
            &setups,
            &captions,
            video_pid,
            &range_starts,
            plans,
            output,
            opts.tables,
        ) {
            Ok(stats) if std::env::var("SMARTCUT_DEBUG").is_ok() => {
                eprintln!(
                    "  tables: {} map, {} service, {} event, {} clock, {} selection",
                    stats.pmt, stats.sdt, stats.eit, stats.tot, stats.sit
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

    if let Some(report) = &writer.progress {
        report(1.0);
    }
    Ok(())
}
