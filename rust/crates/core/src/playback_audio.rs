//! Live audio for the preview player, straight to the sound card.
//!
//! The video half of playback (`preview::play_from`) paces itself against a
//! wall clock, and so does this: both are handed the one instant playback
//! began at, and every sample carries the moment on that clock it is due to
//! be heard. See [`SLACK`] for why that has to be said rather than left to
//! the card.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

/// How many channels the sound is heard in, where that is fewer than the
/// recording has: 0 for the recording's own.
///
/// What a row is written as, heard before it is written. A 5.1 film that the
/// list has been told to fold into stereo is played folded and metered on
/// two bars, so what the cut editor says about the sound is what the output
/// will hold rather than what the recording held. The fold is swresample's
/// with libav's own coefficients, the same one the cutter applies -- see
/// `conform` in [`crate::audio`].
///
/// Shared like [`Volume`], and for the same reason: the answer is changed in
/// the list window while the editor is playing, and the decode loop reads it
/// afresh on every frame.
#[derive(Clone, Default)]
pub struct Fold(Arc<AtomicU32>);

impl Fold {
    pub fn set(&self, channels: u16) {
        self.0.store(u32::from(channels), Ordering::Relaxed);
    }

    pub fn get(&self) -> u16 {
        self.0.load(Ordering::Relaxed) as u16
    }

    /// How many channels a track of `own` is heard in.
    fn of(&self, own: u16) -> u16 {
        match self.get() {
            n if n > 0 && n < own => n,
            _ => own,
        }
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
///
/// It is also where the samples are due. The ring is a run of these, one per
/// decoded frame, and `due` is when the first frame still in it is to be
/// heard, in seconds after playback began -- the clock the pictures are
/// paced on. See [`SLACK`].
#[derive(Clone, Copy)]
struct Bite {
    frames: usize,
    channels: usize,
    peaks: [f32; METERED],
    due: f64,
}

impl Levels {
    /// Take a bite the card has just played some of.
    ///
    /// A frame's peak counts in full as soon as any of it has been heard --
    /// it is a peak, not an average, and a decoded frame is tens of
    /// milliseconds, which is shorter than the meter's own step.
    ///
    /// Measured *before* the volume is applied, because the levels are
    /// applied to the samples on their way out of the ring and these were
    /// measured on the way in: the meter says what the recording holds, and
    /// turning the monitoring down does not make a programme quieter.
    fn eat(&self, bite: &Bite) {
        let channels = bite.channels.clamp(1, METERED);
        self.0.channels.store(channels as u32, Ordering::Relaxed);
        for ch in 0..channels {
            self.0.peak[ch].fetch_max(bite.peaks[ch].to_bits(), Ordering::Relaxed);
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
///
/// The two are in step: a bite of `frames` frames stands for that many frames
/// of `samples`, in the same order.
#[derive(Default)]
struct Feed {
    samples: VecDeque<f32>,
    bites: VecDeque<Bite>,
    /// What [`settle`] has had to do, for the log. Here rather than beside
    /// the ring because it is written under the same lock.
    tally: Tally,
}

impl Feed {
    /// When the frame at the head of the ring is due, if there is one.
    fn due(&self) -> Option<f64> {
        self.bites.front().map(|b| b.due)
    }

    /// Move the bites on by `frames`, handing each one to `heard` as any of
    /// it goes. The samples are the caller's to take.
    fn pass(&mut self, mut frames: usize, rate: u32, mut heard: impl FnMut(&Bite)) {
        while frames > 0 {
            let Some(front) = self.bites.front_mut() else { break };
            let took = front.frames.min(frames);
            heard(front);
            front.frames -= took;
            front.due += took as f64 / rate as f64;
            frames -= took;
            if front.frames == 0 {
                self.bites.pop_front();
            }
        }
    }

    /// Throw `frames` away unheard.
    fn skip(&mut self, frames: usize, rate: u32, channels: usize) {
        let n = (frames * channels).min(self.samples.len());
        self.samples.drain(..n);
        self.pass(frames, rate, |_| {});
    }
}

/// How far the sound may stray from the pictures before it is put back.
///
/// **Left to the card, the sound started late and stayed late.** The card
/// was opened and started before the recording was, and played silence
/// through the device's own opening, the demuxer's, the seek and the first
/// decode. Every sample after that was heard that much after the picture it
/// belonged to. Measured from the moment 再生 was pressed to the first
/// sample reaching the card: 0.55 to 0.8 s on a PipeWire desktop, a quarter
/// of it opening the device. The pictures are on the wall clock and drop
/// what is late; the sound had nothing to measure lateness against. A
/// buffer the decode fell behind on added its silence to the delay, and a
/// gap in the recording's own audio took time out of it.
///
/// So the clock does not start until the sound is ready (see [`Start`]), and
/// each buffer compares the head of the ring with the moment it will leave
/// the speaker: now, plus what the host says it still holds. Anything further
/// out than this is set right in one go -- late samples are thrown away,
/// early ones wait behind silence. Only past this, because every correction
/// is heard: 40 ms is under what anyone notices of sound against a face
/// (ITU-R BT.1359 puts that at 45 ms early), and wide enough that the card
/// and the wall clock drifting apart comes to one correction every several
/// minutes rather than one a buffer.
///
/// **What the host holds is not always said.** ALSA's PulseAudio plugin
/// reports a delay of zero, on PipeWire and on PulseAudio alike, so there the
/// sound is heard later than this reckons by the server's own buffer -- 70 ms
/// on the PipeWire desktop above. That is about what the window takes to put
/// a picture on screen once it has it, so the two are left to cancel.
const SLACK: f64 = 0.040;

/// What [`settle`] has had to do over one run, for the log.
#[derive(Default)]
struct Tally {
    /// Frames thrown away because they were late, and how many times.
    dropped: usize,
    drops: usize,
    /// Frames of silence waited because the sound was early.
    waited: usize,
    waits: usize,
}

impl Tally {
    fn say(&self, rate: u32) {
        if self.drops + self.waits > 0 {
            let ms = |n: usize| n as f64 * 1000.0 / rate as f64;
            eprintln!(
                "audio sync: {:.0} ms dropped in {}, {:.0} ms waited in {}",
                ms(self.dropped),
                self.drops,
                ms(self.waited),
                self.waits,
            );
        }
    }
}

/// Bring the head of the ring to `now`, the moment the next buffer will be
/// heard, and say how many frames of silence it should open with.
///
/// Late samples are dropped here, as many bites deep as it takes -- a gap in
/// the recording's timestamps is a bite whose `due` jumps ahead, and that one
/// is then early. Early ones are waited for, up to the whole buffer. See
/// [`SLACK`].
fn settle(q: &mut Feed, now: f64, rate: u32, channels: usize, frames: usize) -> usize {
    let mut dropped = 0;
    while let Some(due) = q.due() {
        let late = now - due;
        if late <= SLACK {
            break;
        }
        let front = q.bites.front().map_or(0, |b| b.frames);
        let n = ((late * rate as f64).round() as usize).clamp(1, front.max(1));
        q.skip(n, rate, channels);
        dropped += n;
    }
    if dropped > 0 {
        q.tally.drops += 1;
        q.tally.dropped += dropped;
    }
    match q.due() {
        Some(due) if due - now > SLACK => {
            let lead = (((due - now) * rate as f64).round() as usize).min(frames);
            q.tally.waits += 1;
            q.tally.waited += lead;
            lead
        }
        _ => 0,
    }
}

/// The instant playback begins at: the one the pictures are paced from and
/// the sound is due against.
///
/// Set by whichever side is ready last to be able to say so -- the sound,
/// when its first samples are in the ring. Taken any earlier and those
/// samples are already late when they get there, and the first half second
/// of every playback is thrown away to catch up; a preview is started from
/// just before a cut more often than not, and that half second is the part
/// being listened to.
///
/// The pictures wait for it, but not forever: see [`Start::wait`].
#[derive(Clone, Default)]
pub struct Start(Arc<Gate>);

#[derive(Default)]
struct Gate {
    at: OnceLock<Instant>,
    lock: Mutex<()>,
    ready: Condvar,
}

impl Start {
    /// It begins now, unless it already has.
    pub fn mark(&self) -> Instant {
        if let Some(at) = self.get() {
            return at;
        }
        let at = *self.0.at.get_or_init(Instant::now);
        let _held = self.0.lock.lock().unwrap();
        self.0.ready.notify_all();
        at
    }

    /// When it began, if it has. Never waits, so the output callback may ask.
    fn get(&self) -> Option<Instant> {
        self.0.at.get().copied()
    }

    /// When it begins, waiting at most `longest` for the other side to say --
    /// after which it begins now. A sound that never comes (a card that will
    /// not open, a stretch with no audio in it) does not hold the pictures
    /// up for longer than that.
    pub fn wait(&self, longest: Duration) -> Instant {
        let held = self.0.lock.lock().unwrap();
        let _held = self
            .0
            .ready
            .wait_timeout_while(held, longest, |_| self.0.at.get().is_none())
            .unwrap();
        *self.0.at.get_or_init(Instant::now)
    }
}

/// Marks a [`Start`] when dropped, however the function holding it returns.
struct Marks<'a>(&'a Start);

impl Drop for Marks<'_> {
    fn drop(&mut self) {
        self.0.mark();
    }
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
    start: &Start,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let feed = ring.clone();
    let level = volume.clone();
    let meter = levels.clone();
    let start = start.clone();
    let channels = config.channels.max(1) as usize;
    let rate = config.sample_rate.0;
    let mut was = level.get();
    device.build_output_stream(
        config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let want = level.get();
            let frames = (data.len() / channels).max(1);
            let mut q = feed.lock().unwrap();
            // Kept to the clock once there is one. Until then nothing has
            // been handed over, and there is nothing to keep in time.
            let lead = match start.get() {
                Some(began) => {
                    // When this buffer will be heard: now, and behind what
                    // the host says it still holds. See [`SLACK`].
                    let stamp = info.timestamp();
                    let held = stamp.playback.duration_since(&stamp.callback).unwrap_or_default();
                    let now = (began.elapsed() + held).as_secs_f64();
                    settle(&mut q, now, rate, channels, frames)
                }
                None => 0,
            };
            // What was actually there to play. A buffer the decode did not
            // keep up with is padded with silence below, and silence nobody
            // recorded is not something to meter.
            let had = (q.samples.len() / channels).min(frames - lead);
            for (i, out) in data.iter_mut().enumerate() {
                // Per frame, not per sample: the two channels of one instant
                // are one instant, and they are scaled by the one number.
                let frame = i / channels;
                let at = frame as f32 / frames as f32;
                let s = if frame < lead { 0.0 } else { q.samples.pop_front().unwrap_or(0.0) };
                *out = s * (was + (want - was) * at);
            }
            // Under the same lock, and before it is let go: the peaks behind
            // the samples just played are what the meter is about.
            q.pass(had, rate, |bite| meter.eat(bite));
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
    start: &Start,
    why: &mut Option<cpal::BuildStreamError>,
) -> Option<Output> {
    for config in candidates(device, want) {
        match build_stream(device, &config, ring, volume, levels, start) {
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
    start: &Start,
) -> Result<Output> {
    let host = cpal::default_host();
    let mut why = None;

    if let Some(device) = host.default_output_device() {
        if let Some(out) = open_on(&device, want, ring, volume, levels, start, &mut why) {
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
        if let Some(out) = open_on(&device, want, ring, volume, levels, start, &mut why) {
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
/// `from` is heard at `start`, the instant the caller's pictures are paced
/// from, and the rest in step with it. This marks it, once the first of the
/// sound is ready to go -- or on the way out, if it never is. See [`Start`].
///
/// Runs entirely on the calling thread. The `cpal::Stream` it opens is not
/// guaranteed `Send` on every backend, so nothing here may cross a thread
/// boundary once created -- the caller is expected to give this its own
/// thread and simply join it.
#[allow(clippy::too_many_arguments)]
pub fn play_audio(
    src: &Source,
    ranges: &[(f64, f64)],
    from: f64,
    start: &Start,
    volume: &Volume,
    levels: &Levels,
    fold: &Fold,
    stop: impl Fn() -> bool,
) -> Result<()> {
    play_audio_across(
        &[Heard {
            src,
            ranges: ranges.to_vec(),
            from,
            // The cut editor plays the recording as it is. What a fade at a
            // seam does to it is a question with more in it than one pair of
            // numbers -- see `Heard::fades`.
            fades: (0.0, 0.0),
        }],
        start,
        volume,
        levels,
        fold,
        stop,
    )
}

/// One recording's share of a run of playback.
///
/// `ranges` are that recording's own source ranges and `from` the instant
/// within them to begin at, which is what the picture side is already given.
pub struct Heard<'a> {
    pub src: &'a Source,
    pub ranges: Vec<(f64, f64)>,
    pub from: f64,
    /// How long the sound takes to come back at the head of each of those
    /// ranges and to leave at its tail, in seconds. `(0.0, 0.0)` for neither,
    /// which is what an ordinary playback is.
    ///
    /// **What this is for is the seam preview.** A fade at a join is settled
    /// in a window that exists to be judged by ear, and a fade that is only
    /// in the written file cannot be judged at all -- the same argument that
    /// put the pictures of a crossing on screen there rather than six rows
    /// of settings. The curve is [`crate::audio::fade_shape`], which is the
    /// curve the cut writes, so what is heard here is what is written.
    ///
    /// One pair for the whole part rather than one per range: the preview
    /// hands over one range per clip, and the ends of a *cut's* ranges are a
    /// question with more in it -- the two ends of the output fade at
    /// neither, and the pair differs per seam. See `cut::fade_lengths`.
    pub fades: (f64, f64),
}

/// Play several recordings' sound, one after another, through one output.
///
/// What a seam preview needs: the clip before the join, then the clip after
/// it, heard as the joined file will be heard. **Nothing is mixed.** A
/// transition shows two pictures at once but the sound of a join is the one
/// clip and then the other, which is what the cutter writes -- see
/// `crate::cut` -- so this plays them in turn.
///
/// One device for the lot, opened once against the first recording that has
/// any sound. A device opened and closed per clip clicks at the join and
/// costs a fraction of a second's silence there, which on a two second
/// preview is the part being listened to. A second recording at another rate
/// or another channel count is resampled into the open device's format, the
/// way a recording that does not match the card already is.
pub fn play_audio_across(
    parts: &[Heard],
    start: &Start,
    volume: &Volume,
    levels: &Levels,
    fold: &Fold,
    stop: impl Fn() -> bool,
) -> Result<()> {
    // Whatever way this ends, the pictures are not left waiting on it.
    let _mark = Marks(start);
    // The format is the first one with sound in it. A silent clip contributes
    // nothing to play and nothing to open a device for.
    let Some(first) = parts.iter().find(|p| p.src.audio.is_some()) else {
        return Ok(());
    };
    let audio = first.src.audio.clone().expect("just found");
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
    //
    // At the count it is heard in, so a fold asked for before playing opens
    // the card at that count. One asked for part-way through is folded all
    // the same and spread back over the card's channels.
    let want = (audio.sample_rate, fold.of(audio.channels));
    let ring = Arc::new(Mutex::new(Feed::default()));
    let out = open_output(want, &ring, volume, levels, start)?;
    let (rate, channels) = (out.sample_rate, out.channels);
    if (rate, channels) != want {
        eprintln!(
            "audio output: {}Hz/{}ch source played at {rate}Hz/{channels}ch",
            want.0, want.1
        );
    }
    let cap = capacity(rate, channels);
    out.stream
        .play()
        .map_err(|e| anyhow!("cannot start audio output: {e}"))?;

    // Where on the output's clock each part begins: the one after the other,
    // as the pictures are laid out.
    let mut base = 0.0;
    for part in parts {
        if stop() {
            break;
        }
        feed_from(part, rate, channels, cap, &ring, fold, &mut base, start, &stop)?;
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
    ring.lock().unwrap().tally.say(rate);
    Ok(())
}

/// Decode one recording's ranges into an output that is already open.
///
/// The body [`play_audio`] used to be. It is a function of its own so that
/// [`play_audio_across`] can run it twice over one device.
#[allow(clippy::too_many_arguments)]
fn feed_from(
    part: &Heard,
    rate: u32,
    channels: u16,
    cap: usize,
    ring: &Arc<Mutex<Feed>>,
    fold: &Fold,
    base: &mut f64,
    clock: &Start,
    stop: &impl Fn() -> bool,
) -> Result<()> {
    let layout = ff::channel_layout::ChannelLayout::default(channels as i32);
    let Heard { src, ranges, from, fades } = part;
    let (from, fades) = (*from, *fades);
    let Some(audio) = src.audio.clone() else {
        return Ok(());
    };
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
    let mut folder: Option<ff::software::resampling::Context> = None;
    let mut folded = ff::frame::Audio::empty();

    'ranges: for &(a, b) in ranges.iter() {
        if stop() {
            break;
        }
        let start = a.max(from);
        if start >= b - 1e-9 {
            continue;
        }
        // Counted the way the pictures count it (see `play` in the GUI):
        // this range takes up `b - start` of the output however much of it
        // the file turns out to hold.
        let here = *base;
        *base += b - start;
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
                // Folded first, where the row is written in fewer channels:
                // both what is heard and what is metered are the output's.
                let heard_as = fold_frame(&mut folder, &mut folded, &frame, fold)?;
                // Measured before the resampling, so the meter answers for
                // the recording's channels rather than the card's.
                let mut peaks = [0f32; METERED];
                let heard = frame_peaks(heard_as, &mut peaks);
                let data = resample(&mut resampler, &mut resampled, heard_as, rate, layout)?;
                let n = data.len() / channels as usize;
                if let Some((lo, hi)) = frame_window(n, rate, t, start, b) {
                    let mut samples =
                        data[lo * channels as usize..hi * channels as usize].to_vec();
                    let at = t + lo as f64 / rate as f64;
                    ride_fade(&mut samples, channels, at, rate, (a, b), fades);
                    let due = here + (at - start);
                    let bite = Bite { frames: hi - lo, channels: heard, peaks, due };
                    if !push(ring, cap, &stop, samples, bite) {
                        break 'ranges;
                    }
                    // The first of it is in, and the clock can start.
                    clock.mark();
                }
            }
            if past_end {
                break;
            }
        }
    }
    Ok(())
}

/// A decoded frame in the channels it is heard in. See [`Fold`].
///
/// The frame itself where there is nothing to fold -- the ordinary case --
/// and where its decoder named no layout, since swresample cannot be told
/// where the channels of such a frame are. The sample format is kept: the
/// conversion to the card's is [`resample`]'s.
fn fold_frame<'a>(
    folder: &mut Option<ff::software::resampling::Context>,
    folded: &'a mut ff::frame::Audio,
    frame: &'a ff::frame::Audio,
    fold: &Fold,
) -> Result<&'a ff::frame::Audio> {
    let own = frame.channels();
    let to = fold.of(own);
    if to >= own || frame.channel_layout().is_empty() {
        return Ok(frame);
    }
    let layout = ff::channel_layout::ChannelLayout::default(i32::from(to));
    crate::audio::conform(folder, folded, frame, layout, frame.format())
}

/// Ride the fades over one frame's worth of interleaved samples.
///
/// `at` is where the first of them sits on the recording's own clock, and
/// `range` is the stretch being played: the head fade runs from its start and
/// the tail fade back from its end. The two are taken together and the
/// quieter wins, so a range shorter than its two fades has one answer
/// wherever they overlap -- the same rule [`crate::audio`] applies to the
/// frames a cut rewrites.
fn ride_fade(
    samples: &mut [f32],
    channels: u16,
    at: f64,
    rate: u32,
    range: (f64, f64),
    fades: (f64, f64),
) {
    if fades.0 <= 0.0 && fades.1 <= 0.0 {
        return;
    }
    let channels = channels.max(1) as usize;
    let step = 1.0 / rate as f64;
    for (k, frame) in samples.chunks_mut(channels).enumerate() {
        let t = at + k as f64 * step;
        let gain = crate::audio::fade_shape(t - range.0, fades.0)
            .min(crate::audio::fade_shape(range.1 - t, fades.1));
        if gain < 1.0 {
            for s in frame.iter_mut() {
                *s *= gain;
            }
        }
    }
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
///
/// In the channels the row is heard in, where it is folded: the resampling
/// below lays every frame out at `channels`, and that is a fold whenever it
/// is fewer than the frame's. See [`Fold`].
pub fn peaks_at(src: &Source, time: f64, window: f64, fold: &Fold) -> Result<Vec<f32>> {
    let Some(audio) = src.audio.as_ref() else {
        return Ok(Vec::new());
    };
    crate::init()?;
    let channels = (fold.of(audio.channels) as usize).clamp(1, METERED);
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
            // Only the samples inside the window, not every sample of every
            // frame that overlaps it. A frame of AAC is 21 milliseconds and a
            // picture is 33, so two or three frames reach into one picture's
            // window and each of them brings sound from either side of it.
            // The meter beside the picture read that sound as the picture's
            // own, which is what a mark at a junction was being judged
            // against: half a frame of level that belongs to the frame before
            // and half that belongs to the frame after.
            let rate = audio.sample_rate.max(1) as f64;
            let from = (((time - t) * rate).ceil().max(0.0) as usize).min(data.len() / channels);
            let to = (((end - t) * rate).ceil().max(0.0) as usize).min(data.len() / channels);
            for (i, &s) in data[from * channels..to * channels].iter().enumerate() {
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

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// A ring of stereo bites, each `frames` long and due at the given times.
    fn ring(bites: &[(f64, usize)]) -> Feed {
        let mut q = Feed::default();
        for &(due, frames) in bites {
            q.samples.extend(std::iter::repeat_n(1.0, frames * 2));
            q.bites.push_back(Bite { frames, channels: 2, peaks: [0.0; METERED], due });
        }
        q
    }

    #[test]
    fn a_late_start_is_dropped_up_to_now() {
        // The first sound arrived 300 ms after playback began.
        let mut q = ring(&[(0.0, 1024), (1024.0 / 48e3, 24_000)]);
        let lead = settle(&mut q, 0.3, RATE, 2, 960);
        assert_eq!(lead, 0);
        assert!((q.due().unwrap() - 0.3).abs() < 1.0 / 48e3);
        assert_eq!(q.samples.len(), q.bites.iter().map(|b| b.frames * 2).sum::<usize>());
        assert_eq!(q.tally.dropped, 14_400);
    }

    #[test]
    fn inside_the_slack_nothing_moves() {
        let mut q = ring(&[(0.0, 1024)]);
        assert_eq!(settle(&mut q, 0.030, RATE, 2, 960), 0);
        assert_eq!(settle(&mut q, -0.030, RATE, 2, 960), 0);
        assert_eq!(q.bites.front().unwrap().frames, 1024);
    }

    #[test]
    fn a_gap_in_the_recording_is_waited_out() {
        // 100 ms missing between two frames: the second is early by that much.
        let mut q = ring(&[(0.2, 480)]);
        assert_eq!(settle(&mut q, 0.1, RATE, 2, 960), 960);
        assert_eq!(settle(&mut q, 0.18, RATE, 2, 960), 0);
    }

    #[test]
    fn the_pictures_wait_for_the_sound_but_not_forever() {
        let start = Start::default();
        let other = start.clone();
        let marked = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.mark()
        });
        let waited = start.wait(Duration::from_secs(5));
        assert_eq!(waited, marked.join().unwrap());

        let alone = Start::default();
        let asked = Instant::now();
        alone.wait(Duration::from_millis(20));
        assert!(asked.elapsed() >= Duration::from_millis(20));
        assert_eq!(alone.mark(), alone.get().unwrap());
    }
}
