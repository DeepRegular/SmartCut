//! Diagnostic for the film strip's GOP mode.
//!
//! Reproduces what the GUI does: warm the thumbnail track, then ask it for
//! the picture at every GOP start, and compare that against a fresh decode of
//! the same instants. Prints where the two disagree and dumps both as JPEGs.

use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: stripdiag <file> [centre] [span] [outdir]");
    let centre: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1.0);
    let span: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let outdir = args.get(3).cloned().unwrap_or_else(|| "/tmp/stripdiag".into());
    std::fs::create_dir_all(&outdir)?;

    let src = sc::scan(path)?;
    println!(
        "file      {path}\n\
         duration  {:.3}s  start_time {:.3}s  fps {:.5}  {}x{} sar {:.4} field_order {}\n\
         points    {}  (first {:.3}  last {:.3})",
        src.duration,
        src.start_time,
        src.video.frame_rate,
        src.video.width,
        src.video.height,
        src.video.sample_aspect_ratio,
        src.video.field_order,
        src.points.len(),
        src.points.first().map(|p| p.time).unwrap_or(0.0),
        src.points.last().map(|p| p.time).unwrap_or(0.0),
    );

    let opts = sc::ThumbOptions::default();
    let began = std::time::Instant::now();
    let track = sc::thumbs::build(&src, &opts, None)?;
    println!(
        "track     {} thumbs  interval {:.4}s  built in {:.1}s",
        track.thumbs.len(),
        track.interval,
        began.elapsed().as_secs_f64()
    );

    // How far the held picture nearest each access point actually is.
    let mut worst: Vec<(f64, f64, f64)> = Vec::new(); // (delta, point, held)
    let mut missing = 0usize;
    for p in &src.points {
        match track.nearest(p.time) {
            Some(t) => worst.push(((t.time - p.time).abs(), p.time, t.time)),
            None => missing += 1,
        }
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    let over = worst.iter().filter(|w| w.0 > 0.05).count();
    println!(
        "nearest   {} access points, {} with no held picture, {} off by >50ms",
        src.points.len(),
        missing,
        over
    );
    for w in worst.iter().take(12) {
        println!("            off {:7.3}s   point {:9.3}  held {:9.3}", w.0, w.1, w.2);
    }

    // Also: are the held pictures themselves at access points?
    let mut stray = 0usize;
    for th in &track.thumbs {
        let near = src
            .points
            .iter()
            .map(|p| (p.time - th.time).abs())
            .fold(f64::INFINITY, f64::min);
        if near > 0.02 {
            stray += 1;
        }
    }
    println!("stray     {} held pictures more than 20ms from any access point", stray);

    if centre < 0.0 {
        return Ok(());
    }

    // The GUI's GOP cells: every access point inside the window, decimated.
    let (w0, w1) = (centre - span / 2.0, centre + span / 2.0);
    let marks: Vec<f64> =
        src.points.iter().map(|p| p.time).filter(|&t| t >= w0 && t < w1).collect();
    let every = ((marks.len() as f64) / 15.0).ceil().max(1.0) as usize;
    let cells: Vec<f64> =
        marks.iter().copied().enumerate().filter(|(k, _)| k % every == 0).map(|(_, t)| t).collect();
    println!("\ncells     {} in [{:.3}, {:.3})", cells.len(), w0, w1);

    let shots = sc::shots_at(&src, &cells, 192)?;
    for (i, (&t, shot)) in cells.iter().zip(shots.iter()).enumerate() {
        let held = track.nearest(t);
        if let Some(h) = held {
            std::fs::write(format!("{outdir}/{i:02}_held_{:.3}.jpg", h.time), &h.jpeg)?;
        }
        if let Some(s) = shot {
            std::fs::write(format!("{outdir}/{i:02}_dec_{:.3}.jpg", s.time), &s.jpeg)?;
        }
        println!(
            "  cell {i:2}  want {:9.3}   held {:>9}  {:>6}   decoded {:>9} {:>3} {:>6}",
            t,
            held.map(|h| format!("{:.3}", h.time)).unwrap_or_else(|| "-".into()),
            held.map(|h| format!("{}B", h.jpeg.len())).unwrap_or_default(),
            shot.as_ref().map(|s| format!("{:.3}", s.time)).unwrap_or_else(|| "-".into()),
            shot.as_ref().map(|s| s.kind.to_string()).unwrap_or_default(),
            shot.as_ref().map(|s| format!("{}B", s.jpeg.len())).unwrap_or_default(),
        );
    }
    println!("\nwrote     {outdir}");
    Ok(())
}
