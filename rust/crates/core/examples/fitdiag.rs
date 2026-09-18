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
//!     fitdiag <file>... [--disc 25|50|100|128] [--margin 0.01] [--index auto|disc|container|scan]
//!
//! `--index` says how the recordings are opened, because that is what
//! decides whether the rate the estimate is built on was counted, sampled or
//! guessed at. `auto` is the chain the output settings screen itself opens
//! with -- the disc's own map where there is one, the container's table
//! where there is not, the full walk where neither answers.
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

    let how = arg("--index").unwrap_or_else(|| "auto".into());
    let mut estimates = Vec::new();
    for path in &files {
        let src = match how.as_str() {
            "disc" => sc::scan_with(path, &sc::DiscIndex)?,
            "container" => sc::scan_with(path, &sc::ContainerIndex)?,
            "scan" => sc::scan_with(path, &sc::PacketScan)?,
            _ => sc::scan_with(path, &sc::DiscIndex)
                .or_else(|_| sc::scan_with(path, &sc::ContainerIndex))
                .or_else(|_| sc::scan_with(path, &sc::PacketScan))?,
        };
        let e = sc::fit::estimate(&src, &[], sc::fit::Going::Disc);
        let r = sc::fit::rates(&src);
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
        println!(
            "  pictures {:.3} Mbit/s ({}), sound {:.3} Mbit/s ({})",
            r.video / 1e6,
            if src.video.bit_rate.is_some() {
                "read off the stream"
            } else {
                "the file's own rate, less a tenth"
            },
            r.audio / 1e6,
            src.audios
                .iter()
                .map(|a| match a.bit_rate {
                    Some(b) => format!("{} {}ch states {} kbit/s", a.codec, a.channels, b / 1000),
                    None => format!("{} {}ch states nothing", a.codec, a.channels),
                })
                .collect::<Vec<_>>()
                .join("; "),
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
