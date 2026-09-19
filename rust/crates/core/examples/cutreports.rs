//! What a cut reports as it runs: which pass, how far through the job, and
//! how far through that pass.
//!
//!     cutreports <recording> <out.ts> <a-b> [<a-b> ...]
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a recording");
    let out = args.next().expect("an output");
    let ranges: Vec<(f64, f64)> = args
        .map(|r| {
            let (a, b) = r.split_once('-').expect("a-b");
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();
    let src = sc::scan(&path)?;
    let plans = sc::plan::plan(
        &src.video,
        src.duration,
        &src.points,
        &ranges,
        &sc::PlanOptions::default(),
    );
    sc::cut_with_progress(
        &src,
        &plans,
        &out,
        &sc::CutOptions::default(),
        Some(Box::new(|pass, done, within| {
            println!("{pass:?}  job {:5.1}%   pass {:5.1}%", done * 100.0, within * 100.0);
        })),
    )?;
    Ok(())
}
