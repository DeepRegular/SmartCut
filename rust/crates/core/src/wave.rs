//! The sound's outline over the whole recording, for drawing under the
//! timeline.
//!
//! One read of the main audio track, kept as two levels per bucket of
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

/// Read the main audio track and outline it, as a pass of its own.
///
/// For a recording whose outline was not made on the way past: one opened
/// before this was written, or one whose pictures were already kept. Where
/// the pictures are read, [`WaveBuilder`] rides along with them instead and
/// this read never happens; see [`crate::thumbs::build_with_sound`].
pub fn read_wave(src: &Source, mut progress: Option<Box<dyn FnMut(f64) + Send>>) -> Result<Wave> {
    crate::init()?;
    let audio = src
        .audio
        .as_ref()
        .ok_or_else(|| anyhow!("{} has no audio", src.path))?;
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
    frame: ff::frame::Audio,
    levels: Vec<f32>,
    peak: Vec<f32>,
    energy: Vec<(f64, u32)>,
    /// How far into the recording the sound has been read, in seconds.
    pub reached: f64,
}

impl WaveBuilder {
    /// For `src`'s main audio track, out of the demuxer the read has open.
    pub fn new(src: &Source, ictx: &crate::input::Demux) -> Result<Self> {
        let audio = src
            .audio
            .as_ref()
            .ok_or_else(|| anyhow!("{} has no audio", src.path))?;
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
}
