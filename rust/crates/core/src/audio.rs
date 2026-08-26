//! Sample-exact audio, at the price of re-encoding it.
//!
//! Copying audio frames leaves each range's boundary on a whole-frame edge --
//! about half a frame out, 10.7 ms for AAC at 48 kHz. That cannot be trimmed
//! away in the container: this MP4 muxer honours neither
//! `AV_PKT_DATA_SKIP_SAMPLES` nor a stream's `initial_padding`, both of which
//! were tried and measured. Decoding and re-encoding is what is left.
//!
//! The trade is real, so this is not the default: a smart-rendering tool
//! exists to avoid re-encoding, and 10.7 ms sits well under the threshold at
//! which anyone notices lip-sync drift.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::AudioInfo;

pub struct Reencoder {
    decoder: ff::decoder::Audio,
    encoder: ff::encoder::Audio,
    /// Planar float, one buffer per channel.
    pending: Vec<Vec<f32>>,
    frame_size: usize,
    channels: usize,
    sample_rate: u32,
    /// Samples handed to the encoder so far -- the output timeline.
    fed: i64,
    /// Leading frames the encoder emits as priming, which the container will
    /// not trim for us, so they are dropped here instead.
    drop_frames: usize,
    dropped: usize,
    /// Last source packet consumed, so a frame offered twice is ignored.
    last_pts: Option<i64>,
}

impl Reencoder {
    pub fn new(params: ff::codec::Parameters, audio: &AudioInfo, bit_rate: usize) -> Result<Self> {
        let decoder =
            ff::codec::context::Context::from_parameters(params.clone())?.decoder().audio()?;

        let id = params.id();
        let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no encoder for {id:?}"))?;
        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
        enc.set_rate(audio.sample_rate as i32);
        enc.set_channel_layout(ff::channel_layout::ChannelLayout::default(audio.channels as i32));
        enc.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
        enc.set_bit_rate(bit_rate);
        enc.set_time_base(ff::Rational::new(1, audio.sample_rate as i32));
        let encoder = enc.open_as(codec)?;

        let frame_size = if encoder.frame_size() > 0 { encoder.frame_size() as usize } else { 1024 };
        // The native AAC encoder's delay is one frame; dropping that many
        // output frames lines the stream back up.
        let delay = unsafe { (*encoder.as_ptr()).initial_padding.max(0) as usize };
        Ok(Self {
            decoder,
            encoder,
            pending: vec![Vec::new(); audio.channels as usize],
            frame_size,
            channels: audio.channels as usize,
            sample_rate: audio.sample_rate,
            fed: 0,
            drop_frames: delay.div_ceil(frame_size.max(1)),
            dropped: 0,
            last_pts: None,
        })
    }

    /// What the output stream has to say about itself once this encoder is
    /// the one producing the packets.
    ///
    /// Not the source stream's parameters: they describe a different
    /// bitstream. MPEG-TS in particular reframes raw AAC into ADTS using the
    /// stream's own extradata, and given the wrong extradata it writes a PID
    /// full of bytes no decoder can find a sync word in.
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
            let (a, b) = ((lo - first) as usize, (hi - first) as usize);
            for ch in 0..self.channels {
                let plane = if frame.is_planar() { ch } else { 0 };
                let data: &[f32] = frame.plane(plane);
                if frame.is_planar() {
                    self.pending[ch].extend_from_slice(&data[a..b]);
                } else {
                    // interleaved: pick this channel's samples out
                    self.pending[ch]
                        .extend((a..b).map(|i| data[i * self.channels + ch]));
                }
            }
        }
        Ok(())
    }

    /// Hand the encoder every full frame that has accumulated.
    pub fn drain(&mut self, out: &mut Vec<(ff::Packet, i64)>) -> Result<()> {
        while self.pending[0].len() >= self.frame_size {
            let mut frame = ff::frame::Audio::new(
                ff::format::Sample::F32(ff::format::sample::Type::Planar),
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
            if self.dropped < self.drop_frames {
                self.dropped += 1;
                continue;
            }
            let pts = packet.pts().unwrap_or(0) - (self.drop_frames * self.frame_size) as i64;
            out.push((packet, pts.max(0)));
        }
    }
}
