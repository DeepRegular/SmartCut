//! Where each audio encoder puts its packets, relative to what it was fed.
//!
//! Smart rendering swaps one of the recording's own frames for one written
//! here, and that only works where a packet this encoder makes covers
//! exactly the samples the frame it replaces covered. AAC's does; the
//! question this asks is what the others do, and whether feeding a few
//! samples of lead-in first would bring them onto the grid.
//!
//! usage: encdelay [codec ...]   (default: aac ac3 eac3 mp2)

use anyhow::Result;
use ffmpeg_next as ff;

fn probe(name: &str) -> Result<()> {
    let Some(codec) = ff::encoder::find_by_name(name) else {
        println!("{name}: no encoder");
        return Ok(());
    };
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()?;
    let rate = 48_000;
    enc.set_rate(rate);
    enc.set_channel_layout(ff::channel_layout::ChannelLayout::default(2));
    let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let format = codec
        .audio()
        .ok()
        .and_then(|a| a.formats())
        .and_then(|mut f| {
            let all: Vec<_> = f.by_ref().collect();
            if all.contains(&format) {
                Some(format)
            } else {
                all.first().copied()
            }
        })
        .unwrap_or(format);
    enc.set_format(format);
    enc.set_bit_rate(192_000);
    enc.set_time_base(ff::Rational::new(1, rate));
    let mut opts = ff::Dictionary::new();
    opts.set("strict", "experimental");
    let mut enc = enc.open_as_with(codec, opts)?;

    let size = if enc.frame_size() > 0 {
        enc.frame_size() as usize
    } else {
        1024
    };
    let delay = unsafe { (*enc.as_ptr()).initial_padding.max(0) } as usize;
    let layout = enc.channel_layout();
    println!("{name}: frame_size {size}  initial_padding {delay}  format {format:?}");

    let mut pts_out: Vec<i64> = Vec::new();
    let mut fed = 0i64;
    for i in 0..8 {
        let mut frame = ff::frame::Audio::new(format, size, layout);
        frame.set_rate(rate as u32);
        // A recognisable ramp per frame, so a decode of the result could be
        // matched back to what went in. Nothing here reads it.
        if format == ff::format::Sample::F32(ff::format::sample::Type::Planar) {
            for ch in 0..2 {
                for (k, s) in frame.plane_mut::<f32>(ch)[..size].iter_mut().enumerate() {
                    *s = ((i * size + k) as f32 * 0.001).sin() * 0.5;
                }
            }
        }
        frame.set_pts(Some(fed));
        fed += size as i64;
        enc.send_frame(&frame)?;
        drain(&mut enc, &mut pts_out);
    }
    enc.send_eof()?;
    drain(&mut enc, &mut pts_out);
    println!("  packet pts: {pts_out:?}");
    let lead = (size - delay % size) % size;
    println!(
        "  off the grid by {}  -> lead-in of {lead} samples would land them on it",
        delay % size
    );
    Ok(())
}

fn drain(enc: &mut ff::encoder::Audio, out: &mut Vec<i64>) {
    loop {
        let mut packet = ff::Packet::empty();
        if enc.receive_packet(&mut packet).is_err() {
            return;
        }
        out.push(packet.pts().unwrap_or(i64::MIN));
    }
}

fn main() -> Result<()> {
    ff::init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let names: Vec<&str> = if args.is_empty() {
        vec!["aac", "ac3", "eac3", "mp2"]
    } else {
        args.iter().map(String::as_str).collect()
    };
    for name in names {
        if let Err(e) = probe(name) {
            println!("{name}: {e}");
        }
    }
    Ok(())
}
