//! Diagnostic for the weaving of repeated fields into frames.
//!
//! Asks for a run of frames on the editor's grid -- the first access point
//! plus a whole number of frames -- the way the film strip's one-frame mode
//! does, and says which came back, at what instant and of what kind. With
//! `outdir` it also writes each one out, full size, so the comb on the woven
//! frames can be looked at.
//!
//!     weavediag <file> <from seconds> [count] [outdir]

use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: weavediag <file> <from> [count] [outdir]");
    let from: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let outdir = args.get(3).cloned();

    let src = sc::scan(path)?;
    let fd = src.video.frame_duration();
    let head = src.points.first().map_or(0.0, |p| p.time);
    println!(
        "pulldown {}  woven {}  fps {:.5}  field_order {}  head {:.4}",
        src.video.pulldown,
        sc::weave::woven(&src),
        src.video.frame_rate,
        src.video.field_order,
        head
    );

    let first = head + ((from - head) / fd).round() * fd;
    let times: Vec<f64> = (0..count).map(|i| first + i as f64 * fd).collect();
    let width = src.video.width;
    let began = std::time::Instant::now();
    let shots = sc::shots_at(&src, &times, width)?;
    println!("shots_at: {count} slots in {:.0} ms", began.elapsed().as_secs_f64() * 1000.0);
    let mut empty = 0;
    for (i, (t, s)) in times.iter().zip(&shots).enumerate() {
        match s {
            Some(s) => {
                let off = (s.time - t) / fd;
                println!("  {i:3}  {t:9.4}  got {:9.4} ({off:+.2} fr)  {}", s.time, s.kind);
                if let Some(dir) = &outdir {
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(format!("{dir}/{i:03}.jpg"), &s.jpeg)?;
                }
            }
            None => {
                empty += 1;
                println!("  {i:3}  {t:9.4}  --");
            }
        }
    }
    println!("empty {empty} of {count}");

    // The stage's own path, one frame at a time, which has to agree.
    let mut disagree = 0;
    for (i, &t) in times.iter().enumerate() {
        let (at, picture) = sc::preview::picture_at(&src, t)?;
        // At the recording's own lines, as the magnifier asks for them: the
        // comb is one line out of step with the next and any scaling of the
        // height takes it out.
        if let Some(dir) = &outdir {
            let jpeg = sc::preview::jpeg_of(&picture, src.video.sample_aspect_ratio, u32::MAX)?;
            std::fs::write(format!("{dir}/z{i:03}.jpg"), jpeg)?;
        }
        if (at - t).abs() > fd / 4.0 {
            disagree += 1;
            println!("  picture_at({t:.4}) -> {at:.4}");
        }
    }
    println!("picture_at off the asked frame: {disagree}");

    // Access points that fall half way through a frame, and the frame the
    // editor is told they are on.
    let mid = src
        .points
        .iter()
        .filter(|p| (sc::weave::on_frame(&src, p.time) - p.time).abs() > fd / 4.0)
        .count();
    println!("points half way through a frame: {mid} of {}", src.points.len());
    for p in src.points.iter().take(8) {
        println!("  point {:9.4} -> frame {:9.4}", p.time, sc::weave::on_frame(&src, p.time));
    }
    Ok(())
}
