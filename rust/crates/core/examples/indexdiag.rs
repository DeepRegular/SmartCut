//! Which index source answers for a recording, and whether it should have.
//!
//! The editor asks three in turn -- the disc's own map, the container's seek
//! table, the walk over the packets -- and takes the first that answers. A
//! wrong answer from an early one is not an error anywhere: the recording
//! opens, and the film strip is drawn from whatever entry points it was
//! handed. So this asks each of them separately and prints what each says,
//! against the walk, which is the answer they are all approximating.
//!
//!     indexdiag <file> [--walk]
use anyhow::Result;
use smartcut_core as sc;

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
    report("disc map", &sc::scan_with(&path, &sc::DiscIndex));
    report(
        "container table",
        &sc::scan_with(&path, &sc::ContainerIndex),
    );
    if walk {
        report("packet walk", &sc::scan_with(&path, &sc::PacketScan));
    }
    Ok(())
}
