//! Why one recording joins another by copying and the next one does not.
//!
//! The output settings screen says how many clips do not match the master and
//! re-encodes those whole. What it could not say, until it was asked to, is
//! *which* property differs -- and the answer decides whether anything can be
//! done about it. Two recordings off one channel a week apart are the same
//! pictures in the same format; if they come out as a mismatch, the mismatch
//! is in what the stream says about itself rather than in the stream.
//!
//!     conformdiag <file>...              every file against the first
//!     conformdiag --shape <file>...      one line of shape per file, no pairs
//!     conformdiag --group <file>...      group by what the shapes come to
//!
//! `--group` is the one for a folder: it prints each distinct shape once with
//! the recordings that hold it, so a channel that changed something between
//! two evenings shows up as two groups rather than as a wall of pairs.
//!
//! Only the container is read -- a probe of the head of each file, tens of
//! milliseconds -- because that is what the list itself compares.
use anyhow::Result;
use smartcut_core as sc;
use std::collections::BTreeMap;

/// Everything [`sc::conform::compare`] looks at, as one line.
fn shape_of(o: &sc::Source) -> String {
    let v = &o.video;
    let sound: Vec<String> = o
        .audios
        .iter()
        .map(|a| format!("{}/{}Hz/{}ch", a.codec, a.sample_rate, a.channels))
        .collect();
    format!(
        "{} {}x{} {:.3}fps sar={:.4} {} pix={} colour={}/{}/{} | {} track(s): {}",
        v.codec,
        v.width,
        v.height,
        v.frame_rate,
        v.sample_aspect_ratio,
        if v.interlaced() {
            if v.top_field_first() {
                "interlaced tff"
            } else {
                "interlaced bff"
            }
        } else {
            "progressive"
        },
        v.shape.pix_fmt,
        v.shape.primaries,
        v.shape.transfer,
        v.shape.matrix,
        o.audios.len(),
        if sound.is_empty() {
            "-".to_string()
        } else {
            sound.join(", ")
        }
    )
}

fn leaf(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn main() -> Result<()> {
    let mut files: Vec<String> = Vec::new();
    let mut mode = "pairs";
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--shape" => mode = "shape",
            "--group" => mode = "group",
            _ => files.push(a),
        }
    }
    if files.is_empty() {
        eprintln!("usage: conformdiag [--shape|--group] <file>...");
        std::process::exit(2);
    }

    // Read once, and go on where one will not open: a folder of a month's
    // recordings off a share always holds one that has gone away.
    let mut read: Vec<(String, sc::Source)> = Vec::new();
    for path in &files {
        match sc::outline(path).map(sc::Outline::into_source) {
            Ok(o) => read.push((path.clone(), o)),
            Err(e) => eprintln!("skipped {}: {e}", leaf(path)),
        }
    }
    if read.is_empty() {
        eprintln!("nothing could be read");
        std::process::exit(1);
    }

    match mode {
        "shape" => {
            for (path, o) in &read {
                println!("{}\n  {}", leaf(path), shape_of(o));
            }
        }
        "group" => {
            let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
            for (path, o) in &read {
                groups.entry(shape_of(o)).or_default().push(leaf(path));
            }
            println!("{} recording(s), {} shape(s)\n", read.len(), groups.len());
            for (shape, names) in &groups {
                println!("[{}] {}", names.len(), shape);
                for n in names.iter().take(6) {
                    println!("    {n}");
                }
                if names.len() > 6 {
                    println!("    ... and {} more", names.len() - 6);
                }
                println!();
            }
        }
        _ => {
            let (master_path, master) = &read[0];
            println!("master: {}\n  {}\n", leaf(master_path), shape_of(master));
            for (path, src) in read.iter().skip(1) {
                let found = sc::conform::compare(master, src);
                // What the cut will actually do, which is not the same as
                // whether anything differs: a difference the pictures carry
                // for themselves costs nothing. See `What::costs_pictures`.
                let fit = sc::conform::fit(master, src);
                println!(
                    "{}: {}",
                    match (fit.video, found.is_empty()) {
                        (true, _) => "re-encodes",
                        (false, true) => "copies",
                        (false, false) => "copies, and differs",
                    },
                    leaf(path)
                );
                for m in &found {
                    println!(
                        "    {}{}",
                        m.describe(),
                        if m.what.costs_pictures() { "" } else { "  (costs nothing)" }
                    );
                }
            }
        }
    }
    Ok(())
}
