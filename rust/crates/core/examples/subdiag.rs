//! Diagnostic for the pair written beside a cut: read a `.idx` back with its
//! `.sub`, decode every unit, and say what each one draws.
//!
//! What it answers is whether the pair is a pair — one unit to a line, its
//! moment, its size, the rectangle of the screen it covers, the four colours
//! it was reduced to, and **how long it says it stands**, which is the one
//! thing a subtitle written out of a Blu-ray's display sets has to be given
//! (see `smartcut_core::vobsub`). A unit that draws nothing is the one that
//! clears the screen.
//!
//! usage: subdiag <file.idx>

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

fn main() -> Result<()> {
    let idx = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("usage: subdiag <file.idx>"))?;
    let mut ictx = ff::format::input(&idx)?;
    let stream = ictx
        .streams()
        .best(ff::media::Type::Subtitle)
        .ok_or_else(|| anyhow!("{idx} carries no subtitles"))?;
    let index = stream.index();
    let mut decoder = ff::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .subtitle()?;

    let mut n = 0;
    for (stream, packet) in ictx.packets() {
        if stream.index() != index {
            continue;
        }
        let at = packet.pts().unwrap_or(0) as f64 / 1000.0;
        let size = packet.data().map_or(0, |d| d.len());
        let mut sub = ff::codec::subtitle::Subtitle::new();
        if !decoder.decode(&packet, &mut sub)? {
            println!("{n:4}  {at:8.3}s  {size:6} bytes  draws nothing");
            n += 1;
            continue;
        }
        // The decoder counts from the moment the subtitle appears, in
        // milliseconds, and says nothing by saying either nothing or
        // everything.
        let stands = match sub.end() {
            0 | u32::MAX => "stands until the next".to_string(),
            ms => format!("stands {:.3}s", ms as f64 / 1000.0),
        };
        let mut drawn = false;
        for rect in sub.rects() {
            let ff::codec::subtitle::Rect::Bitmap(bitmap) = rect else {
                continue;
            };
            drawn = true;
            let colours = unsafe {
                let r = bitmap.as_ptr();
                let table = (*r).data[1] as *const u32;
                (0..((*r).nb_colors as usize).min(16))
                    .map(|i| format!("{:08x}", if table.is_null() { 0 } else { *table.add(i) }))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let (x, y, w, h) = unsafe {
                let r = bitmap.as_ptr();
                ((*r).x, (*r).y, (*r).w, (*r).h)
            };
            println!("{n:4}  {at:8.3}s  {size:6} bytes  {w}x{h} at {x},{y}  {stands}  [{colours}]");
        }
        if !drawn {
            println!("{n:4}  {at:8.3}s  {size:6} bytes  draws nothing");
        }
        n += 1;
    }
    println!("{n} unit(s)");
    Ok(())
}
