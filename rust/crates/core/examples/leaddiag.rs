//! What measuring the leading pictures costs on a recording whose index could
//! not say -- a disc's -- and whether reading by byte finds what reading by
//! timestamp does.
//!
//!     leaddiag <recording> <a> <b> [passes=2]
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: leaddiag <recording> <a> <b> [passes]");
    let a: f64 = args[1].parse()?;
    let b: f64 = args[2].parse()?;
    let passes: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2);

    let mut src = sc::scan_with(path, &sc::DiscIndex)?;
    println!("leading_known {}  points {}", src.leading_known, src.points.len());
    // The second pass is what every plan after the first costs.
    for n in 0..passes {
        let t = std::time::Instant::now();
        sc::index::refine_leading(
            &src.input.url.clone(),
            &src.video.clone(),
            src.start_time,
            src.byte_seekable,
            &mut src.points,
            &[(a, b)],
        )?;
        let measured = src.points.iter().filter(|p| p.measured).count();
        println!("  pass {n}: {:.2}s, {measured} point(s) measured", t.elapsed().as_secs_f64());
    }

    let mut old = sc::scan_with(path, &sc::DiscIndex)?;
    let t = std::time::Instant::now();
    sc::index::refine_leading(
        &old.input.url.clone(),
        &old.video.clone(),
        old.start_time,
        false,
        &mut old.points,
        &[(a, b)],
    )?;
    let key = |p: &sc::AccessPoint| {
        (format!("{:.4}", p.time), format!("{:.4}", p.lead_start), p.lead_indices.clone(), p.droppable)
    };
    let mut differ = 0;
    for (i, (n, o)) in src.points.iter().zip(&old.points).enumerate() {
        if n.measured && key(n) != key(o) {
            differ += 1;
            println!("  #{i} by byte {:?}\n      by time {:?}", key(n), key(o));
        }
    }
    println!(
        "  by timestamp instead: {:.2}s, {differ} point(s) read differently",
        t.elapsed().as_secs_f64()
    );
    Ok(())
}
