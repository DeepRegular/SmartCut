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

/// The sample format a set of stream parameters names.
///
/// `AVCodecParameters` keeps it as a plain integer rather than as the enum,
/// so it is matched against the formats rather than cast into one.
pub fn sample_format(params: &ff::codec::Parameters) -> ff::format::Sample {
    use ff::format::sample::Type::{Packed, Planar};
    use ff::format::Sample::{F32, F64, I16, I32, I64, U8};
    let raw = unsafe { (*params.as_ptr()).format };
    [
        U8(Packed),
        U8(Planar),
        I16(Packed),
        I16(Planar),
        I32(Packed),
        I32(Planar),
        I64(Packed),
        I64(Planar),
        F32(Packed),
        F32(Planar),
        F64(Packed),
        F64(Planar),
    ]
    .into_iter()
    .find(|f| ff::ffi::AVSampleFormat::from(*f) as i32 == raw)
    .unwrap_or(ff::format::Sample::None)
}

/// The sample format to hand an encoder, given what the recording's own
/// samples are.
///
/// Planar float is what everything here holds and what the AAC and AC-3
/// encoders want, so for a broadcast recording this answers itself. Not every
/// encoder takes it: Blu-ray LPCM wants 16 or 32 bit integers, MP2 wants 16.
/// Handed a format it does not list, `avcodec_open2` refuses to open at all --
/// which is the whole reason an LPCM track used to come out of a cut copied,
/// boundaries and all.
///
/// So ask the encoder, and ask for the recording's own width first. That is
/// not only about keeping the samples: an encoder's *frame* can hang off it.
/// The Blu-ray LPCM encoder frames 16 bit sound 240 samples at a time and
/// 24 bit sound 360, so a 16 bit recording asked for as 24 comes back framed
/// differently from itself -- and a frame that does not line up with the
/// recording's own is a frame that cannot stand in for one.
fn encoder_format(
    codec: &ff::codec::codec::Codec,
    like: ff::format::Sample,
) -> ff::format::Sample {
    let Ok(audio) = codec.audio() else { return PLANAR_F32 };
    let Some(listed) = audio.formats() else { return PLANAR_F32 };
    let listed: Vec<ff::format::Sample> = listed.collect();
    if listed.is_empty() {
        return PLANAR_F32;
    }
    let offered = |f: ff::format::Sample| listed.contains(&f);
    // The recording's own, laid out either way; then planar float, which is
    // what these buffers hold; then the widest on the list, because narrowing
    // is the one conversion that costs the recording something.
    [like, like.planar(), like.packed(), PLANAR_F32, PLANAR_F32.packed()]
        .into_iter()
        .find(|f| *f != ff::format::Sample::None && offered(*f))
        .or_else(|| {
            listed.iter().copied().max_by_key(|f| f.bytes() * 2 + usize::from(f.is_planar()))
        })
        .unwrap_or(PLANAR_F32)
}

/// The channel layout to open an encoder with, given how many channels the
/// output is to have.
///
/// Six channels are not one arrangement but several, and an encoder that
/// takes 5.1 need not take the one libav hands back as the default for six:
/// the DTS encoder wants the surrounds at the sides and the Blu-ray LPCM
/// encoder has a list of its own. Handed a layout it does not list,
/// `avcodec_open2` refuses to open -- the same failure as a sample format it
/// does not list, and answered the same way, by asking the encoder.
///
/// The default is preferred where it is on the list, because it is the
/// arrangement the rest of libav assumes; otherwise the first the encoder
/// names with the right number of channels. An encoder that lists nothing
/// takes anything.
///
/// `None` where the encoder lists layouts and not one of them has this many
/// channels at all: DTS is written mono, stereo, quad, 5.0 or 5.1 and in no
/// other count, so a three channel recording asked for as DTS has no answer
/// here. That is not a layout to guess at -- a guess is a refusal from
/// `avcodec_open2` a moment later, with nothing in it about channels -- so
/// it is said here instead, and [`opens_at`] is how the window asks the
/// question before anyone chooses.
fn encoder_layout(
    codec: &ff::codec::codec::Codec,
    channels: u16,
) -> Option<ff::channel_layout::ChannelLayout> {
    let want = ff::channel_layout::ChannelLayout::default(channels as i32);
    let Ok(audio) = codec.audio() else { return Some(want) };
    let Some(listed) = audio.channel_layouts() else { return Some(want) };
    let listed: Vec<ff::channel_layout::ChannelLayout> = listed.collect();
    if listed.is_empty() || listed.contains(&want) {
        return Some(want);
    }
    listed.into_iter().find(|l| l.channels() == channels as i32)
}

/// The sample rate to open an encoder with, given the rate the output is to
/// run at.
///
/// An encoder need not take every rate. AC-3 has three -- 32, 44.1 and
/// 48 kHz -- and MP2 those three and their halves; handed anything else,
/// `avcodec_open2` refuses to open. That is the same failure as a sample
/// format or a channel layout it does not list, and it is answered the same
/// way, by asking the encoder.
///
/// The nearest rate it does list, and the higher of two equally near, since
/// coming down is the direction that costs the recording its top octave.
/// An encoder that lists nothing takes anything.
fn encoder_rate(codec: &ff::codec::codec::Codec, want: u32) -> u32 {
    let Ok(audio) = codec.audio() else { return want };
    let Some(listed) = audio.rates() else { return want };
    let listed: Vec<i32> = listed.filter(|&r| r > 0).collect();
    if listed.is_empty() || listed.contains(&(want as i32)) {
        return want;
    }
    listed
        .into_iter()
        .min_by_key(|&r| ((i64::from(r) - i64::from(want)).abs(), -i64::from(r)))
        .map_or(want, |r| r as u32)
}

/// The rate a track will actually be written at, given the rate that was
/// asked for and the codec it is going into.
///
/// The same answer [`open_encoder`] arrives at, asked before anything is
/// opened -- which is what lets the cut settle the output's rate once, say
/// so where it is not what was asked for, and then declare a stream that
/// matches the packets it will be fed. A codec with no encoder here answers
/// with the rate it was given; opening one is where that is complained
/// about, and this is not that place.
pub fn writable_rate(id: ff::codec::Id, want: u32) -> u32 {
    ff::encoder::find(id).map_or(want, |codec| encoder_rate(&codec, want))
}

/// Build the encoder that stands in for the recording's own.
///
/// Perceptual noise substitution is off: it is an MPEG-4 tool, and a stream
/// that announces itself as MPEG-2 AAC must not contain one. It is on by
/// default in this encoder, so a frame written into an ARIB recording without
/// this is a frame an MPEG-2 decoder is entitled to refuse.
///
/// `channels` is what comes *out*, which is the recording's own count except
/// where a downmix was asked for. `rate` is likewise the recording's own
/// unless a rate was asked for. `id` is what is written, which is the
/// recording's own codec except where the container has no box for it -- see
/// [`crate::cut`] and Blu-ray LPCM. `like` is the width the samples are
/// wanted at, which is where the encoder's format is picked from -- the
/// recording's own unless a width was asked for.
fn open_encoder(
    id: ff::codec::Id,
    like: ff::format::Sample,
    rate: u32,
    channels: u16,
    bit_rate: usize,
    quiet: bool,
) -> Result<ff::encoder::Audio> {
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no encoder for {id:?}"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
    // An encoder opened to be looked at rather than fed says a word about
    // itself on the way out -- "Qavg: nan", the average of the frames it was
    // never given -- and a probe that says it every time a control is
    // touched buries whatever the log was being kept for. The offset moves
    // what this one context logs a step up the scale, which leaves nothing
    // below `error` audible and `error` itself where it was.
    if quiet {
        unsafe {
            (*enc.as_mut_ptr()).log_level_offset = ff::ffi::AV_LOG_ERROR;
        }
    }
    let rate = encoder_rate(&codec, rate);
    enc.set_rate(rate as i32);
    enc.set_channel_layout(encoder_layout(&codec, channels).ok_or_else(|| {
        anyhow!("{id:?} is not written with {channels} channels, whichever way they are arranged")
    })?);
    enc.set_format(encoder_format(&codec, like));
    // Nothing for LPCM to spend: its size is the samples' own, and a figure
    // handed to it is a figure the encoder throws away.
    if bit_rate > 0 && !uncompressed(id) {
        enc.set_bit_rate(bit_rate);
    }
    enc.set_time_base(ff::Rational::new(1, rate as i32));
    let mut eopts = ff::Dictionary::new();
    if id == ff::codec::Id::AAC {
        eopts.set("aac_pns", "0");
        eopts.set("aac_ltp", "0");
        eopts.set("aac_pred", "0");
    }
    // libavcodec's DTS encoder is marked experimental and refuses to open
    // without being told that is understood. It is the only encoder here
    // that does, and what it produces is a DTS stream a receiver decodes;
    // "experimental" is libav's account of how much attention the encoder
    // has had, not of whether its output is DTS.
    if id == ff::codec::Id::DTS {
        eopts.set("strict", "experimental");
    }
    enc.open_as_with(codec, eopts).map_err(|e| anyhow!("cannot open audio encoder: {e}"))
}

/// Whether a track can be written this way at all.
///
/// Everything an encoder can refuse over is asked of the encoder itself
/// before it is opened -- the rate in [`encoder_rate`], the arrangement of
/// the channels in [`encoder_layout`], the width of the samples in
/// [`encoder_format`] -- and each of those either finds an answer the
/// encoder listed or has none to find. What is left after all three is the
/// bitrate, which no encoder lists at all and which DTS has a floor for:
/// its frame carries a fixed number of samples, a frame has to be long
/// enough to describe every channel in it, and below that length
/// `avcodec_open2` refuses. The floor moves with the channel count and with
/// the sample rate -- 5.1 at 48 kHz cannot be written under about
/// 670 kbit/s, and the same track at 32 kHz can -- so it is not a number to
/// keep a table of. It is asked for by opening an encoder and seeing.
///
/// Which is what this is for: the window greys out an answer the cut could
/// not have given rather than letting it be discovered when the cut is run,
/// and the cut raises a bitrate rather than failing on one. A rate the codec
/// does not speak is *not* refused here -- [`open_encoder`] takes it to the
/// nearest the encoder lists, as [`writable_rate`] says it will -- so that
/// question is asked separately, by comparing the two.
///
/// `bit_rate` of 0 asks about the encoder's own default, which is what a
/// codec with nothing to spend gets.
pub fn opens_at(id: ff::codec::Id, rate: u32, channels: u16, bit_rate: usize) -> bool {
    open_encoder(id, PLANAR_F32, rate, channels, bit_rate, true).is_ok()
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

/// A frame in the shape it is wanted in: the given sample format, with the
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
fn conform<'a>(
    resampler: &mut Option<ff::software::resampling::Context>,
    out: &'a mut ff::frame::Audio,
    frame: &'a ff::frame::Audio,
    layout: ff::channel_layout::ChannelLayout,
    format: ff::format::Sample,
) -> Result<&'a ff::frame::Audio> {
    // A decoder that names no layout at all is not a frame to rematrix --
    // swresample cannot be told where its channels are, and with the right
    // number of them there is nothing to move anyway.
    let same = frame.channel_layout() == layout
        || (frame.channel_layout().is_empty() && frame.channels() as i32 == layout.channels());
    if same && frame.format() == format {
        return Ok(frame);
    }
    let ctx = match resampler {
        Some(ctx) => ctx,
        None => resampler.insert(ff::software::resampling::Context::get(
            frame.format(),
            frame.channel_layout(),
            frame.rate(),
            format,
            layout,
            frame.rate(),
        )?),
    };
    *out = ff::frame::Audio::new(format, frame.samples().max(1), layout);
    ctx.run(frame, out)?;
    // Carried across rather than left to swresample: an encoder reads the
    // frame's own timestamp, and a frame that arrives without one is a frame
    // it has to guess at.
    out.set_pts(frame.pts());
    out.set_rate(frame.rate());
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
    /// The recording's own rate. The window a range is trimmed against is
    /// counted in the recording's samples, so this is what `take` measures
    /// with, whatever rate the track is written at.
    sample_rate: u32,
    /// The rate the track is written at, which is the encoder's own -- the
    /// recording's unless a rate was asked for, and then the nearest the
    /// codec can speak. Everything after the resampler is counted in these
    /// samples, `fed` included.
    out_rate: u32,
    /// Set only when those two differ: the samples kept at the recording's
    /// rate, on their way to the output's.
    ///
    /// Stateful, and deliberately one context for the whole track rather
    /// than one per frame like the two above. A resampler holds part of a
    /// sample back between calls -- the output grid does not land on the
    /// input grid -- and a context rebuilt per frame would drop that
    /// remainder every time, which is a click at every frame boundary
    /// instead of a track.
    resample: Option<ff::software::resampling::Context>,
    /// Samples waiting at the *output's* rate, which is what `drain` frames
    /// from. The same buffer as `pending` where nothing is resampled.
    ready: Pcm,
    /// Samples handed to the encoder so far -- the output timeline.
    fed: i64,
    /// Last source packet consumed, so a frame offered twice is ignored.
    last_pts: Option<i64>,
    /// Set when the frames are to leave here already framed, which is what
    /// keeps a re-encoded transport stream MPEG-2 AAC throughout.
    adts: Option<AdtsFormat>,
    /// What the encoder takes, and the conversion into it -- both idle when
    /// that is the planar float these buffers already hold, which is AAC's
    /// and AC-3's; LPCM, MP2 and DTS all want integers.
    enc_format: ff::format::Sample,
    to_encoder: Option<ff::software::resampling::Context>,
    feeding: ff::frame::Audio,
    /// Whether the encoder has a frame length of its own. LPCM has none: it
    /// writes back whatever it is handed, whole frames or not.
    fixed: bool,
    /// How the encoder wants its channels arranged, which is not always the
    /// arrangement libav hands back as the default for that many. See
    /// [`encoder_layout`].
    layout: ff::channel_layout::ChannelLayout,
}

impl Reencoder {
    /// `channels` is what the track is written with: `audio.channels` to
    /// follow the recording, fewer to downmix it. `rate` is likewise what it
    /// is written at: `audio.sample_rate` to follow the recording, anything
    /// else to resample it -- and the encoder has the last word, since not
    /// every codec speaks every rate. `like` is the sample format to ask the
    /// encoder for; `None` asks for the recording's own, which is right
    /// wherever the codec is also the recording's.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        params: ff::codec::Parameters,
        target: ff::codec::Id,
        like: Option<ff::format::Sample>,
        audio: &AudioInfo,
        channels: u16,
        rate: u32,
        bit_rate: usize,
        adts: Option<AdtsFormat>,
    ) -> Result<Self> {
        let decoder =
            ff::codec::context::Context::from_parameters(params.clone())?.decoder().audio()?;
        let like = like.unwrap_or_else(|| sample_format(&params));
        let encoder = open_encoder(target, like, rate, channels, bit_rate, false)?;
        let frame_size = frame_size_of(&encoder);
        let enc_format = encoder.format();
        let fixed = encoder.frame_size() > 0;
        let layout = encoder.channel_layout();
        // What the encoder settled on, which is the rate asked for wherever
        // the codec has it and the nearest it does have otherwise.
        let out_rate = encoder.rate();
        // Planar float in and planar float out: the resampler is here to
        // change the rate and nothing else, because the channels have
        // already been put right per frame on the way in and the encoder's
        // own format is put on at the other end.
        let resample = (out_rate != audio.sample_rate)
            .then(|| {
                ff::software::resampling::Context::get(
                    PLANAR_F32,
                    layout,
                    audio.sample_rate,
                    PLANAR_F32,
                    layout,
                    out_rate,
                )
            })
            .transpose()?;
        Ok(Self {
            decoder,
            encoder,
            pending: vec![Vec::new(); channels as usize],
            frame_size,
            channels: channels as usize,
            resampler: None,
            remixed: ff::frame::Audio::empty(),
            sample_rate: audio.sample_rate,
            out_rate,
            resample,
            ready: vec![Vec::new(); channels as usize],
            fed: 0,
            last_pts: None,
            adts,
            enc_format,
            to_encoder: None,
            feeding: ff::frame::Audio::empty(),
            fixed,
            layout,
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
            let src = conform(
                &mut self.resampler,
                &mut self.remixed,
                &frame,
                self.layout,
                PLANAR_F32,
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

    /// The rate the track is written at, which is not always the rate it was
    /// asked for: the encoder has the last word. What the output stream says
    /// about itself, and what its packets are timed against, both come from
    /// here.
    pub fn rate(&self) -> u32 {
        self.out_rate
    }

    /// Move what is waiting at the recording's rate over to the output's.
    ///
    /// A no-op but a move where the two rates agree, which is the ordinary
    /// case: `ready` is then simply where `pending` ends up.
    fn convert(&mut self) -> Result<()> {
        if self.pending[0].is_empty() {
            return Ok(());
        }
        let Some(ctx) = self.resample.as_mut() else {
            for ch in 0..self.channels {
                let taken = std::mem::take(&mut self.pending[ch]);
                self.ready[ch].extend(taken);
            }
            return Ok(());
        };
        let n = self.pending[0].len();
        let mut input = ff::frame::Audio::new(PLANAR_F32, n, self.layout);
        input.set_rate(self.sample_rate);
        for ch in 0..self.channels {
            let taken = std::mem::take(&mut self.pending[ch]);
            input.plane_mut::<f32>(ch)[..n].copy_from_slice(&taken);
        }
        // Room for everything the resampler is still holding as well as
        // everything it is about to be handed. Asked for less it keeps the
        // remainder rather than losing it, but keeping it is a buffer that
        // only grows, so the sum is done properly: the delay is asked for in
        // output samples, and the input is scaled into them.
        let held = unsafe { ff::ffi::swr_get_delay(ctx.as_mut_ptr(), i64::from(self.out_rate)) };
        let room = held.max(0) as usize
            + (n as u64 * u64::from(self.out_rate) / u64::from(self.sample_rate).max(1)) as usize
            + 32;
        let mut resampled = ff::frame::Audio::new(PLANAR_F32, room, self.layout);
        ctx.run(&input, &mut resampled)?;
        let made = resampled.samples();
        if made > 0 {
            take_samples(&resampled, self.channels, &mut self.ready, (0, made));
        }
        Ok(())
    }

    /// Push the resampler's own tail out, once nothing more is coming.
    ///
    /// A resampler runs a filter over its input and so is always a little
    /// behind it; what that filter still holds is the last few milliseconds
    /// of the track, and without this they are simply missing from the end.
    fn flush_resampler(&mut self) -> Result<()> {
        let Some(ctx) = self.resample.as_mut() else { return Ok(()) };
        loop {
            let held = unsafe { ff::ffi::swr_get_delay(ctx.as_mut_ptr(), i64::from(self.out_rate)) };
            if held <= 0 {
                return Ok(());
            }
            let mut resampled = ff::frame::Audio::new(PLANAR_F32, held as usize + 32, self.layout);
            ctx.flush(&mut resampled)?;
            let made = resampled.samples();
            if made == 0 {
                return Ok(());
            }
            take_samples(&resampled, self.channels, &mut self.ready, (0, made));
        }
    }

    /// Hand the encoder every full frame that has accumulated.
    pub fn drain(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        self.convert()?;
        while self.ready[0].len() >= self.frame_size {
            let mut frame = ff::frame::Audio::new(PLANAR_F32, self.frame_size, self.layout);
            frame.set_rate(self.out_rate);
            for ch in 0..self.channels {
                let taken: Vec<f32> = self.ready[ch].drain(..self.frame_size).collect();
                frame.plane_mut::<f32>(ch)[..self.frame_size].copy_from_slice(&taken);
            }
            frame.set_pts(Some(self.fed));
            self.fed += self.frame_size as i64;
            let feed = conform(
                &mut self.to_encoder,
                &mut self.feeding,
                &frame,
                self.layout,
                self.enc_format,
            )?;
            self.encoder.send_frame(feed)?;
            self.collect(out)?;
        }
        Ok(())
    }

    /// Finish: pad the tail to a whole frame and flush the encoder.
    pub fn finish(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        // Everything still on the input side, and then the resampler's own
        // tail after it -- in that order, or the last few milliseconds of
        // the track arrive in front of the samples they follow.
        self.convert()?;
        self.flush_resampler()?;
        self.drain(out)?;
        if !self.ready[0].is_empty() {
            if self.fixed {
                let short = self.frame_size - self.ready[0].len();
                for ch in 0..self.channels {
                    self.ready[ch].extend(std::iter::repeat_n(0.0, short));
                }
            } else {
                // Nothing to pad up to. The tail goes out at its own length,
                // so the track ends where the recording did rather than up to
                // a frame of silence later.
                self.frame_size = self.ready[0].len();
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

/// The codecs a cut carries through byte for byte rather than re-encoding a
/// frame of.
///
/// DTS-HD and TrueHD are lossless extensions wrapped around a lossy core, and
/// libavformat's encoders for them write something else: `dca` writes the
/// core alone, `truehd` and `mlp` are experimental. A frame from either in
/// the middle of a lossless track is a hole in it, not a trim. So a boundary
/// on one of these lands on a whole frame -- about 20 ms out -- and the
/// recording's own bytes reach the output unaltered, which for lossless sound
/// is the answer that matters.
pub fn carried_whole(id: ff::codec::Id) -> bool {
    matches!(id, ff::codec::Id::DTS | ff::codec::Id::TRUEHD | ff::codec::Id::MLP)
}

/// How wide a track's samples are once they are written as linear PCM.
///
/// 24 bits only where the recording has more than 16 bits in it, which is
/// two questions and not one. The codec has to be one that carries samples
/// rather than a description of them -- everything lossy decodes to a 32 bit
/// float, and a float is a wide number holding a narrow recording, so a
/// broadcast written out at 24 bits would be half again the size and not one
/// sample better. And the recording itself has to be the wide kind: Blu-ray
/// LPCM is 16, 20 or 24 bit, and libavformat hands the widest two over as 32
/// bit samples and the narrowest as 16.
///
/// This is the one place that decides it. The window multiplies it out to
/// say what an uncompressed track will cost, and the cut asks the encoder
/// for exactly this width -- which matters beyond the size, because the
/// Blu-ray LPCM encoder frames 16 bit sound 240 samples at a time and 24 bit
/// sound 360.
pub fn pcm_bits(params: &ff::codec::Parameters) -> u8 {
    use ff::codec::Id::*;
    let carries_samples = matches!(
        params.id(),
        PCM_BLURAY
            | PCM_DVD
            | PCM_S16BE
            | PCM_S16LE
            | PCM_S24BE
            | PCM_S24LE
            | PCM_S32BE
            | PCM_S32LE
            | PCM_F32BE
            | PCM_F32LE
            | FLAC
            | ALAC
            | TRUEHD
            | MLP
            | DTS
    );
    let wide = !matches!(
        sample_format(params),
        ff::format::Sample::I16(_) | ff::format::Sample::U8(_)
    );
    if carries_samples && wide {
        24
    } else {
        16
    }
}

/// Whether a codec writes the samples down rather than describing them.
///
/// What this decides is whether a bit rate means anything: LPCM's size is
/// the arithmetic of the recording -- channels times bits times the sample
/// rate -- and the encoder ignores the figure it is handed. Everything else
/// here spends what it is given.
pub fn uncompressed(id: ff::codec::Id) -> bool {
    use ff::codec::Id::*;
    matches!(
        id,
        PCM_BLURAY
            | PCM_DVD
            | PCM_S16BE
            | PCM_S16LE
            | PCM_S24BE
            | PCM_S24LE
            | PCM_S32BE
            | PCM_S32LE
            | PCM_F32BE
            | PCM_F32LE
            | PCM_U8
            | PCM_S8
    )
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
    let mut ictx = ff::format::input(&src.input.url)?;
    let stream = ictx
        .stream(audio.stream_index)
        .ok_or_else(|| anyhow!("audio stream {} vanished", audio.stream_index))?;
    let params = stream.parameters();
    let rate = audio.sample_rate as f64;
    let adts = adts.map(|f| f.as_version(aac));

    // Three ways a track can turn out not to be one whose frames this
    // rewrites, and all three end the same way: say so and copy. Smart
    // rendering is an improvement on copying, not a condition of it, and
    // none of these is worth failing a cut over.
    //
    // The first is that re-encoding the codec at all would be the wrong
    // thing -- lossless sound, below.
    //
    // The second is that there is no encoder for it, or none that will open.
    //
    // The third is that a frame this encoder produces cannot stand in for one
    // of the recording's, because the two are not framed alike, and the
    // encoder's delay is what says whether they are. AAC's is a whole frame,
    // so its packets land on the same grid the recording's frames sit on.
    // AC-3's is 256 samples and MP2's is 481, which puts every packet off
    // that grid -- their frames cover the wrong samples to be swapped in,
    // however well they are encoded.
    if carried_whole(params.id()) {
        eprintln!(
            "note: {:?} is lossless sound and is carried through byte for byte -- no encoder \
             here writes it without losing what makes it lossless, so a re-encoded frame \
             would be worse than the one it replaced. The cut's boundaries land on whole \
             frames of it.",
            params.id(),
        );
        return Ok(out);
    }
    let probe = match open_encoder(
        params.id(),
        sample_format(&params),
        audio.sample_rate,
        audio.channels,
        bit_rate,
        true,
    ) {
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
    // What comes out of the decoder is planar float for everything a
    // broadcast carries, and 16 or 32 bit integers for Blu-ray LPCM. What is
    // kept here is float either way, because that is what the mask and the
    // fade are written in.
    let mut resampler = None;
    let mut floated = ff::frame::Audio::empty();

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
            // These frames go back among the recording's own, so nothing
            // here may move a channel: the layout asked for is the layout
            // that arrived, and the only conversion is one of format. A
            // frame that names no layout falls back to the default for its
            // count, which is what swresample can be told about.
            let want = if frame.channel_layout().channels() == channels as i32 {
                frame.channel_layout()
            } else {
                ff::channel_layout::ChannelLayout::default(channels as i32)
            };
            let floats = conform(&mut resampler, &mut floated, &frame, want, PLANAR_F32)?;
            let n = floats.samples();
            let mut pcm: Pcm = vec![Vec::with_capacity(n); channels];
            take_samples(floats, channels, &mut pcm, (0, n));
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

    let mut encoder =
        open_encoder(params.id(), sample_format(params), audio.sample_rate, audio.channels, bit_rate, false)?;
    // A frame written here has to cover exactly the samples the frame it
    // replaces covered. Most encoders have a frame length of their own, and
    // it has to be the recording's; LPCM has none -- it writes back whatever
    // it is handed -- so it fits any recording's.
    let fixed = encoder.frame_size() as usize;
    if fixed != 0 && fixed != size {
        // The encoder frames the audio differently from the recording, so a
        // frame it produces cannot stand in for one of the recording's.
        return Ok(());
    }
    let channels = audio.channels as usize;
    let enc_format = encoder.format();
    let enc_layout = encoder.channel_layout();
    let mut to_encoder = None;
    let mut feeding = ff::frame::Audio::empty();

    let mut fed = 0i64;
    let mut got: Vec<(usize, Vec<u8>)> = Vec::new();
    for f in run {
        let mut frame = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            size,
            enc_layout,
        );
        frame.set_rate(audio.sample_rate);
        for ch in 0..channels {
            let mut samples = f.pcm[ch].clone();
            mask(&mut samples, f.first, window);
            frame.plane_mut::<f32>(ch)[..size].copy_from_slice(&samples);
        }
        frame.set_pts(Some(fed));
        fed += size as i64;
        let feed = conform(&mut to_encoder, &mut feeding, &frame, enc_layout, enc_format)?;
        encoder.send_frame(feed)?;
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
