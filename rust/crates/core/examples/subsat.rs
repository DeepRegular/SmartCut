//! Ask the preview's subtitle reader what is on screen, the way the editor
//! asks it: one track, a run of instants.
//!
//! usage: subsat <file> [from] [to] [step]

use anyhow::{anyhow, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or_else(|| anyhow!("usage: subsat <file> [from] [to] [step]"))?;
    let num = |i: usize, d: f64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let (from, to, step) = (num(2, 0.0), num(3, 600.0), num(4, 1.0));
    // A step of nothing, or backwards, never reaches `to`.
    if step.is_nan() || step <= 0.0 {
        return Err(anyhow!("the step has to be more than zero"));
    }
    let src = smartcut_core::scan(path)?;
    let tracks = smartcut_core::subs::tracks(&src.captions, &src.graphics, &src.subpictures);
    for track in tracks {
        let mut reader = smartcut_core::subs::Reader::open(&src, track.id)?;
        let mut shown = 0;
        let mut t = from;
        while t < to {
            if let Some(s) = reader.at(t)? {
                shown += 1;
                if let smartcut_core::subs::Shown::Picture { x, y, width, height, png, .. } = s {
                    if shown <= 5 {
                        println!("  {t:8.1}s  {width}x{height} at {x},{y}");
                        if let Ok(dir) = std::env::var("SUBSAT_PNG") {
                            std::fs::write(format!("{dir}/{:#x}-{t:.0}.png", track.id), png)?;
                        }
                    }
                }
            }
            // A step too small to move `t` -- or a `from` of minus infinity --
            // would not reach `to` either.
            let next = t + step;
            if next <= t {
                return Err(anyhow!("a step of {step} does not move on from {t}"));
            }
            t = next;
        }
        println!("track {:#x} {:?} {:?}: shown at {shown} instants", track.id, track.kind, track.language);
    }
    Ok(())
}
