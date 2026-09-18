//! Where the front of the file says the material begins, against where the
//! walk says it does.
//!
//! The editor puts a recording on screen from the container's own answer and
//! fills the access points in behind it. Everything it counts from the first
//! picture -- the frame counter, the length of the timeline, the numbers in a
//! mark file -- needs to know which picture that is, and until this the only
//! thing that knew was the walk. `outline_with_head` reads it off the front of
//! the file instead, and it has to be the same picture: a head a fraction of a
//! second out is a mark file that lands a fraction of a second out, and a head
//! a fraction of a *microsecond* below the walk's is a mark the timeline has
//! already closed over.
//!
//!     headdiag <file>...
//!
//! Prints both and says whether they agree. The walk is a pass over the whole
//! recording, so this costs what opening the recording costs.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: headdiag <file>...");
        std::process::exit(2);
    }
    let mut differed = false;
    for path in files {
        let o = sc::outline_with_head(&path)?;
        let s = sc::scan(&path)?;
        let walked = s.points.first().map(|p| p.time);
        let agrees = matches!((o.head, walked), (Some(a), Some(b)) if (a - b).abs() < 1e-9);
        differed |= !agrees;
        println!(
            "{path}\n  head {:?}  walk {:?}  {}  ({} points, start_time {})",
            o.head,
            walked,
            if agrees { "agree" } else { "DIFFER" },
            s.points.len(),
            o.start_time,
        );
    }
    std::process::exit(i32::from(differed));
}
