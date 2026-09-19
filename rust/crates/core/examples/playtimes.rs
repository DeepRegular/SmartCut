//! Which pictures playback puts on the stage, against which ones the cut
//! writes, for the same edit.
//!
//!     playtimes <recording> <a-b> [<a-b> ...]
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a recording");
    let ranges: Vec<(f64, f64)> = args
        .map(|r| {
            let (a, b) = r.split_once('-').expect("a-b");
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();
    let src = sc::scan(&path)?;
    let fd = src.video.frame_duration();
    // The window's own pacing, with the sleeping left out: a picture is
    // passed over when one has already been shown within a frame of the
    // moment this one is due. See `play` in the GUI, which is where the rule
    // and the two numbers below come from.
    let gap = fd;
    let mut next_show = 0.0f64;
    let mut elapsed_out = 0.0f64;
    let mut stage: Vec<(f64, f64)> = Vec::new();
    for (a, b) in &ranges {
        let base = elapsed_out;
        sc::play_from(&src, *a, *b, 320, |t| {
            let out_t = (base + t - *a).max(0.0);
            if out_t + gap / 2.0 < next_show {
                return sc::Pace::Skip;
            }
            next_show = out_t + gap;
            sc::Pace::Show
        }, |t, _| stage.push((elapsed_out + t - *a, t)))?;
        elapsed_out += b - a;
    }
    println!("what the stage actually gets, played straight through:");
    for (out_t, t) in &stage {
        let seam = ranges.iter().any(|(_, b)| (t - b).abs() < 1e-9);
        println!(
            "  out {out_t:8.4}  source {t:8.4}{}",
            if seam { "   <- past the end of a range" } else { "" }
        );
    }
    println!();
    for (a, b) in &ranges {
        let mut shown = Vec::new();
        sc::play_from(
            &src,
            *a,
            *b,
            320,
            |_| sc::Pace::Show,
            |t, _| shown.push(t),
        )?;
        // What the cut covers, which is the plan's own bounds.
        let plan = sc::plan::plan(
            &src.video,
            src.duration,
            &src.points,
            &[(*a, *b)],
            &sc::PlanOptions::default(),
        );
        let (t_in, t_out) = (plan[0].t_in, plan[0].t_out);
        let written: Vec<f64> = shown
            .iter()
            .copied()
            .filter(|t| *t >= t_in - 1e-9 && *t < t_out - 1e-9)
            .collect();
        println!("range {a:.4}-{b:.4}  plan covers {t_in:.4}..{t_out:.4}  (frame {fd:.4}s)");
        println!(
            "  playback shows {} picture(s), {:.4} .. {:.4}",
            shown.len(),
            shown.first().copied().unwrap_or(f64::NAN),
            shown.last().copied().unwrap_or(f64::NAN)
        );
        println!(
            "  the cut writes {} picture(s), {:.4} .. {:.4}",
            written.len(),
            written.first().copied().unwrap_or(f64::NAN),
            written.last().copied().unwrap_or(f64::NAN)
        );
        for t in &shown {
            if *t < t_in - 1e-9 {
                println!("  shown but not written (before the start): {t:.4}");
            }
            if *t >= t_out - 1e-9 {
                println!("  shown but not written (past the end):     {t:.4}");
            }
        }
    }
    Ok(())
}
