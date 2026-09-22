//! Live audio for the preview player, straight to the sound card.
//!
//! The video half of playback (`preview::play_from`) paces itself against a
//! wall clock. Audio does not need that: once the samples are in the ring
//! buffer, the card's own clock plays them at the right speed on its own.
//! The two clocks are independent and can drift apart over a long stretch --
//! accepted for the same reason `audio.rs` accepts 10.7ms of splice error:
//! this is for checking that a cut sounds right, not for watching the
//! programme through.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

use crate::Source;

/// About a second of interleaved samples -- enough that the video decode
/// sharing the same CPU cannot starve the card between pictures.
fn capacity(sample_rate: u32, channels: u16) -> usize {
    sample_rate as usize * channels as usize
}

/// How loud the card is fed, as a plain multiplier on the samples.
///
/// Shared rather than given once, because it is changed while playback is
/// running: the thread below is inside a decode loop for as long as the
/// sound lasts, and the hand on the slider is in another window entirely.
/// Both ends hold the same one, and the output callback reads it afresh on
/// every buffer.
///
/// The bits of an `f32` in an `AtomicU32`, there being no atomic float. It
/// is read on the audio callback, where a lock is the one thing that must
/// not happen: a callback that waits is a callback that misses its deadline,
/// and a missed deadline is heard.
///
/// A multiplier and not a percentage: what a slider's position should mean
/// in loudness is a question about the person looking at it, and it is
/// answered where the slider is. See `gain` in the editor window.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn new(gain: f32) -> Self {
        let v = Self(Arc::new(AtomicU32::new(0)));
        v.set(gain);
        v
    }

    /// Set it. Anything that is not a number -- which is what an empty box
    /// on the way through JSON arrives as -- leaves it where it was.
    pub fn set(&self, gain: f32) {
        if gain.is_nan() {
            return;
        }
        self.0.store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::new(1.0)
    }
}

/// As many channels as the meter has room for. Everything broadcast is one,
/// two or six; a recording with more of them is metered on the first eight
/// and the rest are not drawn. See [`Levels`].
pub const METERED: usize = 8;

/// How loud each channel has been since the meter last looked.
///
/// The editor draws a level meter beside the picture, and what it draws has
/// to be *what is being played*: the loudest the recording reached over the
/// stretch of it the sound card has just been handed. Neither end of the
/// playback path can say that on its own.
///
/// * The **decode** end knows the recording's own channels, but it runs up to
///   a second ahead of the card -- that second is the ring buffer -- so a
///   meter fed from there would show a level before it was heard.
/// * The **output** end knows what has actually been played, but by then the
///   channels are the card's: WASAPI mixes every application at one format,
///   so a 5.1 broadcast on a stereo output arrives here as two channels and
///   a meter fed from there would drop from six bars to two the moment
///   playback started.
///
/// So the two are put together. The decode end measures each frame as the
/// recording holds it and leaves the answer beside the samples it produced
/// ([`Bite`]); the output end, having handed those samples over, says how
/// many frames' worth went and the peaks behind them are what the meter
/// takes. The bars are the recording's channels, and they are the levels of
/// the moment coming out of the speakers.
///
/// Read destructively: asking empties it. A meter is not a running total but
/// the peak over the moment it is drawing, and the window asks about twenty
/// times a second.
///
/// Atomics rather than a lock, for the reason [`Volume`] gives: the output
/// callback must never wait. `fetch_max` works on the bits of an `f32`
/// directly here because every one of these is a magnitude -- for two
/// non-negative floats, the one with the larger IEEE bit pattern is the
/// larger number.
#[derive(Clone, Default)]
pub struct Levels(Arc<Meter>);

#[derive(Default)]
struct Meter {
    /// How many of the peaks below are being written, which is how many bars
    /// the meter draws. The recording's own channel count; see above.
    channels: AtomicU32,
    peak: [AtomicU32; METERED],
}

/// One decoded frame, as the meter sees it: how loud each of the recording's
/// channels was in it, and how many frames of output it turned into.
///
/// The count is in output frames because that is what the card consumes, and
/// what the card has consumed is the only measure of what has been heard.
#[derive(Clone, Copy)]
struct Bite {
    frames: usize,
    channels: usize,
    peaks: [f32; METERED],
}

impl Levels {
    /// Take what the card has just played: `frames` output frames' worth of
    /// the peaks waiting in `bites`.
    ///
    /// A frame's peak counts in full as soon as any of it has been heard --
    /// it is a peak, not an average, and a decoded frame is tens of
    /// milliseconds, which is shorter than the meter's own step.
    ///
    /// Measured *before* the volume is applied, because the levels are
    /// applied to the samples on their way out of the ring and these were
    /// measured on the way in: the meter says what the recording holds, and
    /// turning the monitoring down does not make a programme quieter.
    fn eat(&self, bites: &mut VecDeque<Bite>, mut frames: usize) {
        while frames > 0 {
            let Some(front) = bites.front_mut() else { break };
            let took = front.frames.min(frames);
            let channels = front.channels.clamp(1, METERED);
            self.0.channels.store(channels as u32, Ordering::Relaxed);
            for ch in 0..channels {
                self.0.peak[ch].fetch_max(front.peaks[ch].to_bits(), Ordering::Relaxed);
            }
            front.frames -= took;
            frames -= took;
            if front.frames == 0 {
                bites.pop_front();
            }
        }
    }

    /// What each channel reached since this was last asked, and back to
    /// nothing for the next moment.
    pub fn take(&self) -> Vec<f32> {
        let channels = (self.0.channels.load(Ordering::Relaxed) as usize).min(METERED);
        (0..channels)
            .map(|ch| f32::from_bits(self.0.peak[ch].swap(0, Ordering::Relaxed)))
            .collect()
    }

    /// Nothing is playing, so nothing is being heard. Said at both ends of a
    /// run: a meter left holding the last buffer of the last playback is a
    /// meter reporting a sound that stopped.
    pub fn clear(&self) {
        self.0.channels.store(0, Ordering::Relaxed);
        for p in &self.0.peak {
            p.store(0, Ordering::Relaxed);
        }
    }
}

/// What is waiting for the card: the samples themselves, and the peaks that
/// go with them. One lock covers both, because the output callback takes
/// them together and a callback that takes two locks is a callback with two
/// ways to be late.
#[derive(Default)]
struct Feed {
    samples: VecDeque<f32>,
    bites: VecDeque<Bite>,
}

/// Block until there is room, or `stop` says to give up. Never blocks forever
/// on a chunk bigger than `cap`: one decoded frame is at most a few thousand
/// samples, far under a second's worth.
fn push(
    ring: &Mutex<Feed>,
    cap: usize,
    stop: &impl Fn() -> bool,
    samples: Vec<f32>,
    bite: Bite,
) -> bool {
    loop {
        if stop() {
            return false;
        }
        {
            let mut q = ring.lock().unwrap();
            if q.samples.len() + samples.len() <= cap || q.samples.is_empty() {
                q.samples.extend(samples);
                q.bites.push_back(bite);
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The slice of a decoded frame, in samples-per-channel, that falls inside
/// `[start, end)` of the source clock. `t` is the frame's own presentation
/// time, and `rate` the rate the samples are in by the time they get here --
/// resampling changes how many there are, not how long they last.
fn frame_window(n: usize, rate: u32, t: f64, start: f64, end: f64) -> Option<(usize, usize)> {
    let dur = n as f64 / rate as f64;
    if t + dur <= start || t >= end {
        return None;
    }
    let lo = (((start - t).max(0.0)) * rate as f64).round() as usize;
    let hi = ((end - t).min(dur) * rate as f64).round().min(n as f64) as usize;
    if hi <= lo {
        None
    } else {
        Some((lo, hi))
    }
}

/// The samples of a packed (interleaved) float frame, as `ffmpeg-next`'s own
/// `plane()` cannot be trusted with: it hands back a slice of `samples()`
/// elements regardless of packing, which for anything but mono is short by a
/// factor of the channel count. The frame is always produced by `resample`
/// below, so its shape is under this module's own control.
fn packed_f32(frame: &ff::frame::Audio, channels: usize) -> &[f32] {
    let n = frame.samples() * channels;
    unsafe { std::slice::from_raw_parts((*frame.as_ptr()).data[0] as *const f32, n) }
}

/// The loudest each of a decoded frame's own channels gets, as a fraction of
/// full scale, and how many channels that was.
///
/// Measured on the frame as the recording holds it -- before the resampling
/// that puts it into whatever shape the card takes -- because that is what
/// the meter is about: a 5.1 broadcast has six bars whether or not the
/// machine can play six channels.
///
/// `ffmpeg-next`'s own `plane()` cannot be used for the same reason
/// [`packed_f32`] exists: it hands back a slice of `samples()` elements
/// whatever the packing, which for an interleaved frame is short by the
/// channel count. The lengths are worked out here instead.
fn frame_peaks(frame: &ff::frame::Audio, out: &mut [f32; METERED]) -> usize {
    use ff::format::sample::Sample;
    // What the samples are laid out in, and what is metered: the first eight
    // of them. The two are not the same number on a recording with more
    // channels than the meter has bars, and an interleaved frame has to be
    // strided by the layout's count or every bar reads the wrong channel.
    let laid = (frame.channels() as usize).max(1);
    let channels = laid.min(METERED);
    out.fill(0.0);
    // A frame can claim more planes than an `AVFrame` has pointers to hold:
    // eight is all there are, and a corrupt header in an off-air recording is
    // enough to ask for a ninth. See `cm::frame_peak`, which learned it the
    // hard way.
    let planar = frame.is_planar();
    let planes = if planar { frame.planes().min(channels) } else { 1 };
    for p in 0..planes {
        let n = if planar {
            frame.samples()
        } else {
            frame.samples() * laid
        };
        // The channel a sample belongs to: the plane itself where the frame
        // is planar, and its place in the interleaving where it is not.
        let of = |i: usize| if planar { p } else { i % laid };
        unsafe {
            let data = (*frame.as_ptr()).data[p];
            if data.is_null() {
                continue;
            }
            match frame.format() {
                Sample::F32(_) => {
                    scan(std::slice::from_raw_parts(data as *const f32, n), of, out, |v| v.abs())
                }
                Sample::F64(_) => {
                    scan(std::slice::from_raw_parts(data as *const f64, n), of, out, |v| {
                        v.abs() as f32
                    })
                }
                Sample::I16(_) => {
                    scan(std::slice::from_raw_parts(data as *const i16, n), of, out, |v| {
                        v.unsigned_abs() as f32 / 32768.0
                    })
                }
                Sample::I32(_) => {
                    scan(std::slice::from_raw_parts(data as *const i32, n), of, out, |v| {
                        v.unsigned_abs() as f32 / 2147483648.0
                    })
                }
                // A format nothing here decodes to. Metered as full scale
                // rather than as silence: a meter that reads zero says the
                // recording is silent, which is a worse lie than one that
                // reads loud.
                _ => out[..channels].fill(1.0),
            }
        }
    }
    channels
}

/// One plane's samples, into the peaks they belong to.
fn scan<T: Copy>(
    data: &[T],
    of: impl Fn(usize) -> usize,
    out: &mut [f32; METERED],
    mag: impl Fn(T) -> f32,
) {
    for (i, &v) in data.iter().enumerate() {
        let ch = of(i);
        let m = mag(v);
        if ch < METERED && m > out[ch] {
            out[ch] = m;
        }
    }
}

/// What the ring buffer holds, and what the output stream is asked for.
const PACKED_F32: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Packed);

/// Convert a decoded frame to interleaved f32 in the format the sound card
/// took. Two conversions in one, and swresample does both.
///
/// The sample format has to be converted whatever the card takes: broadcast
/// audio decodes to whatever its codec's native format is (`audio.rs`'s
/// `Reencoder`, which feeds an AAC-only path, can assume float; a general
/// playback path cannot -- MPEG-1 Layer II, common on Japanese terrestrial
/// broadcasts, does not decode to float here). Letting swresample do it is
/// simpler and safer than hand-rolling one conversion per sample format.
///
/// The rate and the channel layout only have to be converted when the card
/// would not take the source's own, which is the ordinary case on Windows and
/// never happens on Linux -- see `candidates`.
///
/// **The shape it converts from is the frame's, not the track's, and it is
/// asked again of every frame.** A broadcast recording changes shape
/// part-way through: a tuner told to start early opens on the end of the
/// programme before, and a bulletin read in mono ahead of a documentary in
/// stereo is an ordinary evening's television. swresample will not take a
/// frame that is not the shape its context was built for, and the refusal
/// arrived here as an error that ended the playback thread -- so the sound
/// stopped at the instant the programme began, with the picture playing on.
/// A fresh context at the change costs one allocation and carries on. See
/// [`crate::audio::settled_shape`], which is the same fact answered for the
/// side that writes.
fn resample<'a>(
    resampler: &mut Option<ff::software::resampling::Context>,
    out: &'a mut ff::frame::Audio,
    frame: &ff::frame::Audio,
    rate: u32,
    layout: ff::channel_layout::ChannelLayout,
) -> Result<&'a [f32]> {
    let arriving = ff::software::resampling::context::Definition {
        format: frame.format(),
        channel_layout: frame.channel_layout(),
        rate: frame.rate(),
    };
    if resampler
        .as_ref()
        .is_some_and(|ctx| *ctx.input() != arriving)
    {
        *resampler = None;
    }
    let ctx = match resampler {
        Some(ctx) => ctx,
        None => {
            let ctx = ff::software::resampling::Context::get(
                frame.format(),
                frame.channel_layout(),
                frame.rate(),
                PACKED_F32,
                layout,
                rate,
            )?;
            resampler.insert(ctx)
        }
    };
    // Size the output frame here rather than leave it to `run`, which asks
    // for room for as many samples as went in -- short by the ratio of the
    // rates whenever the card runs faster than the source. Nothing is lost
    // when it is short, swresample keeping what will not fit, but what it
    // keeps never comes back out: the backlog simply grows for as long as
    // playback lasts.
    let room = (frame.samples() * rate as usize).div_ceil(frame.rate().max(1) as usize) + 32;
    *out = ff::frame::Audio::new(PACKED_F32, room, layout);
    ctx.run(frame, out)?;
    Ok(packed_f32(out, out.channels() as usize))
}

/// Devices to fall back on, best first, when the host default will not open.
/// All of them are ALSA names: this is the Linux problem below, and on hosts
/// that have no such PCMs the list simply finds nothing.
const FALLBACKS: [&str; 3] = ["pipewire", "pulse", "sysdefault"];

/// Hand the ring buffer to the sound card, padding with silence whenever the
/// decode has not kept up, and at whatever [`Volume`] says.
///
/// The level is walked from the one the last buffer ended at to the one
/// standing now, across the buffer, rather than applied whole. A gain that
/// steps is a step in the waveform, and a step in a waveform is a click --
/// which is exactly what ミュート would otherwise be, in the middle of the
/// loudest moment of a programme. Twenty milliseconds is far too short to
/// hear as a fade and quite long enough to not hear as a click.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: &Arc<Mutex<Feed>>,
    volume: &Volume,
    levels: &Levels,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let feed = ring.clone();
    let level = volume.clone();
    let meter = levels.clone();
    let channels = config.channels.max(1) as usize;
    let mut was = level.get();
    device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let want = level.get();
            let frames = (data.len() / channels).max(1);
            let mut q = feed.lock().unwrap();
            // What was actually there to play. A buffer the decode did not
            // keep up with is padded with silence below, and silence nobody
            // recorded is not something to meter.
            let had = q.samples.len().min(data.len()) / channels;
            for (i, out) in data.iter_mut().enumerate() {
                // Per frame, not per sample: the two channels of one instant
                // are one instant, and they are scaled by the one number.
                let at = (i / channels) as f32 / frames as f32;
                *out = q.samples.pop_front().unwrap_or(0.0) * (was + (want - was) * at);
            }
            // Under the same lock, and before it is let go: the peaks behind
            // the samples just played are what the meter is about.
            meter.eat(&mut q.bites, had);
            drop(q);
            was = want;
        },
        |e| eprintln!("audio output error: {e}"),
        None,
    )
}

/// ~20ms of frames. See `play_audio` for why the period is chosen here rather
/// than left to the host to answer.
fn fixed_period(rate: u32) -> cpal::BufferSize {
    cpal::BufferSize::Fixed((rate / 50).max(256))
}

/// A stream the sound card took, and the format it took it in -- which is not
/// necessarily the source's.
struct Output {
    stream: cpal::Stream,
    sample_rate: u32,
    channels: u16,
}

/// The configurations worth offering `device`, best first.
///
/// The source's own rate and layout come first, playing them untouched being
/// one conversion fewer. Linux takes them: cpal's default output is ALSA's
/// `default`, which is a `plug` chain, and `plug` converts whatever the card
/// itself cannot do.
///
/// **Windows does not.** WASAPI's shared mode mixes every application at one
/// format, and an `IAudioClient` will only be initialised in that format --
/// `IsFormatSupported` answers anything else with `S_FALSE` and the nearest
/// match, which cpal reports as `StreamConfigNotSupported`. That format is
/// whatever the machine's sound settings say, so a 48 kHz broadcast is silent
/// on an output set to 44.1 kHz, and a 5.1 broadcast is silent on any stereo
/// output. Asking the device what it mixes at and meeting it there is the
/// whole of the fix; `resample` is then given one more thing to convert.
///
/// The last candidates give up the fixed period. That is an ALSA workaround
/// (see `play_audio`), and `snd_pcm_hw_params_set_buffer_size` refuses an
/// exact size the card cannot take -- 44.1 kHz lands on 882 frames, which a
/// power-of-two card will not have.
fn candidates(device: &cpal::Device, want: (u32, u16)) -> Vec<cpal::StreamConfig> {
    let mut formats = vec![want];
    if let Ok(mixed) = device.default_output_config() {
        let mixed = (mixed.sample_rate().0, mixed.channels());
        if mixed != want {
            formats.push(mixed);
        }
    }
    let sized = |&(rate, channels): &(u32, u16), buffer_size| cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(rate),
        buffer_size,
    };
    let fixed = formats.iter().map(|f| sized(f, fixed_period(f.0)));
    let default = formats.iter().map(|f| sized(f, cpal::BufferSize::Default));
    fixed.chain(default).collect()
}

/// The first of those `device` will take, if any.
fn open_on(
    device: &cpal::Device,
    want: (u32, u16),
    ring: &Arc<Mutex<Feed>>,
    volume: &Volume,
    levels: &Levels,
    why: &mut Option<cpal::BuildStreamError>,
) -> Option<Output> {
    for config in candidates(device, want) {
        match build_stream(device, &config, ring, volume, levels) {
            Ok(stream) => {
                return Some(Output {
                    stream,
                    sample_rate: config.sample_rate.0,
                    channels: config.channels,
                })
            }
            Err(e) => *why = Some(e),
        }
    }
    None
}

/// Open an output stream on the first device that will take one.
///
/// The host default is the right answer nearly everywhere, but not always.
/// On Linux it is whatever the machine's `alsa.conf` chain resolves
/// `default` to, and a PipeWire desktop that never installed `pipewire-alsa`
/// leaves that chain pointing at the bare sound card -- which PipeWire itself
/// holds open. The card comes back `EBUSY`, cpal turns `EBUSY` into
/// `DeviceNotAvailable`, and the user is told their sound card "has been
/// unplugged" while it is sitting there playing everything else on the
/// desktop. Asking the sound server for its own PCM by name gets us the same
/// device every other application on that desktop is already using.
fn open_output(
    want: (u32, u16),
    ring: &Arc<Mutex<Feed>>,
    volume: &Volume,
    levels: &Levels,
) -> Result<Output> {
    let host = cpal::default_host();
    let mut why = None;

    if let Some(device) = host.default_output_device() {
        if let Some(out) = open_on(&device, want, ring, volume, levels, &mut why) {
            return Ok(out);
        }
    }

    // Only now, having failed, is it worth enumerating: cpal opens every PCM
    // the ALSA hints mention in order to list it, which is not something to
    // do on the path that works.
    let mut named: Vec<cpal::Device> = host
        .output_devices()
        .map(|ds| {
            ds.filter(|d| matches!(d.name(), Ok(n) if FALLBACKS.contains(&n.as_str())))
                .collect()
        })
        .unwrap_or_default();
    named.sort_by_key(|d| {
        let name = d.name().unwrap_or_default();
        FALLBACKS
            .iter()
            .position(|n| *n == name)
            .unwrap_or(usize::MAX)
    });
    for device in named {
        if let Some(out) = open_on(&device, want, ring, volume, levels, &mut why) {
            let name = device.name().unwrap_or_default();
            eprintln!("audio output: default would not open, playing through {name}");
            return Ok(out);
        }
    }

    // Name what was tried: on the failures this guards against, the error
    // cpal hands back describes neither the device nor the format it refused.
    let (rate, channels) = want;
    let tried = FALLBACKS.join(", ");
    Err(match why {
        Some(e) => {
            anyhow!(
                "cannot open audio output for {rate}Hz/{channels}ch (tried default, {tried}): {e}"
            )
        }
        None => anyhow!("no audio output device"),
    })
}

/// Play the audio under `ranges` (the edited timeline's source ranges, same
/// as the video gets), starting at `from`, until `stop()` answers true or the
/// ranges run out, at whatever `volume` says at each moment.
///
/// Runs entirely on the calling thread. The `cpal::Stream` it opens is not
/// guaranteed `Send` on every backend, so nothing here may cross a thread
/// boundary once created -- the caller is expected to give this its own
/// thread and simply join it.
pub fn play_audio(
    src: &Source,
    ranges: &[(f64, f64)],
    from: f64,
    volume: &Volume,
    levels: &Levels,
    stop: impl Fn() -> bool,
) -> Result<()> {
    let Some(audio) = src.audio.clone() else {
        return Ok(());
    };
    crate::init()?;

    // `BufferSize::Default` would leave the period to cpal, which asks the
    // device for one. On Linux that question goes through the ALSA-over-
    // PulseAudio plugin most desktops route "default" through, and the answer
    // it gives sends cpal's poll loop into a busy spin -- the output thread
    // pins a core at 100% CPU forever, just to copy a few hundred samples a
    // callback. That CPU is stolen from the video decode this is playing
    // alongside, which is what makes playback heavy, and a thread spinning
    // instead of sleeping between callbacks is what makes the audio itself
    // glitch. Picking a period ourselves sidesteps the plugin's answer
    // entirely; ~20ms is short enough nobody previewing a cut would notice
    // the added latency. `candidates` heads its ladder with that.
    let want = (audio.sample_rate, audio.channels);
    let ring = Arc::new(Mutex::new(Feed::default()));
    let out = open_output(want, &ring, volume, levels)?;
    let (rate, channels) = (out.sample_rate, out.channels);
    if (rate, channels) != want {
        eprintln!(
            "audio output: {}Hz/{}ch source played at {rate}Hz/{channels}ch",
            want.0, want.1
        );
    }
    let layout = ff::channel_layout::ChannelLayout::default(channels as i32);
    let cap = capacity(rate, channels);
    out.stream
        .play()
        .map_err(|e| anyhow!("cannot start audio output: {e}"))?;

    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = audio.stream_index;
    let in_tb = audio.time_base;
    // Only this track is read, so the pictures go by unassembled -- which is
    // most of a recording, and the seeks below still land without them. See
    // [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .audio()?;
    let mut resampler: Option<ff::software::resampling::Context> = None;
    let mut resampled = ff::frame::Audio::empty();

    'ranges: for &(a, b) in ranges {
        if stop() {
            break;
        }
        let start = a.max(from);
        if start >= b - 1e-9 {
            continue;
        }
        // Seek a little early rather than exactly on the target: the
        // container's seek is only approximate, and landing late would lose
        // the beginning of the range outright, where landing early just
        // means a moment more gets decoded and trimmed away below.
        let landing = (start - src.seek_margin).max(0.0);
        let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
        let _ = ictx.seek(target, ..target);
        decoder.flush();

        let mut frame = ff::frame::Audio::empty();
        for (stream, packet) in ictx.packets() {
            if stop() {
                break 'ranges;
            }
            if stream.index() != idx {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            let mut past_end = false;
            while decoder.receive_frame(&mut frame).is_ok() {
                let Some(pts) = frame.pts() else { continue };
                let t = pts as f64 * in_tb - src.start_time;
                if t >= b {
                    past_end = true;
                    break;
                }
                // Measured before the resampling, so the meter answers for
                // the recording's channels rather than the card's.
                let mut peaks = [0f32; METERED];
                let heard = frame_peaks(&frame, &mut peaks);
                let data = resample(&mut resampler, &mut resampled, &frame, rate, layout)?;
                let n = data.len() / channels as usize;
                if let Some((lo, hi)) = frame_window(n, rate, t, start, b) {
                    let samples = data[lo * channels as usize..hi * channels as usize].to_vec();
                    let bite = Bite { frames: hi - lo, channels: heard, peaks };
                    if !push(&ring, cap, &stop, samples, bite) {
                        break 'ranges;
                    }
                }
            }
            if past_end {
                break;
            }
        }
    }

    // Let the tail play out rather than cutting it off the instant decoding
    // catches up with the ranges.
    while !stop() {
        if ring.lock().unwrap().samples.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Nothing is coming out of the card any more, whichever way this ended.
    // The meter is left holding whatever the last buffer reached otherwise,
    // which reads as a sound that is still playing.
    levels.clear();
    Ok(())
}

/// How loud each channel is at `time`, over a window `window` seconds long.
///
/// What the meter shows while nothing is playing. The level under the
/// playhead is as much a part of judging where to cut as the picture is --
/// the quiet between a programme and a break is what a commercial boundary
/// sounds like -- and stepping a frame at a time with a meter stuck at zero
/// says nothing at all.
///
/// The recording's own channels rather than the sound card's, because
/// nothing here goes near a card: a 5.1 broadcast has six bars whatever the
/// machine would play it through.
///
/// One seek and a short decode. The caller is expected to hold off until the
/// playhead has settled -- this is a read of the file, and at a frame a
/// keystroke it would be one read per keystroke.
pub fn peaks_at(src: &Source, time: f64, window: f64) -> Result<Vec<f32>> {
    let Some(audio) = src.audio.as_ref() else {
        return Ok(Vec::new());
    };
    crate::init()?;
    let channels = (audio.channels as usize).clamp(1, METERED);
    let mut peaks = vec![0f32; channels];

    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = audio.stream_index;
    // Only this track. See [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .audio()?;

    // Early, as `play_audio` seeks: landing past the moment asked about would
    // meter the wrong instant, and landing early only costs a few frames of
    // decoding that are then passed over.
    let landing = (time - src.seek_margin).max(0.0);
    let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
    let _ = ictx.seek(target, ..target);
    decoder.flush();

    let layout = ff::channel_layout::ChannelLayout::default(channels as i32);
    let mut resampler: Option<ff::software::resampling::Context> = None;
    let mut resampled = ff::frame::Audio::empty();
    let mut frame = ff::frame::Audio::empty();
    let end = time + window.max(0.0);

    'packets: for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * audio.time_base - src.start_time;
            let dur = frame.samples() as f64 / audio.sample_rate.max(1) as f64;
            if t >= end {
                break 'packets;
            }
            // Wholly before the window: seeking lands early on purpose.
            if t + dur <= time {
                continue;
            }
            let data = resample(&mut resampler, &mut resampled, &frame, audio.sample_rate, layout)?;
            for (i, &s) in data.iter().enumerate() {
                let ch = i % channels;
                let m = s.abs();
                if m > peaks[ch] {
                    peaks[ch] = m;
                }
            }
        }
    }
    Ok(peaks)
}
