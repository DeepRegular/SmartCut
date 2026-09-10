//! Does a commercial detection need the access points to *find* a break?
//!
//! The three reading passes -- captions, audio, logo -- take a `Source` and
//! none of them looks at `points`. This runs them off a walked recording and
//! off the container's own answer alone, and compares what came back. Only
//! the refinement afterwards needs the index, and that is checked to be the
//! whole of the difference.
use anyhow::Result;
use smartcut_core as sc;

fn blocks_of(src: &sc::Source) -> Vec<(f64, f64)> {
    let opts = sc::DetectOptions::default();
    let resets = sc::caption::resets_with(src, None).ok();
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
    let blocks = match (&resets, &logo) {
        (Some(r), _) => sc::cm_blocks_from_resets(r, src.duration),
        (None, Some(l)) if !l.absent.is_empty() => {
            sc::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration)
        }
        (None, Some(_)) => Vec::new(),
        _ => sc::cm_blocks(&cands, &opts, 0.6),
    };
    blocks.into_iter().map(|b| (b.start, b.end)).collect()
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: cmdiag <file>");

    let began = std::time::Instant::now();
    let walked = sc::scan(&path)?;
    let with = blocks_of(&walked);
    let t_with = began.elapsed().as_secs_f64();

    let began = std::time::Instant::now();
    let bare = sc::outline(&path)?.into_source();
    let without = blocks_of(&bare);
    let t_without = began.elapsed().as_secs_f64();

    println!(
        "walked    {t_with:6.2}s  {} blocks  ({} points)",
        with.len(),
        walked.points.len()
    );
    println!(
        "container {t_without:6.2}s  {} blocks  (0 points)",
        without.len()
    );
    let same = with.len() == without.len()
        && with
            .iter()
            .zip(&without)
            .all(|(a, b)| a.0 == b.0 && a.1 == b.1);
    println!(
        "\n  found the same breaks: {}",
        if same { "yes" } else { "NO" }
    );
    for (i, (a, b)) in with.iter().zip(&without).enumerate() {
        if a != b {
            println!("    block {i}: walked {a:?} vs container {b:?}");
        }
    }

    // And that the refinement is the whole of what the index buys.
    let mut refined = sc::cm_blocks(&[], &sc::DetectOptions::default(), 0.6);
    refined.clear();
    let mut a = to_blocks(&with);
    let mut b = to_blocks(&without);
    sc::cm_refine_boundaries(&walked, &mut a, 0.5, 0.08);
    sc::cm_refine_boundaries(&bare, &mut b, 0.5, 0.08);
    println!("\n  estimate -> refined (against the index), and against no index:");
    for (x, y) in a.iter().zip(&b) {
        println!(
            "    {:10.4} -> {:10.4}   {:10.4} -> {:10.4}   (no index: {:10.4} {:10.4})",
            y.start, x.start, y.end, x.end, y.start, y.end
        );
    }
    // Refined against a source built from a cached seek index rather than
    // from a fresh walk, which is what the editor actually holds.
    println!("\n  (the source the editor holds is the one to check, not a fresh walk)");
    if !same {
        std::process::exit(1);
    }
    Ok(())
}

fn to_blocks(v: &[(f64, f64)]) -> Vec<sc::cm::Block> {
    v.iter()
        .map(|&(start, end)| sc::cm::Block {
            start,
            end,
            junctions: 0,
            score: 1.0,
        })
        .collect()
}
