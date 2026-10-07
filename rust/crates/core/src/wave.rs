//! The sound's outline over the whole recording, for drawing under the
//! timeline.
//!
//! One read of the first audio track, kept as two levels per bucket of
//! [`STEP`] seconds: the loudest sample in it and its RMS. Both are stored in
//! dB as a byte each (see [`Wave`]), which is what the drawing wants and is a
//! quarter of a megabyte for two hours.
//!
//! Not the silence detection, though it reads the same samples. That one
//! answers where it is quiet at one level and for one minimum length, and is
//! asked again whenever either changes; this is the sound itself, at every
//! level at once, and is read once per recording. The quiet stretches a break
//! is laid on show up in it before anything has been detected, which is what
//! it is for.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::input::ReadPackets;
use crate::Source;

/// Seconds per bucket. Twenty a second: a junction's silence is a third of a
/// second to a second and a half, so it is always at least one whole bucket,
/// and a two-hour recording is 144,000 of them.
pub const STEP: f64 = 0.05;

/// The quietest level a byte can say, in dB below full scale. Byte 0 is this
/// or quieter, byte 255 is full scale.
pub const FLOOR_DB: f64 = 80.0;

/// The outline: bucket `i` covers `[i * step, (i + 1) * step)` seconds of
/// the recording, rebased the way every other time here is.
///
/// A bucket the sound never reached -- a gap in the track, or the stretch
/// before its first frame -- reads as silence, which is what it sounds like.
#[derive(Debug, Clone, Default)]
pub struct Wave {
    pub step: f64,
    pub peak: Vec<u8>,
    pub rms: Vec<u8>,
}

/// A linear level, 0..=1, as one of [`Wave`]'s bytes.
pub fn level_byte(v: f64) -> u8 {
    if v <= 0.0 {
        return 0;
    }
    let db = 20.0 * v.log10();
    (((db + FLOOR_DB) / FLOOR_DB) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The track the outline is of: the first, which is the one the editor plays
/// while every track is kept (and the one a player starts on), rather than
/// the main one -- libav's widest, which need not be first, and then the
/// editor drew one track and played another.
fn outlined(src: &Source) -> Option<&crate::AudioInfo> {
    src.audios.first().or(src.audio.as_ref())
}

/// Read the first audio track and outline it, as a pass of its own.
///
/// For a recording whose outline was not made on the way past: one opened
/// before this was written, or one whose pictures were already kept. Where
/// the pictures are read, [`WaveBuilder`] rides along with them instead and
/// this read never happens; see [`crate::thumbs::build_with_sound`].
pub fn read_wave(src: &Source, mut progress: Option<Box<dyn FnMut(f64) + Send>>) -> Result<Wave> {
    crate::init()?;
    let audio = outlined(src).ok_or_else(|| anyhow!("{} has no audio", src.path))?;
    let mut ictx = crate::input::demux(&src.input.url)?;
    // The sound and nothing else: see [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[audio.stream_index]);
    let mut builder = WaveBuilder::new(src, &ictx)?;
    let mut told = -1.0;
    for (stream, packet) in ictx.read_packets() {
        if stream.index() != audio.stream_index {
            continue;
        }
        builder.feed(&packet);
        if let Some(f) = progress.as_mut() {
            let done = (builder.reached / src.duration.max(1e-9)).clamp(0.0, 1.0);
            if done - told >= 0.02 {
                told = done;
                f(done);
            }
        }
    }
    if let Some(f) = progress.as_mut() {
        f(1.0);
    }
    Ok(builder.finish())
}

/// The outline, made a packet at a time by whichever read is going past the
/// sound anyway.
///
/// The pictures pass reads every packet of the recording to find the entry
/// pictures, and the sound is a few tens of megabytes of the gigabytes it
/// goes past; decoding it there costs a second or two of one core and saves
/// [`read_wave`]'s whole read, which over a share is a minute a half hour.
pub struct WaveBuilder {
    /// The stream it wants. The read has to keep it switched on.
    pub stream_index: usize,
    decoder: ff::decoder::Audio,
    time_base: f64,
    start_time: f64,
    declared: f64,
    /// The most buckets the outline is given: the recording's length and a
    /// little over. Nothing past it is drawn, and a timestamp that jumps
    /// days or years ahead -- a crafted file's, or a clock that restarted --
    /// would otherwise ask for a vector that size.
    most: usize,
    frame: ff::frame::Audio,
    levels: Vec<f32>,
    peak: Vec<f32>,
    energy: Vec<(f64, u32)>,
    /// How far into the recording the sound has been read, in seconds.
    pub reached: f64,
}

impl WaveBuilder {
    /// For `src`'s first audio track, out of the demuxer the read has open.
    pub fn new(src: &Source, ictx: &crate::input::Demux) -> Result<Self> {
        let audio = outlined(src).ok_or_else(|| anyhow!("{} has no audio", src.path))?;
        let params = ictx
            .stream(audio.stream_index)
            .ok_or_else(|| anyhow!("audio stream vanished"))?
            .parameters();
        let decoder = ff::codec::context::Context::from_parameters(params)?
            .decoder()
            .audio()?;
        Ok(Self {
            stream_index: audio.stream_index,
            decoder,
            time_base: audio.time_base,
            start_time: src.start_time,
            declared: audio.sample_rate.max(1) as f64,
            most: most_buckets(src.duration),
            frame: ff::frame::Audio::empty(),
            levels: Vec::new(),
            peak: Vec::new(),
            energy: Vec::new(),
            reached: 0.0,
        })
    }

    /// One packet of the stream. One the decoder will not take is passed
    /// over: a few dropped milliseconds of an outline are not worth failing
    /// the read that carries it.
    pub fn feed(&mut self, packet: &ff::Packet) {
        if self.decoder.send_packet(packet).is_err() {
            return;
        }
        self.drain();
    }

    fn drain(&mut self) {
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            let Some(pts) = self.frame.pts() else { continue };
            let t = pts as f64 * self.time_base - self.start_time;
            // The frame's own rate, as [`crate::cm`] counts at and for its
            // reason: HE-AAC declares half the rate it decodes to.
            let rate = match self.frame.rate() {
                0 => self.declared,
                own => f64::from(own),
            };
            if !sample_levels(&self.frame, &mut self.levels) {
                continue;
            }
            for (n, &v) in self.levels.iter().enumerate() {
                let at = t + n as f64 / rate;
                if at < 0.0 {
                    continue;
                }
                let b = (at / STEP) as usize;
                if b >= self.most {
                    continue;
                }
                if b >= self.peak.len() {
                    self.peak.resize(b + 1, 0.0);
                    self.energy.resize(b + 1, (0.0, 0));
                }
                self.peak[b] = self.peak[b].max(v);
                let e = &mut self.energy[b];
                e.0 += f64::from(v) * f64::from(v);
                e.1 += 1;
            }
            self.reached = self.reached.max(t);
        }
    }

    /// What it came to, once the read is over.
    pub fn finish(mut self) -> Wave {
        let _ = self.decoder.send_eof();
        self.drain();
        Wave {
            step: STEP,
            peak: self.peak.iter().map(|&v| level_byte(f64::from(v))).collect(),
            rms: self
                .energy
                .iter()
                .map(|&(sum, n)| level_byte(if n == 0 { 0.0 } else { (sum / f64::from(n)).sqrt() }))
                .collect(),
        }
    }
}

/// How many buckets a recording `duration` seconds long can need: up to a
/// minute past its end, and a day where its length is not known.
fn most_buckets(duration: f64) -> usize {
    let span = if duration.is_finite() && duration > 0.0 { duration.min(86_400.0 * 7.0) + 60.0 } else { 86_400.0 };
    (span / STEP).ceil() as usize
}

/// The loudest channel of each sample of `frame`, 0..=1, into `out`.
///
/// False for a layout this does not know how to read, which leaves that frame
/// out rather than drawing it as anything. The channels are read as
/// [`crate::cm`] reads them, through `extended_data` past the eighth.
fn sample_levels(frame: &ff::frame::Audio, out: &mut Vec<f32>) -> bool {
    use ff::format::sample::Sample;
    out.clear();
    let samples = frame.samples();
    if frame.planes() == 0 || samples == 0 {
        return false;
    }
    out.resize(samples, 0.0);
    if frame.is_planar() {
        for p in 0..frame.planes() {
            let mut fold = |levels: &mut dyn Iterator<Item = f32>| {
                for (o, v) in out.iter_mut().zip(levels) {
                    *o = o.max(v);
                }
            };
            match frame.format() {
                Sample::F32(_) => fold(&mut crate::cm::channel::<f32>(frame, p).iter().map(|v| v.abs())),
                Sample::F64(_) => {
                    fold(&mut crate::cm::channel::<f64>(frame, p).iter().map(|v| v.abs() as f32))
                }
                Sample::I16(_) => fold(
                    &mut crate::cm::channel::<i16>(frame, p)
                        .iter()
                        .map(|v| v.unsigned_abs() as f32 / 32768.0),
                ),
                Sample::I32(_) => fold(
                    &mut crate::cm::channel::<i32>(frame, p)
                        .iter()
                        .map(|v| (v.unsigned_abs() as f64 / 2147483648.0) as f32),
                ),
                Sample::U8(_) => fold(
                    &mut crate::cm::channel::<u8>(frame, p)
                        .iter()
                        .map(|&v| (v as f32 - 128.0).abs() / 128.0),
                ),
                _ => return false,
            }
        }
    } else {
        // Every channel in plane nought, one sample's channels side by side.
        // Read as bytes for the reason [`crate::cm`] gives: `plane` would
        // hand back one channel's worth whatever the layout.
        let step = (frame.channels() as usize).max(1);
        let (width, scale) = match frame.format() {
            Sample::F32(_) => (4, 1.0),
            Sample::F64(_) => (8, 1.0),
            Sample::I16(_) => (2, 32768.0),
            Sample::I32(_) => (4, 2147483648.0),
            Sample::U8(_) => (1, 128.0),
            _ => return false,
        };
        let format = frame.format();
        let bytes = frame.data(0);
        let held = (samples * step * width).min(bytes.len());
        for (k, c) in bytes[..held].chunks_exact(width).enumerate() {
            let v = match format {
                Sample::F32(_) => f32::from_ne_bytes(c.try_into().expect("width")).abs() as f64,
                Sample::F64(_) => f64::from_ne_bytes(c.try_into().expect("width")).abs(),
                Sample::I16(_) => i16::from_ne_bytes(c.try_into().expect("width")).unsigned_abs() as f64,
                Sample::U8(_) => (c[0] as f64 - 128.0).abs(),
                _ => i32::from_ne_bytes(c.try_into().expect("width")).unsigned_abs() as f64,
            } / scale;
            let o = &mut out[k / step];
            *o = o.max(v as f32);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_run_from_the_floor_to_full_scale() {
        assert_eq!(level_byte(0.0), 0);
        assert_eq!(level_byte(1.0), 255);
        assert_eq!(level_byte(2.0), 255);
        // -80 dB and anything quieter is the floor.
        assert_eq!(level_byte(1e-4), 0);
        assert_eq!(level_byte(1e-6), 0);
        // -40 dB is half way up.
        assert_eq!(level_byte(0.01), 128);
    }

    #[test]
    fn the_outline_stops_a_little_past_the_end() {
        // A minute past an hour, at twenty buckets a second.
        let near = |got: usize, want: usize| assert!(got.abs_diff(want) <= 1, "{got} for {want}");
        near(most_buckets(3600.0), 3660 * 20);
        // Not known, or not a number: a day.
        near(most_buckets(0.0), 86_400 * 20);
        near(most_buckets(f64::NAN), 86_400 * 20);
        near(most_buckets(f64::INFINITY), 86_400 * 20);
        near(most_buckets(-5.0), 86_400 * 20);
        // A week is the most a length is believed, plus the minute.
        near(most_buckets(1e12), (86_400 * 7 + 60) * 20);
    }

    /// Interleaved samples are one sample's channels side by side: element
    /// `i` is sample `i / channels`, and the loudest of them is the level.
    #[test]
    fn packed_channels_fold_into_their_sample() {
        let layout = ff::channel_layout::ChannelLayout::default(2);
        let mut frame =
            ff::frame::Audio::new(ff::format::Sample::I16(ff::format::sample::Type::Packed), 64, layout);
        frame.data_mut(0).fill(0);
        // Sample 3, right channel, full scale; sample 10, left, half.
        frame.data_mut(0)[(3 * 2 + 1) * 2..(3 * 2 + 2) * 2].copy_from_slice(&i16::MIN.to_ne_bytes());
        frame.data_mut(0)[(10 * 2) * 2..(10 * 2 + 1) * 2].copy_from_slice(&16384i16.to_ne_bytes());
        let mut out = Vec::new();
        assert!(sample_levels(&frame, &mut out));
        assert_eq!(out.len(), 64);
        assert_eq!(out[3], 1.0);
        assert_eq!(out[10], 0.5);
        assert_eq!(out.iter().filter(|&&v| v > 0.0).count(), 2);
    }

    /// A planar track wider than eight channels is read past the eighth, and
    /// unsigned bytes are silent at 128.
    #[test]
    fn planar_and_unsigned_levels() {
        let layout = ff::channel_layout::ChannelLayout::default(10);
        let samples = 32;
        let mut frame =
            ff::frame::Audio::new(ff::format::Sample::F32(ff::format::sample::Type::Planar), samples, layout);
        let mut channel = |p: usize| unsafe {
            let data = *(*frame.as_mut_ptr()).extended_data.add(p);
            std::slice::from_raw_parts_mut(data as *mut f32, samples)
        };
        for p in 0..10 {
            channel(p).fill(0.0);
        }
        channel(9)[7] = -0.25;
        let mut out = Vec::new();
        assert!(sample_levels(&frame, &mut out));
        assert_eq!(out[7], 0.25);
        assert_eq!(out.iter().filter(|&&v| v > 0.0).count(), 1);

        let layout = ff::channel_layout::ChannelLayout::default(1);
        let mut frame =
            ff::frame::Audio::new(ff::format::Sample::U8(ff::format::sample::Type::Packed), 16, layout);
        frame.data_mut(0).fill(128);
        assert!(sample_levels(&frame, &mut out));
        assert!(out.iter().all(|&v| v == 0.0));
        assert_eq!(level_byte(f64::from(out[0])), 0);
    }

    /// A float track's damaged samples: not a number is passed over by the
    /// fold rather than spreading into the level, and an infinite one is
    /// full scale, not a byte the drawing cannot place.
    #[test]
    fn samples_that_are_not_numbers() {
        let layout = ff::channel_layout::ChannelLayout::default(2);
        let samples = 8;
        let mut frame =
            ff::frame::Audio::new(ff::format::Sample::F32(ff::format::sample::Type::Planar), samples, layout);
        let mut channel = |p: usize| unsafe {
            let data = *(*frame.as_mut_ptr()).extended_data.add(p);
            std::slice::from_raw_parts_mut(data as *mut f32, samples)
        };
        channel(0).fill(f32::NAN);
        channel(1).fill(0.0);
        channel(1)[2] = 0.5;
        channel(0)[5] = f32::NEG_INFINITY;
        let mut out = Vec::new();
        assert!(sample_levels(&frame, &mut out));
        assert!(out.iter().all(|v| !v.is_nan()), "{out:?}");
        assert_eq!(out[2], 0.5);
        assert_eq!(level_byte(f64::from(out[5])), 255);
        assert_eq!(level_byte(f64::NAN), 0);
        assert_eq!(level_byte(f64::INFINITY), 255);
    }
}
