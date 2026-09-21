//! Which index source answers for a recording, and whether it should have.
//!
//! The editor asks three in turn -- the disc's own map, the container's seek
//! table, the walk over the packets -- and takes the first that answers. A
//! wrong answer from an early one is not an error anywhere: the recording
//! opens, and the film strip is drawn from whatever entry points it was
//! handed. So this asks each of them separately and prints what each says,
//! against the walk, which is the answer they are all approximating.
//!
//! With `--walk` it reads the recording as well and holds the others up
//! against it, picture by picture: a source that answers has to answer with
//! the instants the *pictures* are shown at, and a table counted in decode
//! order does not. The exit status says whether they did, so a test can ask.
//!
//!     indexdiag <file> [--walk]
use anyhow::Result;
use smartcut_core as sc;

/// Does what a source said stand on the pictures the walk found?
///
/// Every point it offered, against the nearest the walk has. Half a frame is
/// the bar: an entry point is a picture, and a source naming an instant
/// between two of them is naming neither.
fn agrees(name: &str, src: &sc::Source, walk: &sc::Source) -> bool {
    let times: Vec<f64> = walk.points.iter().map(|p| p.time).collect();
    let fd = walk.video.frame_duration();
    let (mut worst, mut at) = (0.0, f64::NAN);
    for p in &src.points {
        let i = times.partition_point(|&t| t < p.time);
        let off = [i.wrapping_sub(1), i]
            .iter()
            .filter_map(|&j| times.get(j))
            .map(|&t| (t - p.time).abs())
            .fold(f64::INFINITY, f64::min);
        if off > worst {
            worst = off;
            at = p.time;
        }
    }
    let ok = worst <= fd / 2.0;
    println!(
        "{name:22} {} the walk's pictures: worst {worst:.4}s at {at:.3}s, half a frame is \
         {:.4}s",
        if ok { "stands on" } else { "stands off" },
        fd / 2.0,
    );
    ok
}

fn report(name: &str, src: &Result<sc::Source>) {
    match src {
        Ok(src) => {
            let pts: Vec<f64> = src.points.iter().map(|p| p.time).collect();
            let gaps: Vec<f64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
            println!(
                "{name:22} {:6} points  first {:8.3}  last {:9.3}  of {:9.3}\n\
                 {:22} gap median {:8.3}  largest {:9.3}",
                pts.len(),
                pts.first().copied().unwrap_or(f64::NAN),
                pts.last().copied().unwrap_or(f64::NAN),
                src.duration,
                "",
                sc::thumbs::median_gap(&gaps).unwrap_or(f64::NAN),
                gaps.iter().copied().fold(0.0, f64::max),
            );
        }
        Err(e) => println!("{name:22} declined: {e}"),
    }
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: indexdiag <file>");
    let walk = std::env::args().any(|a| a == "--walk");
    let disc = sc::scan_with(&path, &sc::DiscIndex);
    report("disc map", &disc);
    let container = sc::scan_with(&path, &sc::ContainerIndex);
    report("container table", &container);
    if !walk {
        return Ok(());
    }
    let walked = sc::scan_with(&path, &sc::PacketScan);
    report("packet walk", &walked);
    let Ok(walked) = walked else { return Ok(()) };
    let stands = [("disc map", disc), ("container table", container)]
        .iter()
        .filter_map(|(name, src)| src.as_ref().ok().map(|src| agrees(name, src, &walked)))
        .all(|ok| ok);
    if !stands {
        std::process::exit(1);
    }
    Ok(())
}
