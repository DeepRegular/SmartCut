//! What the requantiser does to a real recording, before any of it is wired
//! into a cut.
//!
//! Two questions, and this answers both on whatever file it is pointed at.
//!
//! **Does it read MPEG-2 correctly?** Every picture is taken apart and
//! written back at the strength that changes nothing. The answer has to be
//! the bytes it started as -- every field of every macroblock written again
//! from what was read -- so a table with a row wrong or a length counted
//! wrong shows up here as a picture that differs, and where one does, the
//! first byte that differs is printed.
//!
//! **What does it cost?** With `--share`, the pictures are written again at a
//! share of their size, and both the source and the smaller version are
//! written out as elementary streams for whatever wants to compare them:
//!
//! ```text
//! transdiag <file> [--share 0.79 | --lift 2] [--seconds 60] [--out DIR]
//!           [--ceiling 31] [--codes]
//! ffmpeg -i source.m2v -i small.m2v -lavfi psnr -f null -
//! ```
use std::io::Write;
use std::time::Instant;

use anyhow::{bail, Result};
use smartcut_core as sc;
use smartcut_mpeg2 as mpeg2;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Which kind of picture this packet carries: 1 I, 2 P, 3 B.
fn coding_type(data: &[u8]) -> usize {
    let mut i = 0;
    while i + 6 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == 0 {
            return usize::from((data[i + 5] >> 3) & 7).min(3);
        }
        i += 1;
    }
    0
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: transdiag <file>");
    let share: f64 = arg("--share").map_or(1.0, |v| v.parse().unwrap_or(1.0));
    let seconds: f64 = arg("--seconds").map_or(f64::MAX, |v| v.parse().unwrap_or(f64::MAX));
    let ceiling: u8 = arg("--ceiling").map_or(31, |v| v.parse().unwrap_or(31));
    let out = arg("--out");
    // A lift named outright, which skips the rate control: what the
    // pictures come to is then whatever a constant quality comes to.
    let lift: Option<f64> = arg("--lift").and_then(|v| v.parse().ok());
    let thin: Option<f64> = arg("--thin").and_then(|v| v.parse().ok());
    let press = (lift.is_some() || thin.is_some()).then(|| mpeg2::Squeeze {
        lift: lift.unwrap_or(0.0),
        thin: thin.unwrap_or(0.0),
    });
    let codes = std::env::args().any(|a| a == "--codes");

    let src = sc::scan(&path)?;
    if src.video.codec != "mpeg2video" {
        bail!("{} is {}, and this reads MPEG-2", path, src.video.codec);
    }
    println!(
        "{path}\n  {}x{}  {:.3} fps  {:.1}s",
        src.video.width, src.video.height, src.video.frame_rate, src.duration
    );

    let mut ictx = sc::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let tb = src.video.time_base;

    let mut shape = mpeg2::Shape::default();
    let mut rater = mpeg2::Transrater::new(mpeg2::Quantiser { ceiling });
    let mut exact = 0u64;
    let mut differed = 0u64;
    let mut failed: Vec<(String, u64)> = Vec::new();
    let mut first_difference: Option<(f64, usize)> = None;
    let mut told: Vec<String> = Vec::new();
    // How each kind of picture fared, since the three are coded differently
    // enough that "I pictures are fine and B pictures are not" is most of an
    // answer on its own.
    let mut by_type = [[0u64; 2]; 4];
    let mut by_size = [[0u64; 2]; 4];
    let mut source_es: Option<std::fs::File> = None;
    let mut small_es: Option<std::fs::File> = None;
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
        source_es = Some(std::fs::File::create(format!("{dir}/source.m2v"))?);
        small_es = Some(std::fs::File::create(format!("{dir}/small.m2v"))?);
    }

    let began = Instant::now();
    let mut coded = 0u64;
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        let Some(data) = packet.data() else { continue };
        let at = packet.pts().map_or(0.0, |p| p as f64 * tb - src.start_time);
        if at > seconds {
            break;
        }
        coded += 1;

        let kind = coding_type(data);
        // What quantiser scale this picture was written at, picture by
        // picture, for comparing one file against another.
        if codes {
            let mut shape = shape;
            if let Ok(p) = mpeg2::Picture::read(data, &mut shape) {
                let scales: Vec<u8> = p.scales().collect();
                let mean = scales.iter().map(|&q| f64::from(q)).sum::<f64>()
                    / scales.len().max(1) as f64;
                let mut seen = [0u32; 32];
                for &q in &scales {
                    seen[usize::from(q.min(31))] += 1;
                }
                let top: Vec<String> = (1..32)
                    .filter(|&i| seen[i] > 0)
                    .map(|i| format!("{i}:{}", seen[i]))
                    .collect();
                let (coeffs, blocks) = p.coefficients();
                println!(
                    "code {coded} {} {} {mean:.3} {coeffs} {blocks} {}",
                    "?IPB".as_bytes()[kind] as char,
                    data.len(),
                    top.join(",")
                );
            }
            continue;
        }
        // The identity: the picture, written back unchanged.
        match mpeg2::rewrite_unchanged(data, &mut shape) {
            Ok(back) => {
                let kind = coding_type(data);
                if back == data {
                    exact += 1;
                    by_type[kind][0] += 1;
                } else {
                    differed += 1;
                    by_type[kind][1] += 1;
                    if first_difference.is_none() {
                        let k = back
                            .iter()
                            .zip(data)
                            .position(|(a, b)| a != b)
                            .unwrap_or(back.len().min(data.len()));
                        first_difference = Some((at, k));
                    }
                    if told.len() < 8 {
                        if let Ok(Some(where_)) = mpeg2::first_difference(data, &mut shape.clone())
                        {
                            let line = format!("    {} {where_}", "?IPB".as_bytes()[kind] as char);
                            if !told.contains(&line) {
                                told.push(line);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let name = e.to_string();
                match failed.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, n)) => *n += 1,
                    None => failed.push((name, 1)),
                }
            }
        }

        // And the real thing, at the share asked for. Note this is the call
        // that carries state from picture to picture -- the sequence header
        // it last saw, the strength the last picture needed, and what the
        // pictures so far have spent.
        let small = match press {
            Some(p) => rater.picture_at(data, p),
            None => rater.picture(data, share),
        }
        .unwrap_or_else(|_| data.to_vec());
        by_size[kind][0] += data.len() as u64;
        by_size[kind][1] += small.len() as u64;
        if let (Some(a), Some(b)) = (source_es.as_mut(), small_es.as_mut()) {
            a.write_all(data)?;
            b.write_all(&small)?;
        }
    }
    let took = began.elapsed().as_secs_f64();

    let tally = rater.written;
    println!("  {coded} coded picture(s) in {took:.1}s");
    println!(
        "  written back unchanged: {exact} exact, {differed} differed{}",
        first_difference.map_or(String::new(), |(t, k)| format!(
            " (first at {t:.3}s, from byte {k})"
        ))
    );
    for (k, name) in ["?", "I", "P", "B"].iter().enumerate() {
        if by_type[k][0] + by_type[k][1] > 0 {
            println!(
                "    {name}: {} exact, {} differed",
                by_type[k][0], by_type[k][1]
            );
        }
    }
    for line in &told {
        println!("{line}");
    }
    for (name, n) in &failed {
        println!("    declined {n}: {name}");
    }
    if share < 1.0 || press.is_some() {
        println!(
            "  {}: {} -> {} bytes, which is {:.4}",
            match press {
                Some(p) => format!("at a lift of {:.3} and a thinning of {:.1}", p.lift, p.thin),
                None => format!("at a share of {share:.4}"),
            },
            tally.source_bytes,
            tally.written_bytes,
            tally.share()
        );
        for (k, name) in ["?", "I", "P", "B"].iter().enumerate() {
            if by_size[k][0] > 0 {
                println!(
                    "    {name}: {} -> {} bytes, which is {:.4}",
                    by_size[k][0],
                    by_size[k][1],
                    by_size[k][1] as f64 / by_size[k][0] as f64
                );
            }
        }
        println!(
            "    {:.1} pictures a second, {:.1}x real time",
            coded as f64 / took,
            coded as f64 / src.video.frame_rate / took
        );
    }
    if let Some(dir) = &out {
        println!("  elementary streams in {dir}: source.m2v small.m2v");
    }
    Ok(())
}
