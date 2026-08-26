use anyhow::{bail, Context, Result};
use smartcut_core::{cut, index, plan, CutOptions, PlanOptions};

fn fmt_hms(t: f64) -> String {
    let h = (t / 3600.0).floor() as i64;
    let m = ((t % 3600.0) / 60.0).floor() as i64;
    format!("{h:02}:{m:02}:{:06.3}", t % 60.0)
}

fn parse_time(s: &str) -> Result<f64> {
    let mut total = 0.0;
    for part in s.trim().split(':') {
        let v: f64 = part.parse().with_context(|| format!("bad timestamp {s:?}"))?;
        total = total * 60.0 + v;
    }
    Ok(total)
}

fn parse_range(s: &str) -> Result<(f64, f64)> {
    let (a, b) = s.split_once('-').with_context(|| format!("bad range {s:?}, want START-END"))?;
    Ok((parse_time(a)?, parse_time(b)?))
}

/// Keep-ranges are the complement of the cut-ranges over the whole file.
fn complement(cuts: &mut [(f64, f64)], duration: f64) -> Vec<(f64, f64)> {
    cuts.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let mut keeps = Vec::new();
    let mut pos = 0.0f64;
    for &(a, b) in cuts.iter() {
        if a > pos {
            keeps.push((pos, a.min(duration)));
        }
        pos = pos.max(b);
    }
    if pos < duration {
        keeps.push((pos, duration));
    }
    keeps.into_iter().filter(|(a, b)| b - a > 1e-6).collect()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = None;
    let mut keeps: Vec<(f64, f64)> = Vec::new();
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    let mut allow_open_gop = true;
    let mut output: Option<String> = None;
    let mut analyze = false;
    let mut index_kind = "scan".to_string();
    let mut preview_at: Option<f64> = None;
    let mut detect_cm = false;
    let mut scenes = false;
    let mut audio_es = false;
    let mut cut_near: Option<f64> = None;
    let mut use_logo = false;
    let mut audio_mode = smartcut_core::AudioMode::Copy;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keep" => {
                i += 1;
                keeps.push(parse_range(args.get(i).context("--keep needs a range")?)?);
            }
            "--cut" => {
                i += 1;
                cuts.push(parse_range(args.get(i).context("--cut needs a range")?)?);
            }
            "--no-open-gop" => allow_open_gop = false,
            "--detect-cm" => detect_cm = true,
            "--scenes" => scenes = true,
            "--audio-es" => audio_es = true,
            "--cut-near" => {
                i += 1;
                cut_near = Some(parse_time(args.get(i).context("--cut-near needs a time")?)?);
            }
            "--logo" => use_logo = true,
            "--audio-mode" => {
                i += 1;
                audio_mode = match args.get(i).map(String::as_str) {
                    Some("copy") => smartcut_core::AudioMode::Copy,
                    Some("reencode") => smartcut_core::AudioMode::Reencode,
                    other => bail!("--audio-mode wants copy or reencode, got {other:?}"),
                };
            }
            "--preview" => {
                i += 1;
                preview_at = Some(parse_time(args.get(i).context("--preview needs a time")?)?);
            }
            "--index" => {
                i += 1;
                index_kind = args.get(i).context("--index needs scan|container")?.clone();
            }
            "--analyze" => analyze = true,
            "-o" | "--output" => {
                i += 1;
                output = Some(args.get(i).context("-o needs a path")?.clone());
            }
            a if a.starts_with('-') => bail!("unknown option {a}"),
            a => input = Some(a.to_string()),
        }
        i += 1;
    }
    let Some(input) = input else {
        bail!("usage: smartcut <input> [--keep START-END]... [--cut START-END]... [--no-open-gop]");
    };
    if !keeps.is_empty() && !cuts.is_empty() {
        bail!("use --keep or --cut, not both");
    }

    let index_source: Box<dyn index::IndexSource> = match index_kind.as_str() {
        "scan" => Box::new(index::PacketScan),
        "container" => Box::new(index::ContainerIndex),
        other => bail!("unknown --index {other}; want scan or container"),
    };
    let mut src = smartcut_core::scan_with(&input, index_source.as_ref())?;
    let v = &src.video;
    println!("input : {}", src.path);
    println!(
        "        {} {}x{} {:.3}fps  has_b_frames={}  dur={:.3}s  start={:.3}s",
        v.codec, v.width, v.height, v.frame_rate, v.has_b_frames, src.duration, src.start_time
    );

    let open = src.points.iter().filter(|p| p.open_gop()).count();
    let droppable = src.points.iter().filter(|p| p.open_gop() && p.droppable).count();
    let gaps: Vec<f64> = src.points.windows(2).map(|w| w[1].time - w[0].time).collect();
    let mean_gop = if gaps.is_empty() { 0.0 } else { gaps.iter().sum::<f64>() / gaps.len() as f64 };
    let note = if open == 0 {
        "all closed".to_string()
    } else if droppable == open {
        format!("{open} open (leading pictures, droppable)")
    } else if droppable == 0 {
        format!("{open} open (leading pictures, referenced -- cannot start a copy there)")
    } else {
        format!("{open} open ({droppable} droppable, {} referenced)", open - droppable)
    };
    println!(
        "        {} access points, mean GOP {mean_gop:.3}s, {note}  [{}]",
        src.points.len(),
        src.index_name
    );

    if let Some(at) = cut_near {
        for w in [0.5, 1.0, 2.0] {
            let t = smartcut_core::thumbs::cut_near(&src, at, w, 0.08)?;
            println!(
                "  ±{w:.1}s の窓: {}  ({:+.3}s)",
                fmt_hms(t),
                t - at
            );
        }
        return Ok(());
    }

    if scenes {
        let opts = smartcut_core::ThumbOptions::default();
        let began = std::time::Instant::now();
        let track = smartcut_core::thumbs::build(&src, &opts, None)?;
        let bytes: usize = track.thumbs.iter().map(|t| t.jpeg.len()).sum();
        println!(
            "\nサムネイル : {} 枚 ({:.2}s 間隔, 幅 {}px, {:.1} MB) — {:.2}s で構築",
            track.thumbs.len(),
            track.interval,
            track.width,
            bytes as f64 / 1e6,
            began.elapsed().as_secs_f64()
        );
        println!(
            "シーン    : {} 箇所（しきい値 {:.4}、素材の中央値 {:.4}、平均間隔 {:.1}s）",
            track.scenes.len(),
            track.threshold,
            track.typical,
            src.duration / track.scenes.len().max(1) as f64
        );
        if let Ok(path) = std::env::var("SMARTCUT_SCENES_OUT") {
            let dump: String =
                track.scenes.iter().map(|t| format!("{t:.3}\n")).collect();
            std::fs::write(path, dump)?;
        }
        for t in track.scenes.iter().take(if std::env::var_os("SMARTCUT_SCENES_OUT").is_some() { 0 } else { 24 }) {
            let began = std::time::Instant::now();
            let exact = smartcut_core::thumbs::refine(&src, *t)?;
            println!(
                "   {}  →  精密化 {}  ({:.0}ms)",
                fmt_hms(*t),
                fmt_hms(exact),
                began.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    if detect_cm {
        let logo = if use_logo {
            match smartcut_core::logo::detect(&src, &Default::default()) {
                Ok(l) => Some(l),
                Err(e) => {
                    println!("\nロゴ      : 見つかりません（{e}）— 無音のみで判定します");
                    None
                }
            }
        } else {
            None
        };
        if let Some(l) = &logo {
            println!(
                "\nロゴ      : {:?} 隅 (強さ {:.1}) — 不在 {} 区間",
                l.corner,
                l.strength,
                l.absent.len()
            );
            for (a, b) in &l.absent {
                println!("   {}  →  {}   ({:6.1}s)", fmt_hms(*a), fmt_hms(*b), b - a);
            }
        }
        let opts = smartcut_core::DetectOptions::default();
        let silences = smartcut_core::find_silences(&src, &opts)?;
        let cands = smartcut_core::cm_candidates(&silences, &opts);
        let blocks = match &logo {
            Some(l) if !l.absent.is_empty() => {
                smartcut_core::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration)
            }
            _ => smartcut_core::cm_blocks(&cands, &opts, 0.6),
        };
        // Same treatment the window gives them, so what is printed here is
        // what would be marked there.
        let mut blocks = blocks;
        smartcut_core::cm_refine_boundaries(&src, &mut blocks, 0.5, 0.08);
        println!(
            "\nCM ブロック : {} 個{}",
            blocks.len(),
            if logo.is_some() { "（ロゴ＋無音）" } else { "（無音のみ）" }
        );
        for b in &blocks {
            println!(
                "   {}  →  {}   ({:6.1}s, 継ぎ目 {} 箇所, score {:.2})",
                fmt_hms(b.start),
                fmt_hms(b.end),
                b.duration(),
                b.junctions,
                b.score
            );
        }
        println!("\nCM 境界候補 : {} 個の無音から", silences.len());
        println!("   score  run   silence   time");
        for c in cands.iter().filter(|c| c.score >= 0.4).take(40) {
            println!(
                "   {:.2}   {:>3}   {:5.2}s   {:9.3}  ({})",
                c.score,
                c.run,
                c.silence,
                c.time,
                fmt_hms(c.time)
            );
        }
        return Ok(());
    }

    if let Some(at) = preview_at {
        let shot = smartcut_core::shot_at(&src, at, 720)?;
        let path = output.clone().unwrap_or_else(|| "preview.jpg".into());
        std::fs::write(&path, &shot.jpeg)?;
        // The time reported back is the picture actually decoded, not the one
        // asked for: a transport stream seek can land late, and saying so is
        // what makes the miss testable.
        println!(
            "\nwrote {path} ({} bytes)  asked {:.3}s  got {:.3}s  {} picture",
            shot.jpeg.len(),
            at,
            shot.time,
            shot.kind
        );
        return Ok(());
    }

    let ranges = if !cuts.is_empty() {
        complement(&mut cuts, src.duration)
    } else if !keeps.is_empty() {
        keeps
    } else {
        vec![(0.0, src.duration)]
    };

    // A precomputed index knows where the entry points are but not what
    // hangs off them, so measure that for the ones this cut will use.
    if !src.leading_known {
        index::refine_leading(
            &src.path,
            &src.video,
            src.start_time,
            &mut src.points,
            &ranges,
        )?;
    }

    if std::env::var("SMARTCUT_DEBUG").is_ok() {
        for p in src.points.iter().take(6) {
            eprintln!(
                "  point t={:.4} lead_start={:.4} open={} droppable={}",
                p.time,
                p.lead_start,
                p.open_gop(),
                p.droppable
            );
        }
    }

    let opts = PlanOptions { allow_open_gop, ..Default::default() };
    let plans = plan(&src.video, src.duration, &src.points, &ranges, &opts);

    let total: f64 = plans.iter().map(|p| p.copied() + p.reencoded()).sum();
    println!("\nplan  : {} range(s), {total:.3}s output", plans.len());
    for p in &plans {
        println!("  keep {:.3} -> {:.3}", p.t_in, p.t_out);
        for s in &p.segments {
            println!(
                "    {:>8}  {:8.3} -> {:8.3}  ({:6.3}s, {} frames)",
                s.kind.as_str(),
                s.start,
                s.end,
                s.duration(),
                s.frames
            );
        }
    }
    let copied: f64 = plans.iter().map(|p| p.copied()).sum();
    let enc: f64 = plans.iter().map(|p| p.reencoded()).sum();
    if total > 0.0 {
        println!(
            "        copied {copied:.3}s ({:.1}%), re-encoded {enc:.3}s ({:.1}%)",
            100.0 * copied / total,
            100.0 * enc / total
        );
    }
    if analyze || output.is_none() {
        if output.is_none() && !analyze {
            eprintln!("\n(no -o given; nothing written)");
        }
        return Ok(());
    }
    let out = output.unwrap();
    println!("\nrender:");
    cut(&src, &plans, &out, &CutOptions { audio_mode, ..Default::default() })?;
    if audio_es {
        let beside = std::path::Path::new(&out).with_extension("aac");
        let n = smartcut_core::write_audio_es(&out, &beside.to_string_lossy())?;
        println!("wrote {} ({n} packets)", beside.display());
    }
    println!("wrote {out}");
    Ok(())
}
