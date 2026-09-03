//! Audio that is cut where the video is, without re-encoding all of it.
//!
//! Copying audio frames leaves each range's boundary on a whole-frame edge --
//! about half a frame out, 10.7 ms for AAC at 48 kHz. That cannot be trimmed
//! away in the container: this MP4 muxer honours neither
//! `AV_PKT_DATA_SKIP_SAMPLES` nor a stream's `initial_padding`, both of which
//! were tried and measured.
//!
//! Two ways out, and they are the two modes below.
//!
//! [`Reencoder`] decodes the whole track, trims to the sample and encodes it
//! again. Sample-exact everywhere, and lossy everywhere.
//!
//! [`boundary_patches`] does to audio what the rest of this tool does to
//! video: it re-encodes the frames the cut lands *inside* -- two per range
//! edge -- and leaves every other frame the recording's own bytes. The
//! straddling frame is encoded from the recording's own samples with the far
//! side of the cut faded to silence.
//!
//! That does not move the boundary: a range still occupies a whole number of
//! frames, because two frames cannot share an instant in any container this
//! writes. What it removes is what fills the rest of the straddling frame.
//! Copied, that frame carries up to 21 ms of the material the cut was made to
//! get rid of -- the last syllable of a commercial, arriving after the
//! picture has already moved on, at every seam. Patched, it carries silence.
//!
//! The second emitted frame is a guard. AAC frames overlap by half a window,
//! so the decoder rebuilds each frame from that frame and its neighbour: a
//! re-encoded frame sitting directly against a copied one is the one place
//! the two halves can disagree. Silencing part of a frame is a transient, and
//! a transient is what makes the encoder switch to short windows -- which is
//! exactly the disagreement that would be audible. The guard frame carries
//! the recording's own samples, unmasked, and keeps the switch one frame away
//! from the copied material.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use std::collections::HashMap;

use crate::adts::{AacVersion, AdtsFormat};
use crate::{AudioInfo, Source};

/// How long the fade into and out of a silenced stretch is, in samples.
///
/// A hard step in the middle of a frame is a transient, and the encoder
/// answers a transient with short windows; a millisecond of raised cosine
/// costs nothing anyone can hear and keeps the frame long.
const FADE: usize = 48;

/// Build the encoder that stands in for the recording's own.
///
/// Perceptual noise substitution is off: it is an MPEG-4 tool, and a stream
/// that announces itself as MPEG-2 AAC must not contain one. It is on by
/// default in this encoder, so a frame written into an ARIB recording without
/// this is a frame an MPEG-2 decoder is entitled to refuse.
///
/// `channels` is what comes *out*, which is the recording's own count except
/// where a downmix was asked for.
fn open_encoder(
    params: &ff::codec::Parameters,
    audio: &AudioInfo,
    channels: u16,
    bit_rate: usize,
) -> Result<ff::encoder::Audio> {
    let id = params.id();
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no encoder for {id:?}"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
    enc.set_rate(audio.sample_rate as i32);
    enc.set_channel_layout(ff::channel_layout::ChannelLayout::default(channels as i32));
    enc.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
    enc.set_bit_rate(bit_rate);
    enc.set_time_base(ff::Rational::new(1, audio.sample_rate as i32));
    let mut eopts = ff::Dictionary::new();
    if id == ff::codec::Id::AAC {
        eopts.set("aac_pns", "0");
        eopts.set("aac_ltp", "0");
        eopts.set("aac_pred", "0");
    }
    enc.open_as_with(codec, eopts).map_err(|e| anyhow!("cannot open audio encoder: {e}"))
}

/// Where a packet's samples begin, relative to the first sample fed in.
///
/// The encoder says so itself: it declares a delay -- one frame, for AAC --
/// and then stamps its output accordingly, so its opening packet, the one
/// covering the window that reaches back before anything was fed, comes out
/// at -1024, and the packet covering the first frame fed comes out at 0.
/// Counting the delay off separately as well, as an `initial_padding`
/// divided into frames, takes it off twice and lands a frame early.
///
/// `None` for the packet that is all priming.
fn at_sample(packet: &ff::Packet) -> Option<i64> {
    packet.pts().filter(|&pts| pts >= 0)
}

fn frame_size_of(encoder: &ff::encoder::Audio) -> usize {
    if encoder.frame_size() > 0 { encoder.frame_size() as usize } else { 1024 }
}

/// Planar float, one buffer per channel.
type Pcm = Vec<Vec<f32>>;

/// What the encoder is fed and what the buffers below hold.
const PLANAR_F32: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Planar);

/// A decoded frame in the shape the encoder wants it: planar float, with the
/// channels the output is to have.
///
/// The frame is handed straight back when it is already that, which is the
/// ordinary case -- an AAC recording re-encoded to its own channel count.
/// Otherwise swresample does the work: the rematrixing coefficients for 5.1
/// into stereo are libav's own, so what comes out is what a player downmixing
/// the recording would have produced.
///
/// In and out at the same rate, which is what makes this a per-frame
/// operation: swresample returns a frame's samples one for one and holds
/// nothing back between frames, so the sample window the caller trims
/// against still means what it meant on the source's own clock.
fn rematrix<'a>(
    resampler: &mut Option<ff::software::resampling::Context>,
    out: &'a mut ff::frame::Audio,
    frame: &'a ff::frame::Audio,
    channels: u16,
) -> Result<&'a ff::frame::Audio> {
    if frame.channels() == channels && frame.format() == PLANAR_F32 {
        return Ok(frame);
    }
    let layout = ff::channel_layout::ChannelLayout::default(channels as i32);
    let ctx = match resampler {
        Some(ctx) => ctx,
        None => resampler.insert(ff::software::resampling::Context::get(
            frame.format(),
            frame.channel_layout(),
            frame.rate(),
            PLANAR_F32,
            layout,
            frame.rate(),
        )?),
    };
    *out = ff::frame::Audio::new(PLANAR_F32, frame.samples().max(1), layout);
    ctx.run(frame, out)?;
    Ok(out)
}

/// Copy a decoded frame's samples out, whatever layout it arrived in.
fn take_samples(frame: &ff::frame::Audio, channels: usize, into: &mut Pcm, range: (usize, usize)) {
    let (a, b) = range;
    for (ch, buf) in into.iter_mut().enumerate().take(channels) {
        let plane = if frame.is_planar() { ch } else { 0 };
        let data: &[f32] = frame.plane(plane);
        if frame.is_planar() {
            buf.extend_from_slice(&data[a..b]);
        } else {
            buf.extend((a..b).map(|i| data[i * channels + ch]));
        }
    }
}

pub struct Reencoder {
    decoder: ff::decoder::Audio,
    encoder: ff::encoder::Audio,
    pending: Pcm,
    frame_size: usize,
    /// Channels written out, which is the source's own unless a downmix was
    /// asked for.
    channels: usize,
    /// Built on the first frame, and only when the decoded frames are not
    /// already the shape the encoder wants.
    resampler: Option<ff::software::resampling::Context>,
    remixed: ff::frame::Audio,
    sample_rate: u32,
    /// Samples handed to the encoder so far -- the output timeline.
    fed: i64,
    /// Last source packet consumed, so a frame offered twice is ignored.
    last_pts: Option<i64>,
    /// Set when the frames are to leave here already framed, which is what
    /// keeps a re-encoded transport stream MPEG-2 AAC throughout.
    adts: Option<AdtsFormat>,
}

impl Reencoder {
    /// `channels` is what the track is written with: `audio.channels` to
    /// follow the recording, fewer to downmix it.
    pub fn new(
        params: ff::codec::Parameters,
        audio: &AudioInfo,
        channels: u16,
        bit_rate: usize,
        adts: Option<AdtsFormat>,
    ) -> Result<Self> {
        let decoder =
            ff::codec::context::Context::from_parameters(params.clone())?.decoder().audio()?;
        let encoder = open_encoder(&params, audio, channels, bit_rate)?;
        let frame_size = frame_size_of(&encoder);
        Ok(Self {
            decoder,
            encoder,
            pending: vec![Vec::new(); channels as usize],
            frame_size,
            channels: channels as usize,
            resampler: None,
            remixed: ff::frame::Audio::empty(),
            sample_rate: audio.sample_rate,
            fed: 0,
            last_pts: None,
            adts,
        })
    }

    /// What the output stream has to say about itself once this encoder is
    /// the one producing the packets.
    ///
    /// Not the source stream's parameters: they describe a different
    /// bitstream. MPEG-TS in particular reframes raw AAC into ADTS using the
    /// stream's own extradata, and given the wrong extradata it writes a PID
    /// full of bytes no decoder can find a sync word in.
    ///
    /// Moot when the frames leave here framed already and carry the source's
    /// own channels -- then the source's parameters are the true ones,
    /// because the frames are the same shape as the source's were. A downmix
    /// makes them a different shape, and these the true ones again.
    pub fn parameters(&self) -> ff::codec::Parameters {
        let mut par = ff::codec::Parameters::new();
        unsafe {
            ff::ffi::avcodec_parameters_from_context(par.as_mut_ptr(), self.encoder.as_ptr());
        }
        par
    }

    /// Decode a packet and keep whatever falls inside `[from, to)` samples of
    /// the source timeline.
    pub fn take(
        &mut self,
        packet: &ff::Packet,
        audio: &AudioInfo,
        start_time: f64,
        window: (i64, i64),
    ) -> Result<()> {
        if let (Some(pts), Some(last)) = (packet.pts(), self.last_pts) {
            if pts <= last {
                return Ok(());
            }
        }
        self.last_pts = packet.pts().or(self.last_pts);
        if self.decoder.send_packet(packet).is_err() {
            return Ok(());
        }
        let mut frame = ff::frame::Audio::empty();
        while self.decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * audio.time_base - start_time;
            let first = (t * self.sample_rate as f64).round() as i64;
            let n = frame.samples() as i64;
            let lo = window.0.max(first);
            let hi = window.1.min(first + n);
            if hi <= lo {
                continue;
            }
            let src = rematrix(
                &mut self.resampler,
                &mut self.remixed,
                &frame,
                self.channels as u16,
            )?;
            // Same rate in and out, so the window still counts in the frame's
            // own samples; clamped all the same, so a shorter frame than was
            // asked for cannot index past its buffers.
            let have = src.samples();
            let a = ((lo - first) as usize).min(have);
            let b = ((hi - first) as usize).min(have);
            take_samples(src, self.channels, &mut self.pending, (a, b));
        }
        Ok(())
    }

    /// Hand the encoder every full frame that has accumulated.
    pub fn drain(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        while self.pending[0].len() >= self.frame_size {
            let mut frame = ff::frame::Audio::new(
                PLANAR_F32,
                self.frame_size,
                ff::channel_layout::ChannelLayout::default(self.channels as i32),
            );
            frame.set_rate(self.sample_rate);
            for ch in 0..self.channels {
                let taken: Vec<f32> = self.pending[ch].drain(..self.frame_size).collect();
                frame.plane_mut::<f32>(ch)[..self.frame_size].copy_from_slice(&taken);
            }
            frame.set_pts(Some(self.fed));
            self.fed += self.frame_size as i64;
            self.encoder.send_frame(&frame)?;
            self.collect(out)?;
        }
        Ok(())
    }

    /// Finish: pad the tail to a whole frame and flush the encoder.
    pub fn finish(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        if !self.pending[0].is_empty() {
            let short = self.frame_size - self.pending[0].len();
            for ch in 0..self.channels {
                self.pending[ch].extend(std::iter::repeat_n(0.0, short));
            }
            self.drain(out)?;
        }
        self.encoder.send_eof()?;
        self.collect(out)
    }

    fn collect(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        loop {
            let mut packet = ff::Packet::empty();
            if self.encoder.receive_packet(&mut packet).is_err() {
                return Ok(());
            }
            // The priming packet holds the encoder's warm-up and nothing of
            // the audio, so there is nothing in it to write.
            let Some(pts) = at_sample(&packet) else { continue };
            let packet = match &self.adts {
                Some(f) => {
                    let mut framed = ff::Packet::copy(&f.wrap(packet.data().unwrap_or(&[])));
                    framed.set_flags(ff::packet::Flags::KEY);
                    framed
                }
                None => packet,
            };
            out.push((packet, pts));
        }
    }
}

/// A frame to write in place of one of the recording's own.
pub struct Patch {
    /// The frame, framed as the recording's frames are.
    pub bytes: Vec<u8>,
    /// A guard is only worth its re-encode if the frame it guards is written
    /// at all: this is that frame's `pts`, and the guard is to be used only
    /// when it is what came immediately before. A head boundary's straddling
    /// frame reaches back before the range and is not always the frame the
    /// range opens on -- when it is not, the guard has nothing to guard and
    /// the recording's own bytes are the better answer.
    pub after: Option<i64>,
}

/// A frame of the recording, decoded, with where it sits.
struct Decoded {
    /// The source packet this frame arrived in, which is the key a
    /// replacement is looked up by.
    pts: i64,
    /// First sample, on the source's own sample clock.
    first: i64,
    pcm: Pcm,
}

impl Decoded {
    fn len(&self) -> usize {
        self.pcm[0].len()
    }
}

/// Re-encode the frames a keep-range's edges fall inside.
///
/// The result maps a source packet's `pts` to the bytes that should be
/// written in its place. Anything not in the map is copied.
///
/// One encoder per run of replaced frames, primed with the frame before and
/// flushed through the frame after, so that the frames handed back cover
/// exactly the samples the frames they replace covered. Runs from different
/// edges may overlap on a very short range; they agree where they do, because
/// the mask both are built against is the whole range.
pub fn boundary_patches(
    src: &Source,
    audio: &AudioInfo,
    windows: &[(i64, i64)],
    bit_rate: usize,
    adts: Option<AdtsFormat>,
    aac: AacVersion,
) -> Result<HashMap<i64, Patch>> {
    let mut out = HashMap::new();
    if windows.is_empty() {
        return Ok(out);
    }
    let mut ictx = ff::format::input(&src.path)?;
    let stream = ictx
        .stream(audio.stream_index)
        .ok_or_else(|| anyhow!("audio stream {} vanished", audio.stream_index))?;
    let params = stream.parameters();
    let rate = audio.sample_rate as f64;
    let adts = adts.map(|f| f.as_version(aac));

    // A frame this encoder produces can only stand in for one of the
    // recording's if the two are framed alike, and the encoder's delay is
    // what says whether they are. AAC's is a whole frame, so its packets land
    // on the same grid the recording's frames sit on. AC-3's is 256 samples,
    // which puts every packet 256 samples off that grid -- its frames cover
    // the wrong samples to be swapped in for the recording's, however well
    // they are encoded. Better to say so and copy than to quietly do nothing.
    // No encoder for this audio, or none that will take what we would feed
    // it: MP2 wants signed 16-bit and is handed planar float here. Smart
    // rendering is an improvement on copying, not a condition of it, so this
    // says so and copies rather than failing the cut.
    let probe = match open_encoder(&params, audio, audio.channels, bit_rate) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "note: {:?} audio cannot be re-encoded here ({e}), so it was copied, \
                 boundaries and all.",
                params.id()
            );
            return Ok(out);
        }
    };
    let size = frame_size_of(&probe);
    let delay = unsafe { (*probe.as_ptr()).initial_padding.max(0) as usize };
    if size == 0 || delay % size != 0 {
        eprintln!(
            "note: the {:?} encoder frames its output {} samples out of step with the \
             recording, so no frame it makes can replace one of the recording's. \
             The audio was copied, boundaries and all.",
            params.id(),
            delay % size.max(1),
        );
        return Ok(out);
    }
    drop(probe);

    for &(w0, w1) in windows {
        for (edge, is_head) in [(w0, true), (w1, false)] {
            let at = edge as f64 / rate;
            // A range edge that already sits on a frame boundary needs no
            // patch at all -- but that is not known until the frames are in
            // hand, so the decode happens first and the work is skipped after.
            let frames = decode_around(&mut ictx, &params, src, audio, at)?;
            let Some(f) = straddler(&frames, edge, is_head) else { continue };
            patch_run(
                &frames,
                f,
                is_head,
                (w0, w1),
                &params,
                audio,
                bit_rate,
                adts.as_ref(),
                &mut out,
            )?;
        }
    }
    Ok(out)
}

/// Decode the frames around `at`, with enough lead-in that their samples are
/// the ones the recording's own decoder would produce.
///
/// AAC frames overlap, so the frame decoded straight after a seek is missing
/// half its window and comes out wrong. Half a second of lead-in is some
/// twenty frames of it, which is more than enough and costs nothing.
fn decode_around(
    ictx: &mut ff::format::context::Input,
    params: &ff::codec::Parameters,
    src: &Source,
    audio: &AudioInfo,
    at: f64,
) -> Result<Vec<Decoded>> {
    const LEAD: f64 = 0.5;
    /// Frames to keep either side of the edge: the straddler, its guard, and
    /// the lead-in and lead-out the encoder needs around both.
    const KEEP: usize = 6;

    let landing = (at - LEAD).max(0.0) + src.start_time;
    let target = if at - LEAD <= 0.0 {
        i64::MIN / 2
    } else {
        (landing * ff::ffi::AV_TIME_BASE as f64) as i64
    };
    ictx.seek(target, ..target)?;

    let mut decoder =
        ff::codec::context::Context::from_parameters(params.clone())?.decoder().audio()?;
    let channels = audio.channels as usize;
    let rate = audio.sample_rate as f64;
    let mut frames: Vec<Decoded> = Vec::new();
    let mut frame = ff::frame::Audio::empty();
    let mut past = 0usize;

    for (stream, packet) in ictx.packets() {
        if stream.index() != audio.stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * audio.time_base - src.start_time;
            let first = (t * rate).round() as i64;
            let n = frame.samples();
            let mut pcm: Pcm = vec![Vec::with_capacity(n); channels];
            take_samples(&frame, channels, &mut pcm, (0, n));
            frames.push(Decoded { pts, first, pcm });
            if t > at {
                past += 1;
            }
            // Only the frames around the edge are wanted; the rest was
            // lead-in for the decoder.
            if frames.len() > 2 * KEEP {
                frames.remove(0);
            }
        }
        if past > KEEP {
            break;
        }
    }
    Ok(frames)
}

/// The frame the edge falls inside, when it falls inside one at all.
///
/// A head edge is claimed by the frame containing it; a tail edge by the
/// frame containing the last sample before it. An edge already on a frame
/// boundary is claimed by neither -- there is nothing to trim.
fn straddler(frames: &[Decoded], edge: i64, is_head: bool) -> Option<usize> {
    let at = if is_head { edge } else { edge - 1 };
    let i = frames.iter().position(|f| f.first <= at && at < f.first + f.len() as i64)?;
    let f = &frames[i];
    let aligned = if is_head { edge == f.first } else { edge == f.first + f.len() as i64 };
    (!aligned).then_some(i)
}

/// Encode the straddling frame and its guard, and record what they replace.
#[allow(clippy::too_many_arguments)]
fn patch_run(
    frames: &[Decoded],
    straddle: usize,
    is_head: bool,
    window: (i64, i64),
    params: &ff::codec::Parameters,
    audio: &AudioInfo,
    bit_rate: usize,
    adts: Option<&AdtsFormat>,
    out: &mut HashMap<i64, Patch>,
) -> Result<()> {
    // The guard sits on the kept side; the lead-in and lead-out are one frame
    // beyond each end of what is emitted.
    let (emit_first, emit_last) =
        if is_head { (straddle, straddle + 1) } else { (straddle.saturating_sub(1), straddle) };
    let (Some(lead), Some(trail)) = (emit_first.checked_sub(1), emit_last.checked_add(1)) else {
        return Ok(());
    };
    if trail >= frames.len() {
        // Not enough of the recording either side to prime the encoder with;
        // the frame stays the recording's own, boundary and all.
        return Ok(());
    }
    // Nothing to remove, nothing to re-encode. A commercial break is cut in
    // the silence the broadcaster leaves either side of it, and there the far
    // half of the straddling frame is already silent: replacing it would
    // spend one of the recording's own frames to change nothing anyone can
    // hear. TMPGEnc's smart renderer leans on this entirely -- measured
    // against its source, a 24-minute cut of a broadcast re-encodes not one
    // audio frame, because all three of its joins sit in digital silence.
    if !outside_is_audible(&frames[straddle], window) {
        return Ok(());
    }

    let run = &frames[lead..=trail];
    let size = run[0].len();
    if run.iter().any(|f| f.len() != size) {
        return Ok(());
    }

    let mut encoder = open_encoder(params, audio, audio.channels, bit_rate)?;
    if frame_size_of(&encoder) != size {
        // The encoder frames the audio differently from the recording, so a
        // frame it produces cannot stand in for one of the recording's.
        return Ok(());
    }
    let channels = audio.channels as usize;

    let mut fed = 0i64;
    let mut got: Vec<(usize, Vec<u8>)> = Vec::new();
    for f in run {
        let mut frame = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            size,
            ff::channel_layout::ChannelLayout::default(channels as i32),
        );
        frame.set_rate(audio.sample_rate);
        for ch in 0..channels {
            let mut samples = f.pcm[ch].clone();
            mask(&mut samples, f.first, window);
            frame.plane_mut::<f32>(ch)[..size].copy_from_slice(&samples);
        }
        frame.set_pts(Some(fed));
        fed += size as i64;
        encoder.send_frame(&frame)?;
        // Between sends, not only at the end: an encoder holding a full
        // output queue refuses the next frame outright.
        collect_patch(&mut encoder, size, &mut got)?;
    }
    encoder.send_eof()?;
    collect_patch(&mut encoder, size, &mut got)?;

    for (at, data) in got {
        let i = at + lead;
        if i < emit_first || i > emit_last {
            continue;
        }
        let bytes = match adts {
            Some(f) => f.wrap(&data),
            None => data,
        };
        // At a head boundary the straddling frame comes first and the guard
        // second; at a tail boundary the guard comes first and the straddler
        // -- which the range always ends on -- second.
        let after = (is_head && i == straddle + 1).then(|| frames[straddle].pts);
        out.entry(frames[i].pts).or_insert(Patch { bytes, after });
    }
    Ok(())
}

/// Take what the encoder is holding, labelled with how many frames into the
/// run each packet's samples begin.
fn collect_patch(
    encoder: &mut ff::encoder::Audio,
    size: usize,
    got: &mut Vec<(usize, Vec<u8>)>,
) -> Result<()> {
    loop {
        let mut packet = ff::Packet::empty();
        if encoder.receive_packet(&mut packet).is_err() {
            return Ok(());
        }
        let Some(offset) = at_sample(&packet) else { continue };
        if offset % size as i64 != 0 {
            continue;
        }
        got.push(((offset / size as i64) as usize, packet.data().unwrap_or(&[]).to_vec()));
    }
}

/// Whether the part of a frame outside the keep-range can be heard at all.
///
/// -60 dBFS is the bar. Below it, what the far side of the cut leaves in this
/// frame is inaudible under the material either side of it, and the frame is
/// better left as the recording wrote it: a copied frame is exact, and a
/// re-encoded one is only ever as good as the encoder.
fn outside_is_audible(frame: &Decoded, window: (i64, i64)) -> bool {
    const FLOOR: f32 = 0.001;
    let n = frame.len() as i64;
    let head = (window.0 - frame.first).clamp(0, n) as usize;
    let tail = (window.1 - frame.first).clamp(0, n) as usize;
    frame
        .pcm
        .iter()
        .any(|ch| ch[..head].iter().chain(&ch[tail..]).any(|v| v.abs() > FLOOR))
}

/// Fade to silence everything outside the keep-range.
///
/// `first` is where these samples start on the source's sample clock.
fn mask(samples: &mut [f32], first: i64, window: (i64, i64)) {
    let n = samples.len() as i64;
    let (w0, w1) = window;
    let head = (w0 - first).clamp(0, n) as usize;
    let tail = (w1 - first).clamp(0, n) as usize;
    samples[..head].fill(0.0);
    samples[tail..].fill(0.0);
    // Raised cosine into the kept side, so the step the encoder has to code
    // is a slope instead of a cliff.
    let fade = FADE.min(tail.saturating_sub(head));
    if head > 0 {
        for (k, s) in samples[head..head + fade].iter_mut().enumerate() {
            *s *= ramp(k, fade);
        }
    }
    if tail < samples.len() {
        for (k, s) in samples[tail - fade..tail].iter_mut().enumerate() {
            *s *= ramp(fade - 1 - k, fade);
        }
    }
}

fn ramp(k: usize, fade: usize) -> f32 {
    if fade == 0 {
        return 1.0;
    }
    let x = (k as f32 + 0.5) / fade as f32;
    0.5 - 0.5 * (std::f32::consts::PI * x).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(first: i64, n: usize) -> Decoded {
        Decoded { pts: first, first, pcm: vec![vec![1.0; n]] }
    }

    #[test]
    fn finds_the_frame_a_cut_lands_in() {
        let frames: Vec<Decoded> = (0..4).map(|i| decoded(i * 1024, 1024)).collect();
        assert_eq!(straddler(&frames, 1500, true), Some(1));
        assert_eq!(straddler(&frames, 1500, false), Some(1));
        // Already on a boundary: nothing to trim either way.
        assert_eq!(straddler(&frames, 1024, true), None);
        assert_eq!(straddler(&frames, 1024, false), None);
        assert_eq!(straddler(&frames, 99999, true), None);
    }

    #[test]
    fn silences_the_cut_away_side() {
        let mut s = vec![1.0f32; 1024];
        mask(&mut s, 0, (400, 1 << 30));
        assert!(s[..400].iter().all(|&v| v == 0.0));
        assert!(s[400 + FADE..].iter().all(|&v| v == 1.0));
        // The fade runs up, not down.
        assert!(s[400] < s[400 + FADE - 1]);

        let mut s = vec![1.0f32; 1024];
        mask(&mut s, 0, (0, 700));
        assert!(s[700..].iter().all(|&v| v == 0.0));
        assert!(s[..700 - FADE].iter().all(|&v| v == 1.0));
        assert!(s[699] < s[700 - FADE]);
    }

    #[test]
    fn hears_the_far_side_of_a_cut_only_when_there_is_something_there() {
        let mut d = decoded(0, 1024);
        // The range starts 400 samples in; everything before it is silence,
        // which is what a cut made in a commercial break looks like.
        d.pcm[0][..400].fill(0.0);
        assert!(!outside_is_audible(&d, (400, 1 << 30)));
        // A sample below the floor is still nothing to remove.
        d.pcm[0][100] = 0.0005;
        assert!(!outside_is_audible(&d, (400, 1 << 30)));
        // Material on the far side of the cut is.
        d.pcm[0][100] = 0.5;
        assert!(outside_is_audible(&d, (400, 1 << 30)));
        // The same, at the tail edge.
        let mut d = decoded(0, 1024);
        d.pcm[0][700..].fill(0.0);
        assert!(!outside_is_audible(&d, (0, 700)));
        d.pcm[0][900] = -0.5;
        assert!(outside_is_audible(&d, (0, 700)));
    }

    #[test]
    fn leaves_a_frame_inside_the_range_alone() {
        let mut s = vec![1.0f32; 1024];
        mask(&mut s, 4096, (0, 1 << 30));
        assert!(s.iter().all(|&v| v == 1.0));
    }
}
