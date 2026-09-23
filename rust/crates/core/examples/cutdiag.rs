//! What [`sc::thumbs::cut_near`] is deciding between at a real boundary.
//!
//! A commercial block arrives timed by an estimate -- the middle of a
//! silence, a caption reset, the moment a logo's rolling average crossed --
//! and is then put on the picture the cut happens on. The report behind this
//! was that the picture it lands on is frequently no cut at all: a frame in
//! the middle of a shot. Which is what the rule answers when it finds no cut
//! within reach, so what has to be measured is how often that happens and
//! how far away the cut was.
//!
//!     cutdiag <file> [reach]
//!
//! One line per boundary, tab separated, for a survey to add up:
//!
//!     how  at  from-junction  over-floor-in-half  shift-in-half  nearest-cut  its-distance
use anyhow::Result;
use smartcut_core as sc;

/// How far out the cut is looked for when reporting, which is further than
/// the rule reaches: the question is what the rule is missing.
const REACH: f64 = 2.5;
/// The rule's own window and floor, as `cm::refine_boundaries` passes them.
const WINDOW: f64 = 0.5;
const FLOOR: f64 = 0.08;

/// The detection the cut editor makes, in the same order of preference.
fn blocks_of(src: &sc::Source) -> (Vec<sc::cm::Block>, Vec<sc::cm::Candidate>, &'static str) {
    let opts = sc::DetectOptions::default();
    let (silences, found) = sc::cm_silences_and_resets(src, &opts, None)
        .unwrap_or_else(|_| (Vec::new(), sc::caption::resets(src)));
    let resets = found.ok().filter(|r| sc::cm_marks_every_junction(r));
    let cands = sc::cm_candidates(&silences, &opts);
    let logo = if resets.is_none() {
        sc::logo::detect_with(src, &Default::default(), None).ok()
    } else {
        None
    };
    let blocks = match (&resets, &logo) {
        (Some(r), _) => (sc::cm_blocks_from_resets(r, src.duration), "resets"),
        (None, Some(l)) if !l.absent.is_empty() => (
            sc::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration, Some(src)),
            "logo+silence",
        ),
        (None, Some(_)) => (Vec::new(), "logo never absent"),
        _ => (sc::cm_blocks(&cands, &opts, 0.6), "silence"),
    };
    (blocks.0, cands, blocks.1)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: cutdiag <file> [reach]");
    let reach: f64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(REACH);

    let src = sc::scan(&path)?;
    let (blocks, cands, how) = blocks_of(&src);
    let name = path.rsplit('/').next().unwrap_or(&path);
    println!("# {name}\t{:.0}s\t{}blocks\t{how}", src.duration, blocks.len());
    // Where a silence says a junction is. A boundary standing on one is timed
    // by a reading of the recording; one standing nowhere near is a logo edge
    // or a grid position carried on from the last junction, and neither of
    // those is a picture.
    let junctions: Vec<f64> = cands.iter().filter(|c| c.score >= 0.6).map(|c| c.time).collect();
    for b in &blocks {
        for (which, at) in [("start", b.start), ("end", b.end)] {
            if at <= 0.0 || at >= src.duration {
                continue;
            }
            let seen = sc::thumbs::differences_near(&src, at, reach)?;
            let near = |w: f64| -> Option<(f64, f64)> {
                seen.iter()
                    .filter(|&&(t, d)| d >= FLOOR && (t - at).abs() <= w)
                    .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()))
                    .copied()
            };
            let inside = near(WINDOW);
            let out = near(reach);
            let from_junction = junctions
                .iter()
                .map(|j| (j - at).abs())
                .fold(f64::INFINITY, f64::min);
            println!(
                "{name}\t{which}\t{at:.3}\t{from_junction:.3}\t{}\t{}\t{}\t{}",
                seen.iter().filter(|&&(t, d)| d >= FLOOR && (t - at).abs() <= WINDOW).count(),
                inside.map_or("-".into(), |(t, _)| format!("{:+.3}", t - at)),
                out.map_or("-".into(), |(_, d)| format!("{d:.3}")),
                out.map_or("-".into(), |(t, _)| format!("{:+.3}", t - at)),
            );
        }
    }
    Ok(())
}
