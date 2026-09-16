//! Where the thumbnail pass's time actually goes, and whether the decoder is
//! being given anything to thread with.
//!
//!     thumbcost <file> [pictures=300]

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use smartcut_core as sc;

fn decoder(params: ff::codec::Parameters, threads: usize, slice: bool) -> Result<ff::decoder::Video> {
    let mut ctx = ff::codec::context::Context::from_parameters(params)?;
    unsafe {
        (*ctx.as_mut_ptr()).thread_count = threads as i32;
        if slice {
            (*ctx.as_mut_ptr()).thread_type = ff::ffi::FF_THREAD_SLICE;
        }
    }
    Ok(ctx.decoder().video()?)
}

fn out_size(w: u32, h: u32, sar: f64, width: u32) -> (u32, u32) {
    let sar = sar.max(0.01);
    let native = (w as f64 * sar).round() as u32;
    let out_w = (width.min(native).max(16)) & !1;
    let out_h = ((((out_w as f64 * h as f64) / (w as f64 * sar)).round() as u32).max(16)) & !1;
    (out_w, out_h)
}

fn scaler(f: &ff::frame::Video, ow: u32, oh: u32) -> Result<ff::software::scaling::Context> {
    Ok(ff::software::scaling::Context::get(
        f.format(), f.width(), f.height(),
        ff::format::Pixel::YUVJ420P, ow, oh,
        ff::software::scaling::Flags::AREA,
    )?)
}

fn mjpeg(ow: u32, oh: u32) -> Result<ff::encoder::video::Encoder> {
    let codec = ff::encoder::find(ff::codec::Id::MJPEG).ok_or_else(|| anyhow!("no MJPEG"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
    enc.set_width(ow);
    enc.set_height(oh);
    enc.set_format(ff::format::Pixel::YUVJ420P);
    enc.set_time_base(ff::Rational::new(1, 25));
    unsafe {
        (*enc.as_mut_ptr()).flags |= ff::ffi::AV_CODEC_FLAG_QSCALE as i32;
        (*enc.as_mut_ptr()).global_quality = ff::ffi::FF_QP2LAMBDA * 4;
    }
    Ok(enc.open_as(codec)?)
}

/// What the jpeg is asked to do with a picture.
#[derive(Clone, Copy, PartialEq)]
enum Jpeg { None, Fresh, Reused }

/// How far the pass is taken, so the stages can be told apart.
#[derive(Clone, Copy, PartialEq)]
enum Far { Demux, Send, Decode }

/// One pass over the first `n` entry pictures. `drain` reproduces the
/// per-packet flush the real pass makes.
fn pass(src: &sc::Source, n: usize, threads: usize, slice: bool, drain: bool, jpeg: Jpeg, far: Far)
    -> Result<(usize, f64, f64)>
{
    let mut ictx = sc::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("no video"))?.parameters();
    let mut dec = decoder(params, threads, slice)?;
    let mut entries = sc::EntryPictures::new(&src.video);
    let mut frame = ff::frame::Video::empty();
    let mut scaled = ff::frame::Video::empty();
    let mut held: Option<(ff::software::scaling::Context, ff::encoder::video::Encoder)> = None;
    let mut got = 0usize;
    let mut jpeg_secs = 0.0;
    let began = std::time::Instant::now();
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        let step = entries.step(&packet);
        if step == sc::Step::Skip {
            continue;
        }
        if far == Far::Demux {
            got += 1;
            if got >= n { break; }
            continue;
        }
        if dec.send_packet(&packet).is_err() {
            entries.broke();
            continue;
        }
        if step == sc::Step::Half {
            continue;
        }
        if far == Far::Send {
            got += 1;
            dec.flush();
            if got >= n { break; }
            continue;
        }
        if drain {
            let _ = dec.send_eof();
        }
        while dec.receive_frame(&mut frame).is_ok() {
            got += 1;
            if jpeg != Jpeg::None {
                let t = std::time::Instant::now();
                let (ow, oh) = out_size(frame.width(), frame.height(), src.video.sample_aspect_ratio, 192);
                if jpeg == Jpeg::Fresh || held.is_none() {
                    held = Some((scaler(&frame, ow, oh)?, mjpeg(ow, oh)?));
                }
                let (sws, enc) = held.as_mut().unwrap();
                sws.run(&frame, &mut scaled)?;
                scaled.set_pts(Some(got as i64));
                enc.send_frame(&scaled)?;
                let mut out = 0usize;
                let mut packet = ff::Packet::empty();
                while enc.receive_packet(&mut packet).is_ok() {
                    out += packet.data().map_or(0, |d| d.len());
                    packet = ff::Packet::empty();
                }
                std::hint::black_box(out);
                jpeg_secs += t.elapsed().as_secs_f64();
                if jpeg == Jpeg::Fresh {
                    held = None;
                }
            }
        }
        if drain {
            dec.flush();
        }
        if got >= n {
            break;
        }
    }
    Ok((got, began.elapsed().as_secs_f64(), jpeg_secs))
}

/// The same entry pictures, decoded by a pool of independent decoders.
///
/// One thread demuxes and hands each entry picture's packets on; the workers
/// each hold a decoder of their own and answer for one picture at a time,
/// which is exactly what the pass already asks of its one decoder -- it
/// flushes between every pair of them.
fn pooled(src: &sc::Source, n: usize, workers: usize, jpeg: bool) -> Result<(usize, f64)> {
    use std::sync::mpsc;
    let mut ictx = sc::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("no video"))?.parameters();
    let began = std::time::Instant::now();
    let (jobs_tx, jobs_rx) = mpsc::sync_channel::<(usize, Vec<ff::Packet>)>(workers * 2);
    let jobs_rx = std::sync::Arc::new(std::sync::Mutex::new(jobs_rx));
    let (out_tx, out_rx) = mpsc::channel::<(usize, f64)>();
    let sar = src.video.sample_aspect_ratio;
    let tb = src.video.time_base;
    let start = src.start_time;
    std::thread::scope(|scope| -> Result<(usize, f64)> {
        for _ in 0..workers {
            let params = params.clone();
            let rx = jobs_rx.clone();
            let tx = out_tx.clone();
            scope.spawn(move || {
                let Ok(mut dec) = decoder(params, 1, false) else { return };
                let mut frame = ff::frame::Video::empty();
                let mut scaled = ff::frame::Video::empty();
                let mut held: Option<(ff::software::scaling::Context, ff::encoder::video::Encoder)> = None;
                loop {
                    let job = { rx.lock().unwrap().recv() };
                    let Ok((i, packets)) = job else { return };
                    for p in &packets {
                        if dec.send_packet(p).is_err() {
                            break;
                        }
                    }
                    let _ = dec.send_eof();
                    while dec.receive_frame(&mut frame).is_ok() {
                        let Some(pts) = frame.pts() else { continue };
                        if jpeg {
                            let (ow, oh) = out_size(frame.width(), frame.height(), sar, 192);
                            if held.is_none() {
                                held = Some((scaler(&frame, ow, oh).unwrap(), mjpeg(ow, oh).unwrap()));
                            }
                            let (sws, enc) = held.as_mut().unwrap();
                            sws.run(&frame, &mut scaled).unwrap();
                            scaled.set_pts(Some(i as i64));
                            enc.send_frame(&scaled).unwrap();
                            let mut packet = ff::Packet::empty();
                            while enc.receive_packet(&mut packet).is_ok() {
                                packet = ff::Packet::empty();
                            }
                        }
                        let _ = tx.send((i, pts as f64 * tb - start));
                    }
                    dec.flush();
                }
            });
        }
        drop(out_tx);
        let mut entries = sc::EntryPictures::new(&src.video);
        let mut pending: Vec<ff::Packet> = Vec::new();
        let mut sent = 0usize;
        for (stream, packet) in ictx.packets() {
            if stream.index() != idx {
                continue;
            }
            match entries.step(&packet) {
                sc::Step::Skip => continue,
                sc::Step::Half => {
                    pending.push(packet);
                    continue;
                }
                sc::Step::Whole => {
                    pending.push(packet);
                    if jobs_tx.send((sent, std::mem::take(&mut pending))).is_err() {
                        break;
                    }
                    sent += 1;
                    if sent >= n {
                        break;
                    }
                }
            }
        }
        drop(jobs_tx);
        let got = out_rx.iter().count();
        Ok((got, began.elapsed().as_secs_f64()))
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: thumbcost <file> [pictures]");
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let src = sc::scan(path)?;
    println!(
        "{path}\n  {} {}x{}  {} points  {:.1}s  cores {}",
        src.video.codec, src.video.width, src.video.height, src.points.len(), src.duration,
        std::thread::available_parallelism().map(|c| c.get()).unwrap_or(1)
    );
    for (name, threads, slice, drain, jpeg, far) in [
        ("demux alone, nothing decoded                   ", 0usize, false, true, Jpeg::None, Far::Demux),
        ("demux + send + flush, nothing read back        ", 0, false, true, Jpeg::None, Far::Send),
        ("as now: drain+flush, frame threads, fresh jpeg ", 0, false, true, Jpeg::Fresh, Far::Decode),
        ("  ... jpeg encoder and scaler kept             ", 0, false, true, Jpeg::Reused, Far::Decode),
        ("  ... no jpeg at all (the decode alone)        ", 0, false, true, Jpeg::None, Far::Decode),
        ("slice threads, drain+flush, no jpeg            ", 0, true, true, Jpeg::None, Far::Decode),
        ("one thread, drain+flush, no jpeg               ", 1, false, true, Jpeg::None, Far::Decode),
    ] {
        let (got, secs, jpeg_secs) = pass(&src, n, threads, slice, drain, jpeg, far)?;
        println!(
            "  {name} {got} in {secs:.2}s = {:6.1}/s  (jpeg {jpeg_secs:.2}s)",
            got as f64 / secs
        );
    }
    for workers in [2usize, 4, 8, 16] {
        let (got, secs) = pooled(&src, n, workers, true)?;
        println!(
            "  a pool of {workers:2} decoders, with the jpeg              {got} in {secs:.2}s = {:6.1}/s",
            got as f64 / secs
        );
    }
    Ok(())
}
