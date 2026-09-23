//! What the logo detector sees in one recording, corner by corner.
//!
//! The detector's own answer is a single corner and a list of absences; when
//! it returns "no logo" that answer says nothing about why. This prints the
//! whole of what it had to choose from, which is what an argument about
//! thresholds needs.
use anyhow::Result;
use smartcut_core as sc;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: logodiag <file>");
    // Walked, not the container's answer alone: the template's pass reads a
    // long recording in runs rather than end to end, and it is the access
    // points that say where those runs are. What the application holds by
    // the time it asks for a logo is a walked recording.
    let src = sc::scan(&path)?;
    // Swept from the environment so a threshold can be argued with over a
    // pile of recordings without a rebuild between each answer.
    let num = |k: &str, v: f64| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(v);
    let d = sc::logo::LogoOptions::default();
    let opts = sc::logo::LogoOptions {
        min_absent: num("LOGO_MIN_ABSENT", d.min_absent),
        min_present: num("LOGO_MIN_PRESENT", d.min_present),
        typical_break: num("LOGO_TYPICAL_BREAK", d.typical_break),
        window_seconds: num("LOGO_WINDOW", d.window_seconds),
        ..d
    };
    let began = std::time::Instant::now();
    let found = sc::logo::detect_with(&src, &opts, None);
    let took = began.elapsed().as_secs_f64();
    print!("{}  ", path.rsplit('/').next().unwrap_or(&path));
    match found {
        Ok(l) => {
            println!(
                "{:?} strength {:.1}  {} absences  ({:.0}s)",
                l.corner,
                l.strength,
                l.absent.len(),
                took
            );
            for (a, b) in &l.absent {
                println!("    {a:9.2} - {b:9.2}   ({:.1}s)", b - a);
            }
            // The short ones, which are an insert or a caption card and
            // which only the sound tells apart. See `cm::blocks_from_logo`.
            for (a, b) in &l.brief {
                println!("    {a:9.2} - {b:9.2}   ({:.1}s)  brief", b - a);
            }
        }
        Err(e) => println!("no logo: {e}  ({took:.0}s)"),
    }
    Ok(())
}
