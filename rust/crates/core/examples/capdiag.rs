//! What a recording's caption stream actually sends, statement by statement.
//!
//! Written to settle the layout conventions against real broadcasts rather
//! than from memory: which screen format each channel declares, where it
//! puts its display area, how big a character is, and what rows and columns
//! its lines are placed at.
//!
//! usage: capdiag <file> [statements]
//!        capdiag <file> at <seconds> [seconds ...]
//!
//! The second form asks the question the window asks -- what is on screen
//! at this instant -- of every subtitle track the recording carries, which
//! is the path a disc's pictures go down as well as a broadcast's text.

use anyhow::Result;
use ffmpeg_next as ff;
use smartcut_core as sc;

/// The C1 codes, by the name the standard gives them.
fn c1(code: u8) -> &'static str {
    match code {
        0x80..=0x87 => "colour",
        0x88 => "SSZ",
        0x89 => "MSZ",
        0x8A => "NSZ",
        0x8B => "SZX",
        0x90 => "COL",
        0x91 => "FLC",
        0x92 => "CDC",
        0x93 => "POL",
        0x94 => "WMM",
        0x95 => "MACRO",
        0x97 => "HLC",
        0x98 => "RPC",
        0x99 => "SPL",
        0x9A => "STL",
        0x9B => "CSI",
        0x9D => "TIME",
        _ => "C1",
    }
}

/// A CSI sequence's final byte, by name.
fn csi(f: u8) -> &'static str {
    match f {
        0x53 => "SWF",
        0x54 => "CCC",
        0x56 => "SDF",
        0x57 => "SSM",
        0x58 => "SHS",
        0x59 => "SVS",
        0x5F => "SDP",
        0x61 => "ACPS",
        0x62 => "TCC",
        0x63 => "ORN",
        0x64 => "MDF",
        0x65 => "CFS",
        0x66 => "XCS",
        0x67 => "SCR",
        0x68 => "PRA",
        0x69 => "ACS",
        0x6E => "RCS",
        0x6F => "SCS",
        _ => "?",
    }
}

fn dump(units: &[u8]) {
    let mut at = 0usize;
    let mut text = String::new();
    let flush = |text: &mut String| {
        if !text.is_empty() {
            print!("[{text}]");
            text.clear();
        }
    };
    while at < units.len() {
        let b = units[at];
        match b {
            0x0C => {
                flush(&mut text);
                print!(" CS");
                at += 1;
            }
            0x0D => {
                flush(&mut text);
                print!(" APR");
                at += 1;
            }
            0x16 => {
                flush(&mut text);
                print!(" PAPF({})", units.get(at + 1).map_or(0, |p| p & 0x3F));
                at += 2;
            }
            0x1C => {
                flush(&mut text);
                print!(
                    " APS({},{})",
                    units.get(at + 1).map_or(0, |p| p & 0x3F),
                    units.get(at + 2).map_or(0, |p| p & 0x3F)
                );
                at += 3;
            }
            0x9B => {
                flush(&mut text);
                let mut i = at + 1;
                let mut params = String::new();
                while let Some(&p) = units.get(i) {
                    if (0x40..=0x7E).contains(&p) {
                        break;
                    }
                    if p != 0x20 {
                        params.push(p as char);
                    }
                    i += 1;
                }
                let f = units.get(i).copied().unwrap_or(0);
                print!(" {}({params})", csi(f));
                at = i + 1;
            }
            0x80..=0x9F => {
                flush(&mut text);
                let name = c1(b);
                at += 1;
                // Step over the parameters the way `arib::control` does,
                // printing them: which colour, which size, how long.
                let n = match b {
                    0x90 | 0x92 => usize::from(units.get(at) == Some(&0x20)) + 1,
                    0x8B | 0x91 | 0x93 | 0x94 | 0x95 | 0x97 | 0x98 => 1,
                    0x9D => 2,
                    _ => 0,
                };
                let params: Vec<u8> = (0..n).filter_map(|k| units.get(at + k).copied()).collect();
                if (0x80..=0x87).contains(&b) {
                    print!(" fg{}", b & 0x0F);
                } else if params.is_empty() {
                    print!(" {name}");
                } else {
                    print!(" {name}{params:02x?}");
                }
                at += n;
            }
            _ => {
                // Everything else, as text: one byte at a time is wrong for
                // the two byte sets, so the whole run is handed to the
                // reader and printed as it comes back.
                let from = at;
                while at < units.len() && !matches!(units[at], 0x00..=0x1F | 0x80..=0x9F) {
                    at += 1;
                }
                text.push_str(&sc::arib::decode(&units[from..at]));
            }
        }
    }
    flush(&mut text);
    println!();
}

/// What each track has on screen at each of these instants.
fn on_screen(path: &str, times: &[f64]) -> Result<()> {
    let src = sc::scan(path)?;
    let tracks = sc::subs::tracks(&src.captions, &src.graphics, &src.subpictures);
    println!("{} subtitle track(s)", tracks.len());
    for track in &tracks {
        println!(
            "  {:#06x} {:?} {}",
            track.id,
            track.kind,
            track.language.as_deref().unwrap_or("-")
        );
        let mut reader = sc::subs::Reader::open(&src, track.id)?;
        for &t in times {
            let began = std::time::Instant::now();
            let shown = reader.at(t)?;
            let took = began.elapsed().as_secs_f64();
            match shown {
                None => println!("    {t:8.2}s  --                              ({took:.2}s)"),
                Some(sc::subs::Shown::Text { plane, runs }) => {
                    println!("    {t:8.2}s  text on {}x{}  ({took:.2}s)", plane.0, plane.1);
                    for r in runs {
                        println!(
                            "              ({:4},{:4}) {:3}x{:2} adv {:2} #{:06x}  {}",
                            r.x, r.y, r.width, r.height, r.advance, r.colour, r.text
                        );
                    }
                }
                Some(sc::subs::Shown::Picture {
                    screen,
                    x,
                    y,
                    width,
                    height,
                    png,
                }) => {
                    if let Ok(dir) = std::env::var("SMARTCUT_PNG_DIR") {
                        let name = format!("{dir}/sub-{:#06x}-{t:.0}.png", track.id);
                        std::fs::write(&name, png).ok();
                        println!("    wrote {name}");
                    }
                    println!(
                    "    {t:8.2}s  picture {width}x{height} at ({x},{y}) of {}x{}, {} byte PNG  ({took:.2}s)",
                    screen.0,
                    screen.1,
                    png.len()
                )
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    ff::init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: capdiag <file> [statements]");
    if args.get(1).map(String::as_str) == Some("at") {
        let times: Vec<f64> = args[2..].iter().filter_map(|s| s.parse().ok()).collect();
        return on_screen(path, &times);
    }
    let want: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);

    let mut ictx = ff::format::input(path)?;
    let streams: Vec<(usize, f64)> = ictx
        .streams()
        .filter(|s| s.parameters().id() == ff::codec::Id::ARIB_CAPTION)
        .map(|s| (s.index(), f64::from(s.time_base())))
        .collect();
    println!("caption streams: {streams:?}");
    let mut layout = sc::caption::Layout::default();
    let mut seen = 0usize;
    let mut packets = 0usize;
    let mut groups: Vec<u8> = Vec::new();
    let mut units: Vec<u8> = Vec::new();
    for (stream, packet) in ictx.packets() {
        let Some(&(_, tb)) = streams.iter().find(|(i, _)| *i == stream.index()) else {
            continue;
        };
        let (Some(data), Some(pts)) = (packet.data(), packet.pts()) else {
            continue;
        };
        packets += 1;
        sc::caption::data_groups(data, |id, body| {
            if !groups.contains(&id) {
                groups.push(id);
            }
            if !sc::caption::is_statement(id) {
                return;
            }
            sc::caption::text_units(body, &mut units);
            if units.is_empty() {
                return;
            }
            seen += 1;
            print!("{:9.3}s group {id:#04x}: ", pts as f64 * tb);
            dump(&units);
            let written = layout.statement(&units);
            for page in &written.pages {
                // What the statement says about when its own page goes up
                // and comes down, which is only ever what `TIME` told it.
                let when = match (page.at, page.until) {
                    (0.0, None) => String::new(),
                    (at, None) => format!("  [+{at:.1}s]"),
                    (at, Some(until)) => format!("  [+{at:.1}s .. +{until:.1}s]"),
                };
                for r in &page.runs {
                    println!(
                        "            plane {}x{}  ({:4},{:4}) {:3}x{:2} adv {:2} #{:06x}  {}{}",
                        written.plane.0,
                        written.plane.1,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        r.advance,
                        r.colour,
                        r.text,
                        when
                    );
                }
            }
        });
        if seen >= want {
            break;
        }
    }
    println!("{packets} caption packets, data groups seen: {groups:?}, {seen} statements");
    Ok(())
}
