//! What [`sc::thumbs::cut_near`] is deciding between at a real boundary.
//!
//! A commercial block arrives timed by an estimate -- the middle of a
//! silence, a caption reset, the moment a logo's rolling average crossed --
//! and is then put on the picture the cut happens on. The report behind this
//! was that the picture it lands on is frequently no cut at all: a frame in
//! the middle of a shot.
//!
//!     cutdiag <file> [window]
//!
//! This runs the detection the window runs, and for each boundary prints the
//! estimate, where the rule put it, and what the window it looked at actually
//! held -- so that a boundary the rule could not move is told apart from one
//! it moved to the wrong place.
use anyhow::Result;
use smartcut_core as sc;

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// The detection the cut editor makes, in the same order of preference.
fn blocks_of(src: &sc::Source) -> (Vec<sc::cm::Block>, &'static str) {
    let opts = sc::DetectOptions::default();
    let resets = sc::caption::resets_with(src, None)
        .ok()
        .filter(|r| sc::cm_marks_every_junction(r));
    let silences = match &resets {
        Some(_) => Vec::new(),
        None => sc::find_silences_with(src, &opts, None).unwrap_or_default(),
    };
    let cands = sc::cm_candidates(&silences, &opts);
    let logo = if resets.is_none() {
        sc::logo::detect_with(src, &Default::default(), None).ok()
    } else {
        None
    };
    match (&resets, &logo) {
        (Some(r), _) => (sc::cm_blocks_from_resets(r, src.duration), "resets"),
        (None, Some(l)) if !l.absent.is_empty() => (
            sc::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration),
            "logo+silence",
        ),
        (None, Some(_)) => (Vec::new(), "logo never absent"),
        _ => (sc::cm_blocks(&cands, &opts, 0.6), "silence"),
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: cutdiag <file> [window]");
    let window: f64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.5);
    const FLOOR: f64 = 0.08;

    let src = sc::scan(&path)?;
    let (blocks, how) = blocks_of(&src);
    println!(
        "{}  {:.0}s  {} blocks ({how})",
        path.rsplit('/').next().unwrap_or(&path),
        src.duration,
        blocks.len()
    );
    println!("   estimate     moved to     shift   window: median    max  (over floor)");
    let mut stayed = 0usize;
    let mut edges = 0usize;
    for b in &blocks {
        for (which, at) in [("start", b.start), ("end", b.end)] {
            if at <= 0.0 || at >= src.duration {
                continue;
            }
            edges += 1;
            let seen = sc::thumbs::differences_near(&src, at, window)?;
            let diffs: Vec<f64> = seen.iter().map(|d| d.1).collect();
            let max = diffs.iter().copied().fold(0.0, f64::max);
            let over = diffs.iter().filter(|&&d| d >= FLOOR).count();
            let to = sc::thumbs::cut_near(&src, at, window, FLOOR)?;
            let moved = (to - at).abs() > 1e-9;
            if !moved {
                stayed += 1;
            }
            println!(
                "  {at:9.3} {} {to:9.3}  {:+6.3}   {:.4}  {:.4}  {over:3}   {}",
                if moved { "->" } else { "  " },
                to - at,
                median(&diffs),
                max,
                which
            );
        }
    }
    println!("\n  {stayed} of {edges} boundaries had no cut in reach and kept their estimate");
    Ok(())
}
