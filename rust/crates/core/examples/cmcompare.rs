//! Both ways of turning the readings into blocks, on one reading of the file.
//!
//! The point of comparison is what each way does to the same recording, and
//! reading a recording four times to ask twice would put the survey's size
//! at the mercy of the disk. So the silences, the resets and the logo are
//! taken once, and both the order-of-preference path and `cm::plan` are run
//! against them, each refined the way the editor refines them.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: cmcompare <file>");
    let src = sc::scan(&path)?;
    let opts = sc::DetectOptions::default();

    let resets = sc::caption::resets_with(&src, None).ok();
    let silences = sc::find_silences_with(&src, &opts, None).unwrap_or_default();
    let logo = sc::logo::detect_with(&src, &Default::default(), None).ok();

    println!("file\t{}", path.rsplit('/').next().unwrap_or(&path));
    println!("dur\t{:.3}", src.duration);
    println!(
        "readings\t{}\t{}\t{}\t{}",
        resets.as_ref().map_or(0, |r| r.len()),
        silences.len(),
        logo.as_ref()
            .map_or("none".into(), |l| format!("{:?}", l.corner)),
        logo.as_ref().map_or(0, |l| l.absent.len()),
    );

    let cands = sc::cm_candidates(&silences, &opts);
    let mut old = match (&resets, &logo) {
        (Some(r), _) => sc::cm_blocks_from_resets(r, src.duration),
        (None, Some(l)) if !l.absent.is_empty() => {
            sc::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration)
        }
        (None, Some(_)) => Vec::new(),
        _ => sc::cm_blocks(&cands, &opts, 0.6),
    };
    let mut new = sc::cm_plan(
        &silences,
        resets.as_deref().unwrap_or(&[]),
        logo.as_ref().map(|l| l.absent.as_slice()),
        src.duration,
        &opts,
        &Default::default(),
    );
    sc::cm_refine_boundaries(&src, &mut old, 0.5, 0.08);
    sc::cm_refine_boundaries(&src, &mut new, 0.5, 0.08);
    for b in &old {
        println!("old\t{:.3}\t{:.3}", b.start, b.end);
    }
    for b in &new {
        println!("new\t{:.3}\t{:.3}", b.start, b.end);
    }
    Ok(())
}
