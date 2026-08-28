//! Diagnostic for the film strip while a proxy is being built.
//!
//! The complaint is that scrubbing the strip during a build is unusable, and
//! the GUI's own timings cannot say why: a slow strip and a starved strip look
//! the same from the webview. So this runs the two side by side -- a proxy
//! build on one thread, strip requests on another -- and prints how long each
//! request took, against the same requests measured with the machine idle.

use anyhow::Result;
use smartcut_core as sc;

/// The strip's two shapes, as the GUI asks for them: `vis` cells `step`
/// apart, the playhead in the middle.
fn strip_times(centre: f64, step: f64, vis: usize, dur: f64) -> Vec<f64> {
    let half = (vis / 2) as f64;
    (0..vis)
        .map(|i| (centre + (i as f64 - half) * step).clamp(0.0, dur - 0.1))
        .collect()
}

fn time_it<T>(f: impl FnOnce() -> Result<T>) -> Result<(T, f64)> {
    let began = std::time::Instant::now();
    let out = f()?;
    Ok((out, began.elapsed().as_secs_f64()))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: proxydiag <file> [rounds]");
    let rounds: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(6);
    let out = std::env::temp_dir().join("proxydiag.mp4");

    let src = sc::scan(path)?;
    println!(
        "file      {path}\n\
         duration  {:.1}s  {}x{}  {:.3} fps  {} access points\n\
         cores     {}",
        src.duration,
        src.video.width,
        src.video.height,
        src.video.frame_rate,
        src.points.len(),
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
    );

    // Where the strip is asked to look, in the order a scrub would ask.
    let asks: Vec<(&str, Vec<f64>)> = (0..rounds)
        .map(|i| {
            let centre = src.duration * (0.2 + 0.1 * (i % 6) as f64);
            if i % 2 == 0 {
                ("GOP 3分  ", strip_times(centre, 14.0, 13, src.duration))
            } else {
                ("フレーム ", strip_times(centre, 1.0 / src.video.frame_rate, 25, src.duration))
            }
        })
        .collect();

    let run = |label: &str, asks: &[(&str, Vec<f64>)]| -> Result<()> {
        for (shape, times) in asks {
            let (got, secs) = time_it(|| sc::shots_at(&src, times, 200))?;
            let filled = got.iter().filter(|s| s.is_some()).count();
            println!("  {label} {shape} {:6.2}s  {filled}/{} 枚", secs, times.len());
        }
        Ok(())
    };

    println!("\n--- 何も走っていないとき ---");
    run("idle ", &asks)?;

    println!("\n--- プロキシ作成中 ---");
    let building = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let done = building.clone();
    let bsrc = src.clone();
    let bout = out.to_string_lossy().into_owned();
    let builder = std::thread::spawn(move || {
        let r = sc::proxy::build(
            &bsrc,
            &bout,
            &sc::ProxyOptions::default(),
            &sc::ThumbOptions::default(),
            None,
            None,
            Some(Box::new(move || !done.load(std::sync::atomic::Ordering::SeqCst))),
        );
        r.map(|b| b.pictures).map_err(|e| e.to_string())
    });
    // Let the encoder get going, so the strip is measured against a build at
    // full tilt and not against its first second.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let began = std::time::Instant::now();
    run("build", &asks)?;
    let spent = began.elapsed().as_secs_f64();
    building.store(false, std::sync::atomic::Ordering::SeqCst);
    let built = builder.join().map_err(|_| anyhow::anyhow!("build panicked"))?;
    println!("\nストリップ合計 {spent:.1}s   build: {built:?}");
    let _ = std::fs::remove_file(&out);
    Ok(())
}
