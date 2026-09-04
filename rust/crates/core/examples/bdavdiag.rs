//! What does a BDAV disc say it holds?
//!
//! Reads the index of a disc -- a directory or an `.iso` -- and prints the
//! recordings it names, the clip each one plays, and the bytes of the image
//! that clip occupies. The last of those is the answer the rest of the
//! program depends on: it is what gets handed to the demuxer.
//!
//!     cargo run --example bdavdiag -- /rec/Anime.iso
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: bdavdiag <disc or image>");
    let at = std::path::Path::new(&path);
    println!("{path}\n  looks like BDAV: {}", sc::bdav::looks_like_bdav(at));

    let titles = sc::bdav::titles(at)?;
    println!("  {} title(s)\n", titles.len());
    for t in &titles {
        println!("{}  {}", t.playlist, t.label());
        if let Some(made) = &t.made {
            println!("  made    {made}");
        }
        println!("  playing {:.3}s over {} clip(s)", t.duration, t.clips.len());
        if !t.marks.is_empty() {
            let shown: Vec<String> =
                t.marks.iter().take(8).map(|m| format!("{m:.3}")).collect();
            let more = if t.marks.len() > 8 { ", ..." } else { "" };
            println!("  marks   {} [{}{more}]", t.marks.len(), shown.join(", "));
        }
        for c in &t.clips {
            println!("  clip    {} {:.3} -> {:.3}", c.name, c.start, c.end);
            println!("          {}", c.path);
            match sc::input::Input::parse(&c.path) {
                Ok(input) => {
                    let range = match input.range {
                        Some(r) => format!("bytes {}..{}", r.at, r.at + r.len),
                        None => "the whole file".to_string(),
                    };
                    println!("          {range}");
                    println!("          {}", input.url);
                }
                Err(e) => println!("          cannot open: {e}"),
            }
        }
        println!();
    }
    Ok(())
}
