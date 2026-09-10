//! Does one read of the recording give the same answer as two, and faster?
//!
//! The two passes -- `scan` walking every packet, then `build_with` walking
//! them all again to decode the key ones -- against `scan_with_pictures`,
//! which does both in one read. The pictures have to come back byte for byte
//! identical: it is the same decoder fed the same packets, and anything else
//! would mean the one-pass path is not the same answer arrived at cheaper.
//!
//!   mergediag <file> two     the two passes, as the list does them locally
//!   mergediag <file> one     the same work in one read
//!   mergediag <file> both    both, and a comparison (the second is warm)
//!
//! Compare the timings across separate cold runs; `both` is for the identity
//! check, where a warm cache does not matter.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: mergediag <file> two|one|both");
    let how = std::env::args().nth(2).unwrap_or_else(|| "both".into());
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1e9;
    let opts = sc::ThumbOptions::default();
    let per = |s: f64| s / bytes.max(1e-9);

    let mut two: Option<(sc::Source, sc::Track)> = None;
    let mut one: Option<(sc::Source, sc::Track)> = None;

    if how != "one" {
        let began = std::time::Instant::now();
        let src = sc::scan(&path)?;
        let walk = began.elapsed().as_secs_f64();
        let track = sc::thumbs::build_with(&src, &opts, None, None, None)?;
        let all = began.elapsed().as_secs_f64();
        println!(
            "two passes  {all:6.2}s  ({:.2} s/GB)   walk {walk:.2}s + pictures {:.2}s",
            per(all),
            all - walk
        );
        two = Some((src, track));
    }
    if how != "two" {
        let began = std::time::Instant::now();
        let (src, track) = sc::scan_with_pictures(&path, &opts, None, None)?;
        let all = began.elapsed().as_secs_f64();
        println!("one pass    {all:6.2}s  ({:.2} s/GB)", per(all));
        one = Some((src, track));
    }

    if let (Some((asrc, atrack)), Some((bsrc, btrack))) = (&two, &one) {
        let points = asrc.points.len() == bsrc.points.len()
            && asrc
                .points
                .iter()
                .zip(&bsrc.points)
                .all(|(x, y)| x.time == y.time && x.droppable == y.droppable && x.pos == y.pos);
        let same_pictures = atrack.thumbs.len() == btrack.thumbs.len()
            && atrack
                .thumbs
                .iter()
                .zip(&btrack.thumbs)
                .all(|(x, y)| x.time == y.time && x.jpeg == y.jpeg);
        println!(
            "\n  points     {}  ({} vs {})",
            if points { "identical" } else { "DIFFER" },
            asrc.points.len(),
            bsrc.points.len()
        );
        println!(
            "  duration   {}  ({:.3} vs {:.3})",
            if (asrc.duration - bsrc.duration).abs() < 1e-6 {
                "identical"
            } else {
                "DIFFER"
            },
            asrc.duration,
            bsrc.duration
        );
        println!(
            "  pulldown   {}  ({} vs {})",
            if asrc.video.pulldown == bsrc.video.pulldown {
                "identical"
            } else {
                "DIFFER"
            },
            asrc.video.pulldown,
            bsrc.video.pulldown
        );
        println!(
            "  pictures   {}  ({} vs {})",
            if same_pictures { "identical" } else { "DIFFER" },
            atrack.thumbs.len(),
            btrack.thumbs.len()
        );
        println!(
            "  scenes     {}  ({} vs {})",
            if atrack.scenes == btrack.scenes {
                "identical"
            } else {
                "DIFFER"
            },
            atrack.scenes.len(),
            btrack.scenes.len()
        );
        if !(points && same_pictures && atrack.scenes == btrack.scenes) {
            std::process::exit(1);
        }
    }
    Ok(())
}
