//! What does a Blu-ray say it holds?
//!
//! Reads the index of a disc -- a folder or an `.iso`, BDAV or BDMV -- and
//! prints the rows a chooser would show: what each one is called, how long it
//! runs, what the disc says it carries, and the bytes of the image the stream
//! occupies. The last of those is the answer the rest of the program depends
//! on: it is what gets handed to the demuxer.
//!
//!     cargo run --example discdiag -- /rec/Anime.iso
use anyhow::Result;
use smartcut_core as sc;

fn hms(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{:>2}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: discdiag <disc or image>");
    let at = std::path::Path::new(&path);
    println!("{path}\n  looks like a disc: {}", sc::disc::looks_like_disc(at));

    let disc = sc::disc::read(at)?;
    println!("  {} -- {}", disc.shape.as_str(), disc.label);
    let ticked = disc.entries.iter().filter(|e| e.wanted).count();
    println!("  {} row(s), {ticked} offered ticked\n", disc.entries.len());

    for (i, e) in disc.entries.iter().enumerate() {
        let tick = if e.wanted { "*" } else { " " };
        println!(
            "{tick}{:3}  {}  {:>13} bytes  {}",
            i + 1,
            hms(e.duration),
            e.bytes,
            e.label
        );
        if !e.marks.is_empty() {
            let shown: Vec<String> =
                e.marks.iter().take(6).map(|m| format!("{m:.1}")).collect();
            let more = if e.marks.len() > 6 { ", ..." } else { "" };
            println!("       marks {} [{}{more}]", e.marks.len(), shown.join(", "));
        }
        for t in &e.tracks {
            println!(
                "       {:8} 0x{:04x}  {}{}{}",
                t.kind,
                t.pid,
                t.detail,
                t.language.as_deref().map(|l| format!("  {l}")).unwrap_or_default(),
                if t.carried { "" } else { "  (a cut cannot carry this)" },
            );
        }
        println!("       {}", e.path);
        match sc::input::Input::parse(&e.path) {
            Ok(input) => {
                let range = match input.range {
                    Some(r) => format!("bytes {}..{}", r.at, r.at + r.len),
                    None => "the whole file".to_string(),
                };
                println!("       {range}");
            }
            Err(err) => println!("       cannot open: {err}"),
        }
        println!();
    }
    Ok(())
}
