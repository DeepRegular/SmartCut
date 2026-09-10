//! What the film strip can show before the walk has been over the recording,
//! and what showing it costs.
//!
//! The editor draws a reel while the walk is still running: no access points,
//! so the cells are cut on an even grid and each is filled by asking
//! libavformat for its own seek. A landing is the entry point at or before the
//! instant asked for, so it usually belongs to the cell before the one that
//! asked -- which is why the window puts each picture in the cell it landed
//! in. What is left over is how much time a cell covers, and that is what the
//! strip's own menu picks.
//!
//! Prints, for each of the menu's spans: how many cells filled under either
//! rule -- "the cell that asked for it keeps it" and "whichever cell it landed
//! in keeps it" -- and, of the ones that did not, why. `off` is a landing
//! outside the drawn reel, which is the leftmost cell's own picture falling a
//! GOP short of it; `share` is two landings in one cell, which is what leaves
//! a gap in the middle of the strip; `none` is a time nothing decoded near.
//! Then the same reel filled by [`glance_sweep`] instead -- one seek and a
//! read through the whole stretch, which finds every entry point in it -- and
//! what each way costs. The last column is the seek-per-cell refresh paid for
//! an open apiece, which is what [`glance_at`] per cell would have cost.
//!
//!     glancecost <file> [cells=11] [refreshes=3]

use std::time::Instant;

use anyhow::Result;
use smartcut_core as sc;

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// `evenCells`: the grid the reel is cut on when there are no access points to
/// cut it on. Aligned to the recording's own clock rather than to the
/// playhead, which is what lets a scrub back and forth reuse what it asked
/// for. Answers with each cell's start.
fn even_cells(o: f64, step: f64, slots: usize, dur: f64) -> Vec<f64> {
    let i0 = (o / step).floor();
    let half = (slots / 2) as f64;
    (0..slots)
        .map(|k| (i0 + k as f64 - half) * step)
        .filter(|&a| a >= 0.0 && a < dur)
        .collect()
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: glancecost <file> [cells] [refreshes]");
    let cells: usize = args.next().map_or(11, |s| s.parse().unwrap());
    let refreshes: usize = args.next().map_or(3, |s| s.parse().unwrap());

    let o = sc::outline(&path)?;
    println!(
        "{path}\n  {} {}x{} {:.3} fps, {:.1}s",
        o.video.codec, o.video.width, o.video.height, o.video.frame_rate, o.duration
    );
    // Two tables rather than one wide one: what fills the reel, and what that
    // costs.
    let mut fills = String::new();
    let mut costs = String::new();

    for span in [3.0, 6.0, 30.0, 180.0] {
        let step = span / cells as f64;
        let mut inside = 0usize;
        let mut landed = 0usize;
        let mut asked = 0usize;
        // Why a landing filled no cell.
        let (mut off, mut share, mut none) = (0usize, 0usize, 0usize);
        // The same reel, filled by reading the stretch through instead.
        let mut swept = 0usize;
        let mut sweep = 0.0;
        let mut err: Vec<f64> = Vec::new();
        let mut run = 0.0;
        let mut apiece = 0.0;
        for r in 0..refreshes {
            // Spread the refreshes over the recording rather than repeating
            // one: the seek is cheap where the container's index is dense and
            // dear where it is not, and both are the strip's problem.
            let at = o.duration * (r as f64 + 0.5) / refreshes as f64;
            let starts = even_cells(at, step, cells, o.duration);
            // The middle of the cell, so that the ground a landing may fall on
            // and be kept is the cell itself, either side of the mark.
            let times: Vec<f64> = starts.iter().map(|a| a + step / 2.0).collect();

            let t0 = Instant::now();
            let shots = sc::glance_run(&path, &times, 200)?;
            run += t0.elapsed().as_secs_f64();

            // Whichever cell the landing fell in keeps it, so long as no
            // earlier landing has already taken that cell.
            let mut taken = vec![false; starts.len()];
            for (i, shot) in shots.iter().enumerate() {
                asked += 1;
                let Some(s) = shot else {
                    none += 1;
                    continue;
                };
                err.push((s.time - times[i]).abs());
                if s.time >= starts[i] && s.time < starts[i] + step {
                    inside += 1;
                }
                let Some(k) = starts
                    .iter()
                    .position(|&a| s.time >= a && s.time < a + step)
                else {
                    off += 1;
                    continue;
                };
                if taken[k] {
                    share += 1;
                    continue;
                }
                taken[k] = true;
                landed += 1;
            }

            // The same cells, filled from one read of the stretch the reel
            // covers. Every entry point in it comes back, so a cell is empty
            // only where there is genuinely nothing to put in it.
            let (a, b) = (starts[0], starts[starts.len() - 1] + step);
            let t0 = Instant::now();
            // One answer per cell -- a second picture inside one cell has
            // nowhere to go -- with a ceiling well above the cell count, since
            // the thinning is by cell and not by count.
            let all = sc::glance_sweep(&path, a, b, 200, step, cells * 3)?;
            sweep += t0.elapsed().as_secs_f64();
            let mut held = vec![false; starts.len()];
            for shot in &all {
                let Some(k) = starts
                    .iter()
                    .position(|&a| shot.time >= a && shot.time < a + step)
                else {
                    continue;
                };
                if !held[k] {
                    held[k] = true;
                    swept += 1;
                }
            }

            // What the same refresh costs without the shared open. Measured
            // once, on the first refresh: it is the same answer every time and
            // it is the dear one.
            if r == 0 {
                let t0 = Instant::now();
                for &t in &times {
                    let _ = sc::glance_at(&path, t, 200);
                }
                apiece = t0.elapsed().as_secs_f64();
            }
        }
        let worst = err.iter().cloned().fold(0.0, f64::max);
        let pc = |n: usize| n * 100 / asked.max(1);
        fills.push_str(&format!(
            "{span:>6.0}s {:>7.2}s {:>6}% {:>6}% {:>5}% {:>5}% {:>5}% {:>6}%\n",
            step,
            pc(inside),
            pc(landed),
            pc(off),
            pc(share),
            pc(none),
            pc(swept),
        ));
        costs.push_str(&format!(
            "{span:>6.0}s {:>7.2}s {:>7.2}s {:>8.0}ms {:>8.0}ms {:>8.0}ms\n",
            median(&mut err),
            worst,
            run * 1000.0 / refreshes as f64,
            sweep * 1000.0 / refreshes as f64,
            apiece * 1000.0,
        ));
    }

    println!(
        "\n{:>7} {:>8} {:>7} {:>7} {:>6} {:>6} {:>6} {:>7}\n{fills}",
        "span", "cell", "asked", "landed", "off", "share", "none", "swept"
    );
    println!(
        "{:>7} {:>8} {:>8} {:>9} {:>9} {:>9}\n{costs}",
        "span", "median", "worst", "seeks", "sweep", "one by one"
    );

    // What the walk says is there, which is what all of the above is an
    // approximation of -- and the only way to tell "the sweep missed a
    // picture" from "there was no picture to find". Behind a switch because
    // it reads the whole recording, and everything above this is about not
    // having to.
    if std::env::var("SC_WALK").is_ok() {
        let src = sc::scan(&path)?;
        let pts: Vec<f64> = src.points.iter().map(|p| p.time).collect();
        let mut gaps: Vec<f64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
        let widest = gaps.iter().cloned().fold(0.0, f64::max);
        println!(
            "the walk: {} entry points, {:.3}s apart at the median, {:.3}s at the widest",
            pts.len(),
            median(&mut gaps),
            widest
        );
        // And over one reel: what is really there against what a read through
        // it came back with.
        for span in [6.0, 30.0] {
            let step = span / cells as f64;
            let at = o.duration / 2.0;
            let starts = even_cells(at, step, cells, o.duration);
            let (a, b) = (starts[0], starts[starts.len() - 1] + step);
            let there = pts.iter().filter(|&&t| t >= a && t < b).count();
            let found = sc::glance_sweep(&path, a, b, 200, step, cells * 3)?.len();
            let cellsful = starts
                .iter()
                .filter(|&&c| pts.iter().any(|&t| t >= c && t < c + step))
                .count();
            println!(
                "  {span:>4.0}s reel {a:.1}-{b:.1}s: {there} entry points in it, {cellsful} of \
                 {} cells have one, the sweep came back with {found}",
                starts.len()
            );
        }
    }
    Ok(())
}
