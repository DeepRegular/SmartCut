//! Everything the commercial detector reads, written out for study.
//!
//! The detector turns three readings into blocks in one go, so an argument
//! about how they should be combined has nowhere to stand. This writes the
//! readings themselves -- silences, caption resets, and the logo's own
//! absences -- so that a different way of combining them can be tried
//! against the same evidence without re-reading the recording each time.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: cmdump <file>");
    let src = sc::outline(&path)?.into_source();
    let opts = sc::DetectOptions::default();

    println!("# {}", path);
    println!("duration\t{:.3}", src.duration);

    if let Ok(resets) = sc::caption::resets_with(&src, None) {
        for t in resets {
            println!("reset\t{t:.3}");
        }
    }

    for s in sc::find_silences_with(&src, &opts, None)? {
        println!("silence\t{:.3}\t{:.3}", s.start, s.end);
    }

    match sc::logo::detect_with(&src, &Default::default(), None) {
        Ok(l) => {
            println!("logo\t{:?}\t{:.1}", l.corner, l.strength);
            for (a, b) in l.absent {
                println!("absent\t{a:.3}\t{b:.3}");
            }
        }
        Err(e) => println!("nologo\t{e}"),
    }
    Ok(())
}
