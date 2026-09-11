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
    let src = sc::outline(&path)?.into_source();
    let opts = sc::logo::LogoOptions::default();
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
        }
        Err(e) => println!("no logo: {e}  ({took:.0}s)"),
    }
    Ok(())
}
