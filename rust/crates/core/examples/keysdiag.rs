//! Does skipping the pictures between entry points change what comes back?
//!
//! `shots_at` decodes only the entry pictures when every time asked for is an
//! entry point. That has to be invisible: the same picture, byte for byte, as
//! the path that decodes everything -- which is what `shot_at` still does.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: keysdiag <file>");
    let n: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let src = sc::scan(&path)?;
    let step = (src.points.len() / n.max(1)).max(1);
    let times: Vec<f64> = src.points.iter().step_by(step).map(|p| p.time).take(n).collect();

    let began = std::time::Instant::now();
    let run = sc::shots_at(&src, &times, 200)?;
    let strip = began.elapsed().as_secs_f64();

    let began = std::time::Instant::now();
    let one: Vec<_> = times.iter().map(|&t| sc::shot_at(&src, t, 200)).collect();
    let each = began.elapsed().as_secs_f64();

    let mut same = 0;
    for (i, (a, b)) in run.iter().zip(&one).enumerate() {
        match (a, b) {
            (Some(a), Ok(b)) if a.time == b.time && a.jpeg == b.jpeg => same += 1,
            (Some(a), Ok(b)) => println!(
                "  differs at {:.3}: run {:.3} {}B, one {:.3} {}B",
                times[i], a.time, a.jpeg.len(), b.time, b.jpeg.len()
            ),
            (a, b) => println!(
                "  missing at {:.3}: run {}, one {}",
                times[i],
                if a.is_some() { "ok" } else { "none" },
                match b { Ok(_) => "ok".to_string(), Err(e) => e.to_string() },
            ),
        }
    }
    println!("{same}/{} identical   shots_at {strip:.2}s, one at a time {each:.2}s", times.len());
    Ok(())
}
