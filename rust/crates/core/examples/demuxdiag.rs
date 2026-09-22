//! Does the demuxer hand over every packet the recording holds?
//!
//! It does not always. While a stream nothing reads is left switched on,
//! libavformat can lose its place in a Matroska cluster and resume at the
//! next one, and what it skips is gone from every stream it was delivering.
//! The packets that do arrive are whole and correctly timed, so the loss is
//! invisible from anywhere downstream: playback just stops early, the film
//! strip has stretches with no picture in them, the frame rate reads as
//! varying, and a cut writes fewer frames than it planned. A subtitle track
//! it cannot parse is the way in that has been seen; see
//! [`smartcut_core::input::keep_only`], which is the answer, and
//! `tests/run_demux_tests.sh`, which makes a file that does it.
//!
//! So this reads each stream twice -- once with the others switched on, once
//! with them thrown away -- and prints both counts. A recording where the two
//! disagree is one where naming what is read is not optional.
//!
//!     demuxdiag <file>
//!
//! The exit status says whether they agreed, so a test can ask.
use anyhow::Result;
use smartcut_core as sc;

/// How many packets each stream hands over, and the first and last instant
/// seen on each, reading `only` alone where it is given.
fn count(path: &str, only: Option<usize>) -> Result<Vec<(usize, u64, f64, f64)>> {
    let mut ictx = sc::input::demux(path)?;
    if let Some(keep) = only {
        sc::input::keep_only(&mut ictx, &[keep]);
    }
    let mut per: Vec<(usize, u64, f64, f64)> = Vec::new();
    for (stream, packet) in ictx.packets() {
        let tb = f64::from(stream.time_base());
        let t = packet.pts().map(|p| p as f64 * tb).unwrap_or(f64::NAN);
        match per.iter_mut().find(|e| e.0 == stream.index()) {
            Some(e) => {
                e.1 += 1;
                e.3 = t;
            }
            None => per.push((stream.index(), 1, t, t)),
        }
    }
    Ok(per)
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: demuxdiag <file>");
    sc::init()?;
    let probe = sc::input::demux(&path)?;
    println!("file    {path}");
    println!("format  {}", probe.format().name());
    let streams: Vec<(usize, String, String)> = probe
        .streams()
        .map(|s| {
            let p = s.parameters();
            (
                s.index(),
                format!("{:?}", p.medium()).to_lowercase(),
                format!("{:?}", p.id()).to_lowercase(),
            )
        })
        .collect();
    drop(probe);

    let together = count(&path, None)?;
    println!(
        "\n{:>6}  {:>10}  {:>10}  {:>9}  {:>9}  lost",
        "stream", "medium", "codec", "together", "alone"
    );
    let mut short: Vec<usize> = Vec::new();
    for (i, medium, codec) in &streams {
        let with = together.iter().find(|e| e.0 == *i).map_or(0, |e| e.1);
        let alone = count(&path, Some(*i))?
            .iter()
            .find(|e| e.0 == *i)
            .map_or(0, |e| e.1);
        let lost = alone.saturating_sub(with);
        if lost > 0 {
            short.push(*i);
        }
        println!(
            "{i:>6}  {medium:>10}  {codec:>10}  {with:>9}  {alone:>9}  {}",
            if lost == 0 {
                "-".to_string()
            } else {
                format!("{lost} ({:.1}%)", 100.0 * lost as f64 / alone.max(1) as f64)
            }
        );
    }

    let Some(&idx) = short.first() else {
        println!("\nevery stream hands over the same count either way.");
        return Ok(());
    };
    // Where it went, for the first stream that came up short. The runs are
    // what tells a cluster's worth from a frame's: one that ends on a key
    // picture is a cluster the read gave up on and resumed past.
    let mut ictx = sc::input::demux(&path)?;
    let tb = ictx
        .stream(idx)
        .map(|s| f64::from(s.time_base()))
        .unwrap_or(1.0);
    let mut times: Vec<(f64, bool)> = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if let Some(pts) = packet.pts() {
            times.push((pts as f64 * tb, packet.is_key()));
        }
    }
    let typical = {
        let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1].0 - w[0].0).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        gaps.get(gaps.len() / 2).copied().unwrap_or(0.0)
    };
    let runs: Vec<(f64, f64, bool)> = times
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 > typical * 1.5)
        .map(|w| (w[0].0, w[1].0, w[1].1))
        .collect();
    let on_key = runs.iter().filter(|r| r.2).count();
    println!(
        "\nstream {idx} arrives with {} run(s) missing, {on_key} of them ending on a key \
         picture -- a cluster the read gave up on. Typical spacing {typical:.4}s.",
        runs.len()
    );
    for (a, b, key) in runs.iter().take(12) {
        println!(
            "  {a:9.3} -> {b:9.3}  ({:.3}s){}",
            b - a,
            if *key { "  on a key picture" } else { "" }
        );
    }
    if runs.len() > 12 {
        println!("  ... and {} more", runs.len() - 12);
    }
    std::process::exit(1);
}
