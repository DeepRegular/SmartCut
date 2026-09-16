//! Does spreading the entry pictures over the cores change the answer?
//!
//! Builds the track twice -- once the way [`smartcut_core::thumbs::build`]
//! now does it, and once with the single decoder it used to use, written out
//! here so the two can be compared on the same machine -- and reports any
//! difference in what they held, when, and how the scenes came out.
//!
//!     poolcheck <file> [seconds=120]

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use smartcut_core as sc;

/// The pass as it stood: one decoder, one picture at a time.
fn serially(src: &sc::Source, opts: &sc::ThumbOptions) -> Result<sc::thumbs::Track> {
    let mut ictx = sc::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("no video"))?.parameters();
    let mut decoder = sc::video_decoder_with(params, opts.threads)?;
    let mut collector = sc::thumbs::Collector::new(src, opts);
    let mut entries = sc::EntryPictures::new(&src.video);
    let mut frame = ff::frame::Video::empty();
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        let step = entries.step(&packet);
        if step == sc::Step::Skip {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            entries.broke();
            continue;
        }
        if step == sc::Step::Half {
            continue;
        }
        let _ = decoder.send_eof();
        while decoder.receive_frame(&mut frame).is_ok() {
            if let Some(pts) = frame.pts() {
                let t = pts as f64 * src.video.time_base - src.start_time;
                collector.feed(t, &frame)?;
            }
        }
        decoder.flush();
    }
    Ok(collector.finish(src.duration))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: poolcheck <file> [seconds]");
    let until = f64::INFINITY;
    let src = sc::scan(path)?;
    println!(
        "{path}\n  {} {}x{}  {} points  {:.1}s",
        src.video.codec, src.video.width, src.video.height, src.points.len(), src.duration
    );
    let opts = sc::ThumbOptions::default();

    let began = std::time::Instant::now();
    let one = serially(&src, &opts)?;
    let one_secs = began.elapsed().as_secs_f64();

    // Both over the whole file: the scene threshold is read off the
    // recording's own typical difference, so a truncated pass and a whole one
    // do not have comparable scene lists.
    let began = std::time::Instant::now();
    let many = sc::thumbs::build(&src, &opts, None)?;
    let many_secs = began.elapsed().as_secs_f64();

    let a: Vec<_> = one.thumbs.iter().filter(|t| t.time <= until).collect();
    let b: Vec<_> = many.thumbs.iter().filter(|t| t.time <= until).collect();
    println!(
        "  one decoder  {} pictures in {one_secs:.2}s\n  a pool       {} pictures in {many_secs:.2}s",
        a.len(),
        b.len()
    );
    let mut off = 0;
    let mut bytes = 0;
    for (x, y) in a.iter().zip(&b) {
        if (x.time - y.time).abs() > 1e-6 {
            if off < 8 {
                println!("    time   {:.4} against {:.4}", x.time, y.time);
            }
            off += 1;
        } else if x.jpeg != y.jpeg {
            if bytes < 8 {
                println!(
                    "    image at {:.4}  {} bytes against {}",
                    x.time,
                    x.jpeg.len(),
                    y.jpeg.len()
                );
            }
            let dir = std::path::Path::new("/media/hdd/kaz/tmp/poolcheck");
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(format!("{:.4}-one.jpg", x.time)), &x.jpeg);
            let _ = std::fs::write(dir.join(format!("{:.4}-pool.jpg", y.time)), &y.jpeg);
            bytes += 1;
        }
    }
    let sa: Vec<_> = one.scenes.iter().filter(|t| **t <= until).collect();
    let sb: Vec<_> = many.scenes.iter().filter(|t| **t <= until).collect();
    println!(
        "  {} pictures at a different time, {bytes} with a different image\n  \
         scenes {} against {}{}",
        off,
        sa.len(),
        sb.len(),
        if sa == sb { "  (the same list)" } else { "  DIFFERENT" }
    );
    if a.len() == b.len() && off == 0 && bytes == 0 && sa == sb {
        println!("  same answer.");
    }
    Ok(())
}
