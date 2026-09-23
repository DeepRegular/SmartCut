//! Can playback keep up? Decodes a stretch the way the preview's 再生 does,
//! showing every picture, and reports how fast the pictures came against how
//! fast the recording needs them.
//!
//!     playrate <recording> [from=600] [seconds=20] [width=1280]
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: playrate <file> [from] [seconds] [width]");
    let from: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(600.0);
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let width: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1280);

    // As the editor opens it: a disc's own index where there is one.
    let src = sc::scan_with(path, &sc::DiscIndex).or_else(|_| sc::scan(path))?;
    let fps = src.video.frame_rate;
    let began = std::time::Instant::now();
    let mut first: Option<std::time::Duration> = None;
    let mut shown = 0usize;
    let mut worst = 0.0f64;
    let mut last = None::<std::time::Instant>;
    sc::play_from(
        &src,
        from,
        from + secs,
        width,
        |_| sc::Pace::Show,
        |_, _| {
            let now = std::time::Instant::now();
            first.get_or_insert(began.elapsed());
            if let Some(l) = last {
                worst = worst.max((now - l).as_secs_f64());
            }
            last = Some(now);
            shown += 1;
        },
    )?;
    let wall = began.elapsed().as_secs_f64();
    let first = first.map(|d| d.as_secs_f64()).unwrap_or(f64::NAN);
    let rate = (shown.saturating_sub(1)) as f64 / (wall - first).max(1e-9);
    println!(
        "{path}\n  {}x{} {} {:.3}fps  from {from}s for {secs}s at {width}px\n  \
         first picture {first:.2}s  {shown} shown in {wall:.2}s  -> {rate:.1} pictures/s \
         ({:.0}% of real time)  longest wait {:.0}ms",
        src.video.width,
        src.video.height,
        src.video.codec,
        fps,
        100.0 * rate / fps,
        worst * 1000.0,
    );
    Ok(())
}
