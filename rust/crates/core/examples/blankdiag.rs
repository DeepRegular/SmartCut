//! What the flat-picture pass finds, and what it cost to find it.
//!
//! Checked against `ffmpeg -vf blackdetect`, which is the same question asked
//! by other means: on a 30-minute 1080i MPEG-2 recording off BS, that filter
//! reports black at 65.80-65.87, 200.30-200.43, 416.02-416.08 and
//! 603.27-603.94 in the first fifteen minutes. The times printed here are
//! the recording's own clock, as the filter's are.
//!
//!     blankdiag <file> [min-pictures]
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: blankdiag <file> [min-pictures]");
    let min: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(2);

    // The container's own answer: this pass reads pictures and never asks
    // where a cut would be free, so there is nothing for a walk to add.
    let src = sc::outline(&path)?.into_source();
    println!(
        "{}  {}x{}  {:.2} fps  {:.1} s",
        src.video.codec, src.video.width, src.video.height, src.video.frame_rate, src.duration
    );

    let opts = sc::BlankOptions {
        min_pictures: min,
        ..Default::default()
    };
    let began = std::time::Instant::now();
    let runs = sc::find_blank_runs(&src, &opts)?;
    let took = began.elapsed().as_secs_f64();

    for r in &runs {
        println!(
            "{:>5} {:>10.3} -> {:>10.3}  {:>6.3} s  {:>4} pictures",
            r.shade.as_str(),
            r.start,
            r.end,
            r.duration(),
            r.pictures
        );
    }
    println!(
        "{} runs in {took:.1} s ({:.0}x real time)",
        runs.len(),
        src.duration / took.max(1e-9)
    );

    // The other half of what the editor offers, and the cheap half: the
    // silences are already what commercial detection is built on, and reading
    // the sound is seconds against the pictures' minute.
    let began = std::time::Instant::now();
    let quiet = sc::find_silences(&src, &sc::DetectOptions::default())?;
    let took = began.elapsed().as_secs_f64();
    for s in &quiet {
        println!(
            "quiet {:>10.3} -> {:>10.3}  {:>6.3} s",
            s.start,
            s.end,
            s.duration()
        );
    }
    println!("{} silences in {took:.1} s", quiet.len());
    Ok(())
}
