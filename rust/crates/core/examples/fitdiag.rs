//! What a night's cuts are expected to come to, and what a disc would ask of
//! them.
//!
//! The estimate this prints is the one the output settings screen draws its
//! graph from, and it is built out of rates rather than out of the file: how
//! many bits a second the pictures take, measured while the recording was
//! indexed, times how much of it is being kept. Pointed at whole recordings
//! with nothing cut out of them, what it says should be the size of the file
//! -- which is what this is for.
//!
//!     fitdiag <file>... [--disc 25|50|100|128] [--margin 0.01]
use anyhow::Result;
use smartcut_core as sc;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> Result<()> {
    let disc: u64 = arg("--disc").map_or(25, |v| v.parse().unwrap_or(25));
    let margin: f64 = arg("--margin").map_or(sc::fit::MARGIN, |v| v.parse().unwrap_or(0.01));
    let capacity = sc::fit::DISCS
        .iter()
        .find(|d| d.bytes / 1_000_000_000 == disc)
        .map_or(sc::fit::DISCS[0], |d| *d);

    let mut files: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a.starts_with("--") {
            args.next();
        } else {
            files.push(a);
        }
    }

    let mut estimates = Vec::new();
    for path in &files {
        let src = sc::scan(path)?;
        let e = sc::fit::estimate(&src, &[], sc::fit::Going::Disc);
        let actual = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        println!(
            "{path}\n  {:.1}s  estimate {} MB  actual {} MB  out by {:+.2}%  pictures {} MB ({}%)",
            e.seconds,
            e.bytes / 1_000_000,
            actual / 1_000_000,
            if actual > 0 {
                (e.bytes as f64 / actual as f64 - 1.0) * 100.0
            } else {
                0.0
            },
            e.video_bytes / 1_000_000,
            e.video_bytes * 100 / e.bytes.max(1)
        );
        estimates.push(e);
    }

    let f = sc::fit::fit(&estimates, capacity.bytes, margin);
    println!(
        "\n{} of {}: {} MB onto {} MB usable of {} MB",
        files.len(),
        capacity.name,
        f.bytes / 1_000_000,
        f.usable / 1_000_000,
        f.capacity / 1_000_000
    );
    if f.fits {
        println!("  it fits as it is, with {} MB to spare", (f.usable - f.bytes) / 1_000_000);
    } else if f.reachable {
        println!(
            "  the pictures have to come to {:.2}% of their own size",
            f.share * 100.0
        );
    } else {
        println!(
            "  it cannot be made to fit: the pictures would have to come to {:.2}%",
            f.share * 100.0
        );
    }
    Ok(())
}
