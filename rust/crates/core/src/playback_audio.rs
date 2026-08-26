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

/// Block until there is room, or `stop` says to give up. Never blocks forever
/// on a chunk bigger than `cap`: one decoded frame is at most a few thousand
/// samples, far under a second's worth.
fn push(ring: &Mutex<VecDeque<f32>>, cap: usize, stop: &impl Fn() -> bool, samples: Vec<f32>) -> bool {
    loop {
        if stop() {
            return false;
        }
        {
            let mut q = ring.lock().unwrap();
            if q.len() + samples.len() <= cap || q.is_empty() {
                q.extend(samples);
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
fn resample<'a>(
    resampler: &mut Option<ff::software::resampling::Context>,
    out: &'a mut ff::frame::Audio,
    frame: &ff::frame::Audio,
    rate: u32,
    layout: ff::channel_layout::ChannelLayout,
) -> Result<&'a [f32]> {
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
/// decode has not kept up.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: &Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let feed = ring.clone();
    device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut q = feed.lock().unwrap();
            for out in data.iter_mut() {
                *out = q.pop_front().unwrap_or(0.0);
            }
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
    ring: &Arc<Mutex<VecDeque<f32>>>,
    why: &mut Option<cpal::BuildStreamError>,
) -> Option<Output> {
    for config in candidates(device, want) {
        match build_stream(device, &config, ring) {
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
fn open_output(want: (u32, u16), ring: &Arc<Mutex<VecDeque<f32>>>) -> Result<Output> {
    let host = cpal::default_host();
    let mut why = None;

    if let Some(device) = host.default_output_device() {
        if let Some(out) = open_on(&device, want, ring, &mut why) {
            return Ok(out);
        }
    }

    // Only now, having failed, is it worth enumerating: cpal opens every PCM
    // the ALSA hints mention in order to list it, which is not something to
    // do on the path that works.
    let mut named: Vec<cpal::Device> = host
        .output_devices()
        .map(|ds| ds.filter(|d| matches!(d.name(), Ok(n) if FALLBACKS.contains(&n.as_str()))).collect())
        .unwrap_or_default();
    named.sort_by_key(|d| {
        let name = d.name().unwrap_or_default();
        FALLBACKS.iter().position(|n| *n == name).unwrap_or(usize::MAX)
    });
    for device in named {
        if let Some(out) = open_on(&device, want, ring, &mut why) {
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
            anyhow!("cannot open audio output for {rate}Hz/{channels}ch (tried default, {tried}): {e}")
        }
        None => anyhow!("no audio output device"),
    })
}

/// Play the audio under `ranges` (the edited timeline's source ranges, same
/// as the video gets), starting at `from`, until `stop()` answers true or the
/// ranges run out.
///
/// Runs entirely on the calling thread. The `cpal::Stream` it opens is not
/// guaranteed `Send` on every backend, so nothing here may cross a thread
/// boundary once created -- the caller is expected to give this its own
/// thread and simply join it.
pub fn play_audio(
    src: &Source,
    ranges: &[(f64, f64)],
    from: f64,
    stop: impl Fn() -> bool,
) -> Result<()> {
    let Some(audio) = src.audio.clone() else { return Ok(()) };
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
    let ring = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let out = open_output(want, &ring)?;
    let (rate, channels) = (out.sample_rate, out.channels);
    if (rate, channels) != want {
        eprintln!("audio output: {}Hz/{}ch source played at {rate}Hz/{channels}ch", want.0, want.1);
    }
    let layout = ff::channel_layout::ChannelLayout::default(channels as i32);
    let cap = capacity(rate, channels);
    out.stream.play().map_err(|e| anyhow!("cannot start audio output: {e}"))?;

    let mut ictx = ff::format::input(&src.path)?;
    let idx = audio.stream_index;
    let in_tb = audio.time_base;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("stream {idx} vanished"))?.parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().audio()?;
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
                let data = resample(&mut resampler, &mut resampled, &frame, rate, layout)?;
                let n = data.len() / channels as usize;
                if let Some((lo, hi)) = frame_window(n, rate, t, start, b) {
                    let samples = data[lo * channels as usize..hi * channels as usize].to_vec();
                    if !push(&ring, cap, &stop, samples) {
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
        if ring.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
