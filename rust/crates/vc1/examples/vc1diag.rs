//! Say what is in a VC-1 elementary stream.
//!
//! ```text
//! cargo run --example vc1diag -- clip.vc1
//! ```
//!
//! Written to be held against `ffprobe -show_frames` on the same clip: if
//! the picture types and the pulldown flags here do not match what
//! libavcodec makes of them, the index this parsing feeds would be wrong in
//! ways a cut only shows at the splice.

use smartcut_vc1::{headers, Shape};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: vc1diag <file.vc1>");
        std::process::exit(2);
    };
    let data = std::fs::read(&path).expect("read");
    let Some(shape) = Shape::read(&data) else {
        eprintln!("no advanced-profile sequence header in {path}");
        std::process::exit(1);
    };
    println!("{:#?}", shape.sequence);
    println!("{:#?}", shape.entry);

    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut pictures = 0;
    let mut fields = 0i64;
    let mut unreadable = 0;
    for (kind, payload) in headers::bdus(&data) {
        if kind != headers::FRAME {
            continue;
        }
        pictures += 1;
        let window = &payload[..payload.len().min(64)];
        match headers::picture(window, &shape.sequence, &shape.entry) {
            Some(p) => {
                fields += p.display_fields();
                let name = format!(
                    "{:?} {:?}{} tff={} rff={} pq={}",
                    p.coding,
                    p.kind,
                    p.second.map(|s| format!("/{s:?}")).unwrap_or_default(),
                    p.tff as u8,
                    p.rff as u8,
                    p.pqindex,
                );
                match counts.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((name, 1)),
                }
            }
            None => unreadable += 1,
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("\n{pictures} pictures, {fields} fields, {unreadable} unreadable");
    for (name, n) in counts {
        println!("  {n:5}  {name}");
    }
}
