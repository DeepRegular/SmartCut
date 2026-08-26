//! Diagnostic for `play_audio`: play a couple of disjoint ranges from a real
//! file straight to the sound card, the same way the GUI's preview does when
//! ranges hold a cut. Runs until each range plays out or `--seconds` elapses.
//!
//! usage: audiodiag <file> [seconds]

use anyhow::Result;
use smartcut_core as sc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: audiodiag <file> [seconds]");
    let seconds: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20.0);

    let src = sc::scan(path)?;
    println!(
        "file {path}\nduration {:.3}s has_audio {}",
        src.duration,
        src.audio.is_some()
    );
    if let Some(a) = &src.audio {
        println!(
            "audio stream_index {} rate {} channels {} time_base {:?}",
            a.stream_index, a.sample_rate, a.channels, a.time_base
        );
    }

    let d = src.duration;
    let ranges = vec![(d * 0.1, d * 0.1 + 15.0), (d * 0.5, d * 0.5 + 15.0)];
    println!("ranges {ranges:?}");

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs_f64(seconds));
        stop2.store(true, Ordering::SeqCst);
    });

    let began = Instant::now();
    let r = sc::play_audio(&src, &ranges, ranges[0].0, move || stop.load(Ordering::SeqCst));
    println!("play_audio -> {r:?}  elapsed {:.2}s", began.elapsed().as_secs_f64());
    Ok(())
}
