//! Does a clip whose clocks were joined read as one run of time?
//!
//! Opens a recording the way everything else in the program opens one, walks
//! its pictures in the order the demuxer hands them over, and says where the
//! times go backwards -- which on a joined clip is where two of its stretches
//! were placed on top of one another. Prints the correction table first, so a
//! disagreement can be read against the join it belongs to.
//!
//!     cargo run --example restampdiag -- /rec/Recording.iso/BDAV/STREAM/00001.m2ts
use anyhow::Result;
use ffmpeg_next as ff;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: restampdiag <clip>");
    ff::init()?;

    if let Some(table) = sc::disc::clip_restamp(&path) {
        println!("joined from {} stretch(es):", table.pieces().len());
        for p in table.pieces() {
            println!(
                "  packet {:>9}  {:+.3}s",
                p.at / 192,
                p.shift as f64 / 90_000.0
            );
        }
    } else {
        println!("read as it stands: no clocks to join");
    }

    let input = sc::input::Input::parse(&path)?;
    let mut ictx = sc::input::demux(&input.url)?;
    let video = ictx
        .streams()
        .best(ff::media::Type::Video)
        .map(|s| s.index())
        .expect("no video stream");
    let tb = f64::from(ictx.stream(video).unwrap().time_base());

    let (mut seen, mut back, mut worst) = (0u64, 0u64, 0.0f64);
    let (mut first, mut last, mut high) = (f64::MAX, 0.0f64, f64::MIN);
    for (stream, packet) in ictx.packets() {
        if stream.index() != video {
            continue;
        }
        let Some(pts) = packet.pts() else { continue };
        let at = pts as f64 * tb;
        seen += 1;
        first = first.min(at);
        last = at;
        if at < high {
            back += 1;
            worst = worst.max(high - at);
            if back <= 20 {
                println!("  picture {seen}: {high:.3}s then {at:.3}s");
            }
        }
        high = high.max(at);
    }
    println!(
        "{seen} picture(s), {first:.3}s to {:.3}s (last read {last:.3}s)",
        high
    );
    println!("{back} went backwards, the worst by {worst:.3}s");
    Ok(())
}
