//! What a cached seek index actually holds, and what the film strip would
//! get out of it.
//!
//! `stripcost` answers the same question from the recording, which means
//! reading the whole of it -- eight minutes for a 22 GB disc title. This
//! reads the index the last open left behind instead, so the answer for the
//! material somebody actually opened is a second away.
//!
//!     scixdiag <index.scix> [span=6] [vis=15] [samples=40] [fps=23.976]
use anyhow::Result;
use smartcut_core as sc;

/// `gopCells`, in Rust -- the cell start times a reel is cut on.
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

fn to_neighbour(pts: &[f64], at: f64) -> f64 {
    let i = pts.partition_point(|&t| t < at - 1e-6);
    let after = pts[i..].iter().find(|&&t| t > at + 1e-6).map(|&t| t - at);
    let before = pts[..i].last().map(|&t| at - t);
    after.into_iter().chain(before).fold(f64::INFINITY, f64::min)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: scixdiag <index.scix> [span] [vis] [samples] [fps]");
    let span: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let vis: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
    let samples: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);
    let fps: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(23.976);
    let fd = 1.0 / fps;

    let ix = sc::seek_index::SeekIndex::load(std::path::Path::new(path))?;
    let pts: Vec<f64> = ix.points.iter().map(|p| p.time).collect();
    let dur = ix.end.or_else(|| pts.last().copied()).unwrap_or(0.0);
    let gaps: Vec<f64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    println!(
        "{path}\n  dur {dur:.1}s  points {}  median gap {:.3}s  (min {:.3} max {:.3})",
        pts.len(),
        sc::thumbs::median_gap(&gaps).unwrap_or(f64::NAN),
        gaps.iter().copied().fold(f64::INFINITY, f64::min),
        gaps.iter().copied().fold(0.0, f64::max),
    );
    let Some(track) = ix.track.as_ref() else {
        println!("  no track");
        return Ok(());
    };
    let bytes: usize = track.thumbs.iter().map(|t| t.jpeg.len()).sum();
    println!(
        "  held {} ({:.1} MB, {} B each)  interval {:.3}s  covered {:.1}s  scenes {}",
        track.thumbs.len(),
        bytes as f64 / 1e6,
        bytes / track.thumbs.len().max(1),
        track.interval,
        track.covered,
        track.scenes.len(),
    );

    let (mut fell_back, mut holes, mut total) = (0, 0usize, 0usize);
    for s in 0..samples {
        let o = dur * (s as f64 + 0.5) / samples as f64;
        let cs = cells(&pts, o, span, vis, vis * 2 + 1);
        if cs.len() < 2 {
            continue;
        }
        total += cs.len();
        let gaps: Vec<f64> =
            cs.windows(2).map(|w| (w[1] - w[0]).abs()).filter(|d| *d > 1e-9).collect();
        let gap = sc::thumbs::median_gap(&gaps).unwrap_or(f64::INFINITY);
        let against = track.spacing_over(cs[0], cs[cs.len() - 1]).unwrap_or(track.interval);
        if gap < against * 0.9 {
            fell_back += 1;
            holes += cs.len();
            continue;
        }
        holes += cs
            .iter()
            .filter(|&&t| {
                let tol = (fd * 2.0).min(to_neighbour(&pts, t) / 2.0);
                !track.nearest(t).is_some_and(|h| (h.time - t).abs() <= tol)
            })
            .count();
    }
    println!(
        "  span {span}s / {vis} cells: {fell_back} of {samples} refreshes fall back whole, \
         cells to decode {holes}/{total} ({}%)",
        holes * 100 / total.max(1),
    );
    Ok(())
}
