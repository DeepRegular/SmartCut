//! How far a copied segment would have to be re-encoded to reach a picture it
//! can be joined onto cleanly.
//!
//! A Blu-ray or a broadcast may put either of two things at an entry point: an
//! IDR, which restarts the coded video sequence and so is a place a splice
//! costs nothing, or an I picture with a recovery point, which is only a place
//! to *start reading* -- the pictures after it may still reference pictures
//! before it, which at a splice are not there. This walks a recording's
//! pictures, marks which entry points are IDRs, and prints how far it is from
//! each entry point to the next IDR at or after it. That distance is what
//! joining onto an entry point would cost if the cut re-encoded its way to a
//! clean one.
//!
//!     idrdiag <file>
use anyhow::Result;
use smartcut_core as sc;

/// Does this packet carry an IDR picture? Annex-B, which is what a transport
/// stream and a `.m2ts` hold.
fn has_idr(data: &[u8], codec: &str) -> bool {
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let b = data[i + 3];
            match codec {
                // h.264: five bits of type, and 5 is an IDR slice.
                "h264" => {
                    if b & 0x1F == 5 {
                        return true;
                    }
                }
                // HEVC: six bits, and 16..=23 are the intra random access
                // pictures, of which 19 and 20 are the IDRs.
                "hevc" => {
                    let t = (b >> 1) & 0x3F;
                    if t == 19 || t == 20 {
                        return true;
                    }
                }
                _ => return true,
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: idrdiag <file>");
    let src = sc::scan(&path)?;
    println!("{path}");
    println!(
        "  {} {}x{}  {:.3} fps  {:.1}s  {} access point(s)",
        src.video.codec,
        src.video.width,
        src.video.height,
        src.video.frame_rate,
        src.duration,
        src.points.len()
    );

    let mut ictx = sc::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let tb = src.video.time_base;
    let mut idrs: Vec<f64> = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx || !packet.is_key() {
            continue;
        }
        let Some(data) = packet.data() else { continue };
        if has_idr(data, &src.video.codec) {
            if let Some(pts) = packet.pts() {
                idrs.push(pts as f64 * tb - src.start_time);
            }
        }
    }
    idrs.sort_by(f64::total_cmp);
    println!("  {} of them are IDRs", idrs.len());
    if idrs.is_empty() {
        println!("  nothing to join onto: every entry point is a recovery point");
        return Ok(());
    }

    // For each entry point, how far to the next IDR at or after it.
    let mut costs: Vec<f64> = Vec::new();
    for p in &src.points {
        let i = idrs.partition_point(|t| *t < p.time - 1e-6);
        match idrs.get(i) {
            Some(t) => costs.push(t - p.time),
            // Past the last IDR there is nothing to reach; count it as the
            // run to the end, which is the worst this can cost.
            None => costs.push(src.duration - p.time),
        }
    }
    costs.sort_by(f64::total_cmp);
    let at = |q: f64| costs[((costs.len() - 1) as f64 * q).round() as usize];
    let free = costs.iter().filter(|c| **c < 1e-6).count();
    println!(
        "  joining at an entry point costs, in seconds re-encoded:\n\
             free {free} of {}  median {:.2}  p90 {:.2}  p99 {:.2}  worst {:.2}",
        costs.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    );
    let mean: f64 = costs.iter().sum::<f64>() / costs.len() as f64;
    println!("    mean {mean:.2}");

    // The two lists themselves, for building a test out of: entry points that
    // are IDRs and so can be joined onto for nothing, and entry points that
    // are as far from one as this recording gets.
    if std::env::args().any(|a| a == "--times") {
        let near: Vec<String> = idrs.iter().map(|t| format!("{t:.3}")).collect();
        println!("  idr: {}", near.join(" "));
        // Every entry point and what joining onto it would cost, so the
        // selection of a test set can be made outside.
        let all: Vec<String> = src
            .points
            .iter()
            .map(|p| {
                let i = idrs.partition_point(|t| *t < p.time - 1e-6);
                let cost = idrs.get(i).map_or(src.duration, |t| *t) - p.time;
                format!("{:.3}/{cost:.3}", p.time)
            })
            .collect();
        println!("  all: {}", all.join(" "));
        let mut far: Vec<(f64, f64)> = src
            .points
            .iter()
            .map(|p| {
                let i = idrs.partition_point(|t| *t < p.time - 1e-6);
                (p.time, idrs.get(i).map_or(src.duration, |t| *t) - p.time)
            })
            .collect();
        far.sort_by(|a, b| b.1.total_cmp(&a.1));
        let worst: Vec<String> = far
            .iter()
            .take(40)
            .map(|(t, c)| format!("{t:.3}/{c:.1}"))
            .collect();
        println!("  far: {}", worst.join(" "));
    }
    Ok(())
}
