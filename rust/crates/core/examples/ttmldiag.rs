//! What a 4K recording's subtitle stream actually sends, document by
//! document.
//!
//! Written to read the real thing rather than the shape of it: the times a
//! recorder writes on the packets (a counter, not a clock), the times inside
//! the documents, where each caption is placed, and what it says.
//!
//! usage: ttmldiag <file> [count]

use anyhow::{anyhow, Result};
use smartcut_core as sc;

fn main() -> Result<()> {
    sc::init()?;
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or_else(|| anyhow!("usage: ttmldiag <file> [count]"))?;
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(8);

    let src = sc::scan(&path)?;
    for c in &src.captions {
        println!(
            "stream {} pid {:#06x}  {:?} {:?}",
            c.stream_index, c.pid, c.kind, c.format
        );
    }
    let Some(track) = src
        .captions
        .iter()
        .find(|c| c.format == sc::TextFormat::Ttml)
    else {
        return Err(anyhow!("{path} carries no TTML subtitles"));
    };

    let mut ictx = sc::input::demux(&src.input.url)?;
    let mut seen = 0;
    let mut documents = 0;
    for (stream, packet) in ictx.packets() {
        if stream.index() != track.stream_index {
            continue;
        }
        let Some(data) = packet.data() else { continue };
        documents += 1;
        let stamped = packet.pts().unwrap_or(-1);
        let Some((begin, end)) = sc::ttml::cue(data) else {
            println!("  {documents:4}: no times in this document");
            continue;
        };
        let written = sc::ttml::written(data);
        let text: String = written
            .iter()
            .flat_map(|w| w.pages.iter())
            .flat_map(|p| p.runs.iter())
            .map(|r| r.text.as_str())
            .collect();
        let where_at = written
            .as_ref()
            .and_then(|w| w.pages.first())
            .and_then(|p| p.runs.first())
            .map(|r| format!("({},{}) {}px", r.x, r.y, r.advance))
            .unwrap_or_default();
        println!("  {documents:4}: pts {stamped:>6}  {begin:8.3}..{end:8.3}  {where_at:>18}  {text}");
        seen += 1;
        if seen >= count {
            break;
        }
    }
    println!("{documents} document(s) read");
    Ok(())
}
