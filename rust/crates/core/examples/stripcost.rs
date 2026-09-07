//! How much of a film strip refresh is answered out of memory, and what the
//! rest of it costs.
//!
//! The GUI's own arithmetic, off to one side: build the thumbnail track, cut
//! the reel into cells the way `gopCells` does, then apply `thumbs_now`'s two
//! tests -- "are the held pictures fine enough to answer with at all" and "is
//! the nearest one *the* picture asked for" -- and decode whatever is left
//! over, which is what the strip pays for on a redraw.
//!
//! `SC_MAXBYTES` overrides [`ThumbOptions::max_bytes`], which is how the old
//! 4000-picture cap is reproduced: on an eight-minute recording it worked out
//! at 1.2 MB of pictures, and a film got the same spacing the cap gave it.
//!
//!     stripcost <file> [span=6] [cells=11] [refreshes=20]

use anyhow::Result;
use smartcut_core as sc;

fn med(v: &[f64]) -> Option<f64> {
    sc::thumbs::median_gap(v)
}

/// `gopCells`, in Rust.
fn cells(pts: &[f64], o: f64, span: f64, vis: usize, slots: usize) -> Vec<f64> {
    let d = (span / vis as f64).max(1e-3);
    let n = pts.len();
    let mut i0 = 0;
    while i0 + 1 < n && pts[i0 + 1] <= o + 1e-9 {
        i0 += 1;
    }
    let half = slots / 2;
    let mut marks = vec![usize::MAX; slots];
    marks[half] = i0;
    let mut j = i0;
    for mark in marks[half + 1..].iter_mut() {
        let want = pts[j] + d;
        let mut m = j + 1;
        while m < n && pts[m] < want - 1e-9 {
            m += 1;
        }
        if m >= n {
            break;
        }
        if m - 1 > j && pts[m - 1] - pts[j] >= d / 2.0 && want - pts[m - 1] < pts[m] - want {
            m -= 1;
        }
        *mark = m;
        j = m;
    }
    let mut j = i0;
    for k in (0..half).rev() {
        let want = pts[j] - d;
        let mut m = j as i64 - 1;
        while m >= 0 && pts[m as usize] > want + 1e-9 {
            m -= 1;
        }
        if m < 0 {
            break;
        }
        let mut m = m as usize;
        if m + 1 < j && pts[j] - pts[m + 1] >= d / 2.0 && pts[m + 1] - want < want - pts[m] {
            m += 1;
        }
        marks[k] = m;
        j = m;
    }
    marks.into_iter().filter(|&m| m != usize::MAX).map(|m| pts[m]).collect()
}

fn to_neighbour(points: &[sc::AccessPoint], at: f64) -> f64 {
    let i = points.partition_point(|p| p.time < at - 1e-6);
    let after = points[i..].iter().find(|p| p.time > at + 1e-6).map(|p| p.time - at);
    let before = points[..i].last().map(|p| at - p.time);
    after.into_iter().chain(before).fold(f64::INFINITY, f64::min)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: stripcost <file> [span] [vis] [samples]");
    let span: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let vis: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(11);
    let samples: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);

    let src = sc::scan(path)?;
    let mut opts = sc::ThumbOptions::default();
    if let Ok(v) = std::env::var("SC_MAXBYTES") {
        opts.max_bytes = v.parse().unwrap();
    }
    let track = sc::thumbs::build(&src, &opts, None)?;
    let bytes: usize = track.thumbs.iter().map(|t| t.jpeg.len()).sum();
    println!(
        "{path}\n  dur {:.1}s  points {}  held {} ({:.1} MB)  interval {:.3}s",
        src.duration,
        src.points.len(),
        track.thumbs.len(),
        bytes as f64 / 1e6,
        track.interval,
    );

    let pts: Vec<f64> = src.points.iter().map(|p| p.time).collect();
    let fd = src.video.frame_duration();
    let (mut fell_back, mut with_holes, mut holes, mut total, mut ms) = (0, 0, 0usize, 0usize, 0.0);
    for s in 0..samples {
        let o = src.duration * (s as f64 + 0.5) / samples as f64;
        let cs = cells(&pts, o, span, vis, vis + 1);
        if cs.len() < 2 {
            continue;
        }
        total += cs.len();
        let gaps: Vec<f64> =
            cs.windows(2).map(|w| (w[1] - w[0]).abs()).filter(|d| *d > 1e-9).collect();
        let gap = med(&gaps).unwrap_or(f64::INFINITY);
        // Weighed against the held pictures over the stretch the cells cover,
        // the way `thumbs_now` weighs it.
        let held = track.spacing_over(cs[0], cs[cs.len() - 1]).unwrap_or(track.interval);
        let want: Vec<f64> = if gap < held * 0.9 {
            fell_back += 1;
            cs.clone()
        } else {
            cs.iter()
                .copied()
                .filter(|&t| {
                    let tol = (fd * 2.0).min(to_neighbour(&src.points, t) / 2.0);
                    !track.nearest(t).is_some_and(|h| (h.time - t).abs() <= tol)
                })
                .collect()
        };
        holes += want.len();
        if !want.is_empty() {
            with_holes += 1;
            let t0 = std::time::Instant::now();
            let _ = sc::shots_at(&src, &want, 200)?;
            ms += t0.elapsed().as_secs_f64() * 1000.0;
        }
    }
    println!(
        "  {samples} refreshes: {fell_back} fell back whole, {with_holes} had to decode at all\n  \
         cells decoded {holes}/{total} ({}%)   decoding cost {:.0} ms a refresh (mean over all {samples})",
        holes * 100 / total.max(1),
        ms / samples as f64,
    );
    Ok(())
}
