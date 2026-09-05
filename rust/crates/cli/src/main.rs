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

/// The recordings on `input`, when it is a disc rather than a recording.
///
/// A directory of `.ts` files is not a disc and a `.iso` that holds no BDAV
/// or BDMV is not one either; both are simply not this, and the caller
/// carries on with what it was given.
fn on_a_disc(input: &str) -> Result<Option<smartcut_core::disc::Disc>> {
    let at = std::path::Path::new(input);
    if !smartcut_core::disc::looks_like_disc(at) {
        return Ok(None);
    }
    smartcut_core::disc::read(at).map(Some)
}

/// The recording `--title` names: its number in the list, or a piece of the
/// programme's name.
///
/// A number is a number: `--title 7` on a disc of three is a mistake, not a
/// search for the digit 7 in the names.
fn pick<'a>(
    entries: &'a [smartcut_core::disc::Entry],
    want: Option<&str>,
) -> Result<Option<&'a smartcut_core::disc::Entry>> {
    let Some(want) = want else { return Ok(None) };
    if let Ok(n) = want.parse::<usize>() {
        return match entries.get(n.wrapping_sub(1)).filter(|_| n >= 1) {
            Some(e) => Ok(Some(e)),
            None => bail!("--title {n}: this disc holds {} recording(s)", entries.len()),
        };
    }
    entries
        .iter()
        .find(|e| e.label.contains(want) || e.path.contains(want))
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("--title {want:?}: no recording on this disc is called that"))
}

fn list_disc(input: &str, disc: &smartcut_core::disc::Disc) {
    println!("disc  : {input}");
    println!("        {} -- {}", disc.shape.as_str(), disc.label);
    println!("        {} recording(s)\n", disc.entries.len());
    // What a pressed disc mostly holds is not the film, so the ones worth a
    // look are pointed at rather than left to be found by their length. A
    // disc of recordings is all worth a look, and a column of stars beside
    // every row would be a column saying nothing.
    let some = disc.entries.iter().any(|e| !e.wanted);
    for (i, e) in disc.entries.iter().enumerate() {
        let marks = if e.marks.is_empty() {
            String::new()
        } else {
            format!("  {} mark(s)", e.marks.len())
        };
        let tick = match (some, e.wanted) {
            (true, true) => "*",
            (true, false) => " ",
            (false, _) => "",
        };
        println!("{tick}{:3}  {}  {}{marks}", i + 1, fmt_hms(e.duration), e.label);
        // Only where there is a choice to make. A clip with one sound track
        // and nothing else is a clip the list has already described.
        if e.tracks.iter().filter(|t| t.kind != "video").count() > 1 {
            for t in &e.tracks {
                if t.kind == "video" {
                    continue;
                }
                let lang = t.language.as_deref().map(|l| format!(" {l}")).unwrap_or_default();
                let gone = if t.carried { "" } else { " -- a cut cannot carry this" };
                println!("       0x{:04x}  {}{lang}{gone}", t.pid, t.detail);
            }
        }
    }
    println!("\nname one with --title N to open it");
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
    let mut seek_index: Option<String> = None;
    let mut preview_at: Option<f64> = None;
    let mut make_proxy = false;
    // Which recording on a disc. Nothing else takes one.
    let mut title: Option<String> = None;
    let mut as_proxy = false;
    let mut detect_cm = false;
    let mut scenes = false;
    let mut audio_es = false;
    let mut cut_near: Option<f64> = None;
    let mut use_logo = false;
    // Whatever the engine has as its default, which is smart rendering.
    let mut audio_mode = smartcut_core::AudioMode::default();
    // And the recording's own codec, which is what every mode but a
    // whole-track re-encode can offer.
    let mut audio_codec = smartcut_core::AudioCodec::default();
    let mut aac = smartcut_core::AacVersion::Auto;
    // All of these follow the recording unless they are asked not to.
    let mut audio_channels: Option<u16> = None;
    let mut audio_bit_rate: Option<usize> = None;
    let mut audio_sample_rate: Option<u32> = None;
    let mut audio_bits: Option<u8> = None;
    // Everything the recording carries is written unless it is named here.
    let mut drop_streams: Vec<usize> = Vec::new();
    // A cut of a broadcast is a partial transport stream unless asked for
    // in one of the other two shapes. See `smartcut_core::si::Tables`.
    let mut tables = smartcut_core::si::Tables::default();

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
            "--title" => {
                i += 1;
                title = Some(args.get(i).context("--title needs a number or a name")?.clone());
            }
            "--audio-mode" => {
                i += 1;
                audio_mode = match args.get(i).map(String::as_str) {
                    Some("copy") => smartcut_core::AudioMode::Copy,
                    Some("smart") => smartcut_core::AudioMode::Smart,
                    Some("reencode") => smartcut_core::AudioMode::Reencode,
                    other => bail!("--audio-mode wants copy, smart or reencode, got {other:?}"),
                };
            }
            "--audio-codec" => {
                i += 1;
                let v = args.get(i).context("--audio-codec needs a name")?;
                audio_codec = smartcut_core::AudioCodec::parse(v).with_context(|| {
                    format!("--audio-codec wants source, aac, lpcm, ac3 or dts, got {v:?}")
                })?;
            }
            "--audio-channels" => {
                i += 1;
                let v = args.get(i).context("--audio-channels needs a count")?;
                audio_channels = Some(
                    v.parse::<u16>()
                        .ok()
                        .filter(|c| (1..=8).contains(c))
                        .with_context(|| format!("--audio-channels wants 1..8, got {v:?}"))?,
                );
            }
            "--audio-bitrate" => {
                i += 1;
                let v = args.get(i).context("--audio-bitrate needs a rate")?;
                // Written either way round -- 192k as it is spoken, or the
                // bits per second the engine actually takes.
                let bits = match v.strip_suffix(['k', 'K']) {
                    Some(n) => n.parse::<f64>().map(|n| n * 1000.0),
                    None => v.parse::<f64>(),
                };
                audio_bit_rate = Some(
                    bits.ok()
                        .filter(|b| (8000.0..=2_000_000.0).contains(b))
                        .map(|b| b as usize)
                        .with_context(|| format!("--audio-bitrate wants 8k..2000k, got {v:?}"))?,
                );
            }
            "--audio-samplerate" => {
                i += 1;
                let v = args.get(i).context("--audio-samplerate needs a rate")?;
                // Written either way round -- 48k as it is spoken, or the
                // samples per second the engine actually takes.
                let hz = match v.strip_suffix(['k', 'K']) {
                    Some(n) => n.parse::<f64>().map(|n| n * 1000.0),
                    None => v.parse::<f64>(),
                };
                audio_sample_rate = Some(
                    hz.ok()
                        .filter(|r| (8000.0..=192_000.0).contains(r))
                        .map(|r| r as u32)
                        .with_context(|| {
                            format!("--audio-samplerate wants 8000..192000, got {v:?}")
                        })?,
                );
            }
            "--audio-bits" => {
                i += 1;
                let v = args.get(i).context("--audio-bits needs a width")?;
                audio_bits = Some(
                    v.parse::<u8>()
                        .ok()
                        .filter(|b| matches!(b, 16 | 24))
                        .with_context(|| format!("--audio-bits wants 16 or 24, got {v:?}"))?,
                );
            }
            "--aac" => {
                i += 1;
                aac = match args.get(i).map(String::as_str) {
                    Some("auto") => smartcut_core::AacVersion::Auto,
                    Some("mpeg2") => smartcut_core::AacVersion::Mpeg2,
                    Some("mpeg4") => smartcut_core::AacVersion::Mpeg4,
                    other => bail!("--aac wants auto, mpeg2 or mpeg4, got {other:?}"),
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
            "--seek-index" => {
                i += 1;
                seek_index =
                    Some(args.get(i).context("--seek-index needs a path")?.clone());
            }
            "--drop-stream" => {
                i += 1;
                let v = args.get(i).context("--drop-stream needs a stream index")?;
                drop_streams.push(
                    v.parse::<usize>()
                        .with_context(|| format!("--drop-stream wants a number, got {v:?}"))?,
                );
            }
            "--tables" => {
                i += 1;
                let v = args.get(i).context("--tables needs partial, broadcast or muxer")?;
                tables = match v.as_str() {
                    "partial" => smartcut_core::si::Tables::Partial,
                    "broadcast" => smartcut_core::si::Tables::Broadcast,
                    "muxer" | "none" => smartcut_core::si::Tables::Muxer,
                    other => bail!("--tables wants partial, broadcast or muxer, got {other:?}"),
                };
            }
            // What the option was called when there were only two answers.
            "--no-tables" => tables = smartcut_core::si::Tables::Muxer,
            "--proxy" => make_proxy = true,
            "--as-proxy" => as_proxy = true,
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
        bail!(
            "usage: smartcut <input> [--keep START-END]... [--cut START-END]... \
             [--drop-stream INDEX]... [--tables partial|broadcast|muxer] [--no-open-gop] \
             [--title N]\n\
             <input> is a recording, or a Blu-ray -- a BDAV or BDMV folder, \
             or an .iso of one -- whose recordings are listed when no --title \
             is given"
        );
    };
    // A share the machine has already mounted may be named the way it is
    // written down -- `smb://nas/rec/a.ts` or `\\nas\rec\a.ts` -- rather than
    // by the mount point it happens to have been given.
    let input = smartcut_core::netpath::resolve(&input)?
        .to_string_lossy()
        .into_owned();
    // A disc holds several recordings and is opened by naming one of them.
    // Without a name it is a question rather than a job: say what is on it.
    // The chapter points the disc's index carried, on the clip's own clock.
    // Held until the recording is open: saying where a mark *is* means
    // rebasing it by the container's start, and nothing knows that yet.
    let mut chapters: Vec<f64> = Vec::new();
    let input = match on_a_disc(&input)? {
        None => input,
        Some(disc) => match pick(&disc.entries, title.as_deref())? {
            Some(entry) => {
                println!("title : {}", entry.label);
                chapters = entry.marks.iter().map(|m| entry.start + m).collect();
                entry.path.clone()
            }
            None => {
                list_disc(&input, &disc);
                return Ok(());
            }
        },
    };
    if !keeps.is_empty() && !cuts.is_empty() {
        bail!("use --keep or --cut, not both");
    }

    let index_source: Box<dyn index::IndexSource> = match index_kind.as_str() {
        "scan" => Box::new(index::PacketScan),
        "container" => Box::new(index::ContainerIndex),
        other => bail!("unknown --index {other}; want scan or container"),
    };
    // A seek index written by an earlier run stands in for the walk over the
    // packets. Reading it back is the whole point: it is the same answer, and
    // it did not cost a pass over the recording to get.
    let index_file = seek_index.as_ref().map(std::path::PathBuf::from);
    let held = match &index_file {
        Some(p) if p.is_file() => Some(smartcut_core::SeekIndex::load(p)?),
        _ => None,
    };
    // `--as-proxy` reads the input as a proxy of something else: same file,
    // but its timestamps are the recording's and must not be rebased again.
    let mut src = if as_proxy {
        smartcut_core::proxy::open(&input)?
    } else if let Some(ix) = &held {
        smartcut_core::scan_with(&input, ix)?
    } else {
        smartcut_core::scan_with(&input, index_source.as_ref())?
    };
    // Written straight away, so that a run which never builds a thumbnail
    // track still leaves the expensive half behind. The `--scenes` path
    // writes it again with the track once it has one.
    let writing = index_file.filter(|_| held.is_none() && !as_proxy);
    if let Some(p) = &writing {
        smartcut_core::SeekIndex::of(&src, None).save(p)?;
    }
    let v = &src.video;
    println!("input : {}", src.path);
    println!(
        "        {} {}x{} {:.3}fps  has_b_frames={}  dur={:.3}s  start={:.3}s",
        v.codec, v.width, v.height, v.frame_rate, v.has_b_frames, src.duration, src.start_time
    );
    // Where the recorder set its chapters, in the recording's own seconds --
    // which is what the editor draws them at, and what `--cut` would take.
    if !chapters.is_empty() {
        let shown: Vec<String> = chapters
            .iter()
            .take(8)
            .map(|c| c - src.start_time)
            // A mark on the first picture lands a rounding away from zero,
            // the playlist counting in 45 kHz and the container in
            // microseconds. Printed as `-0.000` it reads like a mark before
            // the recording, which it is not.
            .map(|t| if t.abs() < 0.0005 { 0.0 } else { t })
            .map(|t| format!("{t:.3}"))
            .collect();
        let more = if chapters.len() > 8 { ", ..." } else { "" };
        println!("marks : {} [{}{more}]", chapters.len(), shown.join(", "));
    }

    // Which AAC the recording carries is the thing to know before a cut
    // re-encodes any of it: a broadcast is MPEG-2 AAC, and a frame this tool
    // writes has to say the same. Read off the main track; a recording does
    // not mix the two within itself.
    let form = match smartcut_core::adts::of_source(&src) {
        Some(f) if f.mpeg2 => "  MPEG-2 ADTS".to_string(),
        Some(_) => "  MPEG-4 ADTS".to_string(),
        None => String::new(),
    };
    for (n, a) in src.audios.iter().enumerate() {
        // The stream index is what names a track to `--drop-stream`, so it
        // is printed even when there is only one.
        let main = if src.audio.as_ref().is_some_and(|m| m.stream_index == a.stream_index) {
            "  main"
        } else {
            ""
        };
        let lang = a.language.as_deref().map(|l| format!("  {l}")).unwrap_or_default();
        println!(
            "audio{}: {} {}Hz {}ch{lang}{}{main}   [stream {} pid 0x{:04x}]",
            if src.audios.len() > 1 { format!(" {}", n + 1) } else { "  ".to_string() },
            a.codec,
            a.sample_rate,
            a.channels,
            if n == 0 { form.as_str() } else { "" },
            a.stream_index,
            a.pid,
        );
    }
    for c in &src.captions {
        let lang = c.language.as_deref().map(|l| format!(" {l}")).unwrap_or_default();
        println!("caption:{lang} ARIB STD-B24   [stream {} pid 0x{:04x}]", c.stream_index, c.pid);
    }
    // Said out loud rather than dropped in silence: these are streams a cut
    // has no way to carry. See `smartcut_core::DroppedStream`.
    for d in &src.dropped {
        println!("        not carried: {} on pid 0x{:04x}", d.describe(), d.pid);
    }

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
        // A held index carries the track it was built with, so the pass over
        // the key pictures is not repeated either.
        let built;
        let (track, how) = match held.as_ref().and_then(|ix| ix.track.as_ref()) {
            Some(t) => (t, "読み込み"),
            None => {
                built = smartcut_core::thumbs::build(&src, &opts, None)?;
                (&built, "構築")
            }
        };
        let bytes: usize = track.thumbs.iter().map(|t| t.jpeg.len()).sum();
        println!(
            "\nサムネイル : {} 枚 ({:.2}s 間隔, 幅 {}px, {:.1} MB) — {:.2}s で{how}",
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
        if let Some(p) = &writing {
            smartcut_core::SeekIndex::of(&src, Some(track)).save(p)?;
            println!(
                "シーク用インデックス : {} ({:.1} MB)",
                p.display(),
                std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as f64 / 1e6
            );
        }
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
        // Ask the caption stream first. When the broadcaster resets the
        // service at its junctions those marks are exact, which neither of
        // the other two readings can be, and they cost one pass over a
        // stream that needs no decoding. When they are absent -- and on
        // several channels they are -- nothing is lost by having looked.
        let resets = match smartcut_core::caption::resets(&src) {
            Ok(r) => {
                println!("\n字幕リセット : {} 箇所", r.len());
                Some(r)
            }
            Err(e) => {
                println!("\n字幕リセット : ありません（{e}）");
                None
            }
        };
        // The logo costs half a minute of decoding and is the weaker signal
        // where the resets exist, so it is not paid for then.
        let logo = if use_logo && resets.is_none() {
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
        // Silences are only wanted where they still decide something.
        let silences = match &resets {
            Some(_) => Vec::new(),
            None => smartcut_core::find_silences(&src, &opts)?,
        };
        let cands = smartcut_core::cm_candidates(&silences, &opts);
        let blocks = match (&resets, &logo) {
            (Some(r), _) => smartcut_core::cm_blocks_from_resets(r, src.duration),
            (None, Some(l)) if !l.absent.is_empty() => {
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
            match (&resets, &logo) {
                (Some(_), _) => "（字幕リセット）",
                (None, Some(_)) => "（ロゴ＋無音）",
                _ => "（無音のみ）",
            }
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
        if let Some(r) = &resets {
            println!("\n継ぎ目 : 字幕リセット {} 箇所", r.len());
            for t in r {
                println!("   {:9.3}  ({})", t, fmt_hms(*t));
            }
            return Ok(());
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

    if make_proxy {
        // A recording inside a disc image has nothing to be written beside,
        // so the name has to be given rather than derived.
        if output.is_none() && src.input.nested() {
            bail!("--make-proxy on a recording inside a disc needs -o");
        }
        let out = output.clone().unwrap_or_else(|| {
            std::path::Path::new(&src.path)
                .with_extension("proxy.mp4")
                .to_string_lossy()
                .into_owned()
        });
        let opts = smartcut_core::ProxyOptions::default();
        let built = smartcut_core::proxy::build(
            &src,
            &out,
            &opts,
            &smartcut_core::ThumbOptions::default(),
            Some(Box::new(|f| {
                eprint!("\r  proxy {:5.1}%", f * 100.0);
                use std::io::Write as _;
                let _ = std::io::stderr().flush();
            })),
            None,
            None,
        )?;
        eprintln!();
        println!(
            "\nwrote {} ({:.1} MB)  {}x{}  {}  {} pictures  {} thumbs  {} scenes  {:.1}s",
            built.path,
            built.bytes as f64 / 1e6,
            built.width,
            built.height,
            built.encoder,
            built.pictures,
            built.track.thumbs.len(),
            built.track.scenes.len(),
            built.seconds
        );
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
    // What the audio will be, which is not always what was asked for: a
    // downmix has no copy path, and the engine says so and re-encodes.
    let asked = audio_channels.filter(|&c| src.audio.as_ref().is_some_and(|a| a.channels != c));
    // A rate the recording does not have is the same story told about the
    // other axis of a sample, and so is a width.
    let resampled =
        audio_sample_rate.filter(|&r| src.audio.as_ref().is_some_and(|a| a.sample_rate != r));
    let requantised = audio_bits.filter(|&b| src.audio.as_ref().is_some_and(|a| a.bits != b));
    // As is naming a codec: there is no copying a frame into one it is not
    // already in.
    let recoded = audio_codec != smartcut_core::AudioCodec::Source;
    println!(
        "\nrender:  audio {}{}{}{}{}{}",
        if asked.is_some() || resampled.is_some() || requantised.is_some() || recoded {
            "reencode"
        } else {
            audio_mode.as_str()
        },
        if recoded { format!(", as {}", audio_codec.as_str()) } else { String::new() },
        asked.map_or(String::new(), |c| format!(", downmixed to {c}ch")),
        resampled.map_or(String::new(), |r| format!(", at {r} Hz")),
        requantised.map_or(String::new(), |b| format!(", {b} bit")),
        audio_bit_rate.map_or(String::new(), |b| format!(", {} kbit/s", b / 1000)),
    );
    // What is going out beside the pictures, and what is not.
    let to_ts = std::path::Path::new(&out)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ts" | "m2ts" | "mts" | "m2t"));
    let kept_audio = src.audios.iter().filter(|a| !drop_streams.contains(&a.stream_index)).count();
    let kept_caps = if to_ts {
        src.captions.iter().filter(|c| !drop_streams.contains(&c.stream_index)).count()
    } else {
        0
    };
    println!(
        "         {kept_audio} of {} sound track(s), {kept_caps} of {} caption stream(s){}",
        src.audios.len(),
        src.captions.len(),
        match (to_ts, tables) {
            (true, smartcut_core::si::Tables::Partial) => ", written as a partial transport stream",
            (true, smartcut_core::si::Tables::Broadcast) => ", the broadcast's own tables",
            (true, smartcut_core::si::Tables::Muxer) => ", tables left to the muxer",
            (false, _) => "",
        },
    );
    cut(
        &src,
        &plans,
        &out,
        &CutOptions {
            audio_mode,
            audio_codec,
            aac,
            audio_channels,
            audio_bit_rate,
            audio_sample_rate,
            audio_bits,
            drop_streams,
            tables,
            ..Default::default()
        },
    )?;
    // The sidecar exists for the ARIB workflow, where what is wanted beside
    // the video is an AAC elementary stream. A cut written in another codec
    // has no AAC in it to put there, and a `.aac` holding AC-3 would be worse
    // than no file at all.
    let es_is_aac =
        matches!(audio_codec, smartcut_core::AudioCodec::Source | smartcut_core::AudioCodec::Aac);
    if audio_es && es_is_aac {
        let beside = std::path::Path::new(&out).with_extension("aac");
        let n = smartcut_core::write_audio_es(&out, &beside.to_string_lossy(), aac)?;
        println!("wrote {} ({n} packets)", beside.display());
    } else if audio_es {
        eprintln!(
            "note: --audio-es writes the sound out as an AAC elementary stream, and this cut's \
             sound is {}. No sidecar was written.",
            audio_codec.as_str(),
        );
    }
    println!("wrote {out}");
    Ok(())
}
