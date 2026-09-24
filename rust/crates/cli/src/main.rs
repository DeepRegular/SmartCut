use anyhow::{bail, Context, Result};
use smartcut_core::{index, plan_on, CutOptions, PlanOptions};

/// HH:MM:SS.mmm.
///
/// Rounded to milliseconds once, and every field read back out of that
/// count. Field by field the seconds were rounded on their own while the
/// hours and minutes were floored, so a time a hair under a minute came out
/// as `00:00:60.000` -- a reading no clock has.
fn fmt_hms(t: f64) -> String {
    let sign = if t < 0.0 { "-" } else { "" };
    let ms = (t.abs() * 1000.0).round() as i64;
    format!(
        "{sign}{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

fn parse_time(s: &str) -> Result<f64> {
    let mut total = 0.0;
    for part in s.trim().split(':') {
        let v: f64 = part
            .parse()
            .with_context(|| format!("bad timestamp {s:?}"))?;
        total = total * 60.0 + v;
    }
    Ok(total)
}

/// A range, checked for being one.
///
/// A start at or after the end is not a short range, it is a mistake -- and
/// one that used to be carried all the way through to a nought-byte file
/// reported as written, because nothing between here and the muxer had
/// anything to say about a range that selects nothing.
fn parse_range(s: &str) -> Result<(f64, f64)> {
    let (a, b) = s
        .split_once('-')
        .with_context(|| format!("bad range {s:?}, want START-END"))?;
    let (start, end) = (parse_time(a)?, parse_time(b)?);
    if start.is_nan() || end.is_nan() || end <= start {
        bail!(
            "range {s:?}: {} does not come before {}",
            fmt_hms(start),
            fmt_hms(end)
        );
    }
    Ok((start, end))
}

/// Sort the kept ranges and join any that touch or overlap.
///
/// Two ranges that overlap describe one stretch of the recording, and asking
/// for both wrote the shared part twice -- once per range, spliced to itself.
/// `--cut` has always been normalised, by [`complement`] having to work out
/// what is left; this is the same courtesy for the ranges given directly.
fn merge_ranges(mut keeps: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    keeps.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(keeps.len());
    for (start, end) in keeps {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
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
            None => bail!(
                "--title {n}: this disc holds {} recording(s)",
                entries.len()
            ),
        };
    }
    entries
        .iter()
        .find(|e| e.label.contains(want) || e.path.contains(want))
        .map(Some)
        .ok_or_else(|| {
            anyhow::anyhow!("--title {want:?}: no recording on this disc is called that")
        })
}

/// A disc named, or a size in bytes.
///
/// `bd25` and the rest are the discs; a plain number is bytes, which is what
/// somebody writing onto something else -- a card, a share, half a disc --
/// has. Sizes are the disc's own, which are not powers of two: a "25GB"
/// Blu-ray holds 25,025,314,816 bytes, i.e. 23.3 of what a file manager calls
/// a gigabyte.
fn disc_size(v: &str) -> Result<u64> {
    let lower = v.trim().to_ascii_lowercase();
    let key = lower.trim_start_matches("bd").trim_start_matches('-');
    if let Some(d) = smartcut_core::fit::DISCS
        .iter()
        .find(|d| (d.bytes / 1_000_000_000).to_string() == key)
    {
        return Ok(d.bytes);
    }
    // **`bd30` is a disc that was misremembered, not a size of thirty bytes.**
    // Anything named like one of the discs is answered as a disc, or the
    // message sends somebody looking for what is wrong with their number.
    let discs = || {
        smartcut_core::fit::DISCS
            .iter()
            .map(|d| format!("bd{}", d.bytes / 1_000_000_000))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if lower.starts_with("bd") {
        bail!("--fit {v:?}: no such disc. The discs are {}", discs());
    }
    let n: u64 = key
        .parse()
        .with_context(|| format!("--fit wants {} or a size in bytes, got {v:?}", discs()))?;
    if n < 1_000_000 {
        bail!("--fit {v:?}: that is not a size anything can be written onto");
    }
    Ok(n)
}

fn list_disc(input: &str, disc: &smartcut_core::disc::Disc) {
    println!("disc  : {input}");
    println!("        {} -- {}", disc.shape.as_str(), disc.label);
    println!("        {} recording(s)", disc.entries.len());
    // Said here because the rows below are readable and what they name is
    // not: the index files a recorder writes are in the clear and every
    // stream beside them is encrypted, so a disc like this lists itself
    // perfectly and opens nothing.
    if disc.protected {
        println!("        AACS -- the streams are encrypted; only the index is readable");
    }
    println!();
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
        println!(
            "{tick}{:3}  {}  {}{marks}",
            i + 1,
            fmt_hms(e.duration),
            e.label
        );
        // Only where there is a choice to make. A clip with one sound track
        // and nothing else is a clip the list has already described.
        if e.tracks.iter().filter(|t| t.kind != "video").count() > 1 {
            for t in &e.tracks {
                if t.kind == "video" {
                    continue;
                }
                let lang = t
                    .language
                    .as_deref()
                    .map(|l| format!(" {l}"))
                    .unwrap_or_default();
                let gone = if t.carried {
                    ""
                } else {
                    " -- a cut cannot carry this"
                };
                println!("       0x{:04x}  {}{lang}{gone}", t.pid, t.detail);
            }
        }
    }
    println!("\nname one with --title N to open it");
}

/// How to call this, for a run with no recording named. `--help` puts the
/// options under it; see [`help`].
fn usage() -> String {
    "usage: smartcut <input> [options] -o OUTPUT\n       \
     smartcut <input> [options] --bdav FOLDER\n\
     <input> is a recording, or a disc (a BDAV, BDMV or VIDEO_TS folder, or an .iso\n\
     of one), whose recordings are listed when no --title is given.\n\
     smartcut --help lists every option."
        .to_string()
}

/// The options, grouped as `docs/user-guide/cli.md` groups them, one to a
/// line and wrapped to a terminal's eighty columns. What used to be here was
/// the same material as paragraphs, and came out as fourteen lines of up to
/// nine hundred characters each.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Where to cut",
        &[
            ("--keep START-END", "A range to keep, in seconds or as 1:23:45.6. Repeatable"),
            ("--cut START-END", "A range to drop. Repeatable. Not with --keep"),
            ("--no-open-gop", "Never start a copy at an open GOP"),
            (
                "--clean-joins",
                "Re-encode up to 2 s at the start of each range, so that no picture \
                 is decoded out of order",
            ),
        ],
    ),
    (
        "Where the output goes",
        &[
            (
                "-o, --output OUTPUT",
                "The output file; its extension picks the container. Not the recording \
                 being read, and not a URL",
            ),
            ("--drop-stream INDEX", "Leave one of the recording's streams out. Repeatable"),
            ("--title N|NAME", "Which recording on a disc. Left out, the disc's recordings are listed"),
        ],
    ),
    (
        "Joining recordings",
        &[
            ("--join RECORDING", "Another recording, written after this one, whole. Repeatable"),
            ("--master N", "Which of them the output takes its shape from, counting from 1"),
            (
                "--transition KIND",
                "none, fade-black, fade-white, dissolve, wipe-left|right|top|bottom, \
                 slide-left|right|top|bottom. At every join",
            ),
            ("--transition-seconds S", "How long it runs, up to 30. Default 1"),
            ("--transition-easing C[:M]", "The curve, and in|out|in-out|out-in"),
            ("--transition-image FILE", "A still laid over each crossing"),
            ("--join-fade-out S", "Fade the sound out at the end of each clip (0-10)"),
            ("--join-fade-in S", "...and in at the start of the next one (0-10)"),
        ],
    ),
    (
        "Audio",
        &[
            (
                "--audio-mode MODE",
                "smart (default): rebuild only the frames a boundary falls inside; \
                 copy; reencode",
            ),
            ("--audio-codec CODEC", "source (default), aac, lpcm, ac3 or dts"),
            ("--audio-channels N", "1 to 8. Fewer than the recording's is a downmix"),
            ("--audio-samplerate RATE", "48k or 48000, and so on"),
            ("--audio-bits 16|24", "Sample width, for linear PCM"),
            ("--audio-bitrate RATE", "192k or 192000, and so on, when re-encoding"),
            ("--aac auto|mpeg2|mpeg4", "Which AAC the frames written announce themselves as"),
            ("--audio-fade S", "Fade the sound into and out of every seam (0-10)"),
            ("--audio-es", "Also write the AAC beside the output, as a bare stream"),
            (
                "--sound-only",
                "Write one sound track and no pictures: .aac, .ac3, .mp2, .mp3, \
                 .dts, .m4a or .wav",
            ),
        ],
    ),
    (
        "Writing a broadcast .ts",
        &[
            (
                "--tables KIND",
                "broadcast (default for .ts), partial (default for .m2ts), or muxer",
            ),
            (
                "--no-data-broadcast",
                "Leave out the data broadcast (the d button's pages), which a .ts otherwise carries",
            ),
            ("--data-broadcast", "Ask for it outright: a cut that cannot carry it says so"),
        ],
    ),
    (
        "Writing a disc (BDAV)",
        &[
            ("--bdav FOLDER", "Write the cut onto a disc of recordings in FOLDER, instead of -o"),
            (
                "--fit SIZE",
                "bd25, bd50, bd100, bd128 or bytes: shrink the pictures to fit. MPEG-2 only",
            ),
            ("--video-share 0.35..1", "The share to shrink the pictures to, named outright"),
            ("--iso 2.50|2.60", "Wrap the finished disc in a UDF image, FOLDER.iso"),
            ("--iso-access ACCESS", "read-only (default) or overwritable, for a recorder"),
            ("--iso-only", "Take the folder away once the image holds it"),
            ("--disc-title NAME", "What the disc is called"),
            ("--programme NAME", "What this recording is called on the disc"),
            ("--channel NAME[,N]", "The channel, and its number"),
            ("--about TEXT", "What the programme was"),
            ("--made \"Y-M-D H:M:S\"", "When it was recorded"),
        ],
    ),
    (
        "Looking before writing",
        &[
            ("--analyze", "Print the plan and write no cut"),
            ("--detect-cm", "Look for the commercial breaks"),
            ("--logo", "Use the station logo in that as well"),
            ("--inserts", "And report a channel's own few-second idents"),
            ("--scenes", "List the scene changes"),
            ("--preview TIME", "Save the picture at TIME as a JPEG (or -o)"),
            ("--cut-near TIME", "Where the nearest picture change to TIME is"),
        ],
    ),
    (
        "Index and proxy",
        &[
            ("--seek-index PATH", "Keep the seek index here, and read it back next time"),
            ("--index KIND", "auto (default), disc, scan or container"),
            ("--proxy", "Build the editing proxy and stop (.proxy.mp4 or -o)"),
            ("--as-proxy", "Treat the input as a proxy"),
        ],
    ),
    (
        "Subtitles and VC-1 discs",
        &[
            ("--subtitles WHERE", "pgs (default, in the cut), beside (.idx/.sub), sup"),
            ("--drop-subpicture ID", "Leave out one of a DVD's subtitle streams, as 0x20"),
            ("--vc1-quant 3..31", "How finely VC-1 join pictures are written. Default 4"),
        ],
    ),
];

/// Everything, for `--help`.
fn help() -> String {
    const WIDTH: usize = 80;
    const FLAG: usize = 27;
    let mut out = usage();
    out.push('\n');
    for (group, options) in HELP {
        out.push('\n');
        out.push_str(group);
        out.push_str(":\n");
        for (flag, says) in options.iter() {
            let words: Vec<&str> = says.split_whitespace().collect();
            let mut line = format!("  {flag}");
            // A flag too long for its column has its words start on the line
            // below, where the column is.
            if line.chars().count() >= FLAG {
                out.push_str(&line);
                out.push('\n');
                line = String::new();
            }
            let mut first = true;
            for word in words {
                let room = line.chars().count().max(FLAG);
                if !first && room + 1 + word.chars().count() > WIDTH {
                    out.push_str(&line);
                    out.push('\n');
                    line = String::new();
                    first = true;
                }
                if first {
                    let pad = FLAG - line.chars().count().min(FLAG);
                    line.push_str(&" ".repeat(pad));
                } else {
                    line.push(' ');
                }
                line.push_str(word);
                first = false;
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str("\nEvery option, with what it costs and why: docs/user-guide/cli.md\n");
    out
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = None;
    let mut keeps: Vec<(f64, f64)> = Vec::new();
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    // Recordings written into the same output after the first, in the order
    // they are named. See `smartcut_core::cut::Reel`.
    let mut joined: Vec<String> = Vec::new();
    // Which of them the file takes its shape from, counted from nought here
    // and from one on the command line.
    let mut master = 0usize;
    // What happens at every join between two of them. One setting for the
    // whole list rather than one per join: a command line naming a
    // transition per clip would be a project file with a worse syntax.
    let mut crossing = smartcut_core::transition::Crossing::None;
    let mut crossing_secs = 1.0f64;
    let mut easing = smartcut_core::transition::Easing::default();
    let mut crossing_image: Option<String> = None;
    // How long the sound takes to leave at the end of a clip and to come
    // back at the start of the next. Nought, like the fade at a cut: what a
    // fade fades is the programme.
    let mut join_fade_out = 0.0f64;
    let mut join_fade_in = 0.0f64;
    let mut allow_open_gop = true;
    let mut clean_join = false;
    let mut output: Option<String> = None;
    let mut analyze = false;
    let mut index_kind = "auto".to_string();
    let mut seek_index: Option<String> = None;
    let mut preview_at: Option<f64> = None;
    let mut make_proxy = false;
    // Which recording on a disc. Nothing else takes one.
    let mut title: Option<String> = None;
    let mut as_proxy = false;
    let mut detect_cm = false;
    let mut scenes = false;
    let mut audio_es = false;
    // Write the sound and no pictures. See `smartcut_core::sound`.
    let mut sound_only = false;
    let mut cut_near: Option<f64> = None;
    let mut find_inserts = false;
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
    let mut vc1_quant: Option<u8> = None;
    let mut audio_fade = 0.0f64;
    let mut audio_sample_rate: Option<u32> = None;
    let mut audio_bits: Option<u8> = None;
    // Everything the recording carries is written unless it is named here.
    let mut drop_streams: Vec<usize> = Vec::new();
    let mut drop_subpictures: Vec<i32> = Vec::new();
    let mut subtitles = smartcut_core::cut::Subtitles::default();
    let mut subtitles_given = false;
    // Unsaid, the shape follows where the cut is going: a `.ts` carries the
    // broadcast's own tables and a Blu-ray clip is a partial transport
    // stream. See `smartcut_core::tables_for`.
    let mut tables: Option<smartcut_core::si::Tables> = None;
    // Whether the recording's data broadcast travels with the cut. Nothing
    // said leaves it to the engine, which carries it wherever it can be
    // carried -- a cut is meant to be the recording, shorter, and that was
    // in the recording. `--no-data-broadcast` is how to say no: a carousel
    // is between a hundredth and a fifth of what a multiplex spends. See
    // `smartcut_core::carousel`.
    let mut data_broadcast: Option<bool> = None;
    // What the output has to fit, and what that comes to for the pictures.
    // See `smartcut_core::fit` and `CutOptions::video_share`.
    let mut fit: Option<u64> = None;
    let mut video_share: Option<f64> = None;
    // Where a disc of recordings is being built, and what to call it and the
    // recording going onto it. See `smartcut_core::bdav`.
    let mut bdav: Option<String> = None;
    let mut disc_title: Option<String> = None;
    let mut iso: Option<smartcut_core::udfw::Revision> = None;
    // And whether the folder goes once the image has been made of it.
    let mut iso_only = false;
    // What the image says may be done to the disc it is burned onto; see
    // `smartcut_core::udfw::Access`.
    let mut iso_access = smartcut_core::udfw::Access::default();
    let mut iso_access_given = false;
    let mut programme: Option<String> = None;
    let mut given_channel: Option<String> = None;
    let mut given_number: Option<u16> = None;
    let mut about: Option<String> = None;
    let mut given_made: Option<smartcut_core::si::Began> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            // Asked for rather than stumbled into, so it goes to stdout and
            // the run ends well. Before everything else: somebody who types
            // this wants the text, not an argument three places later being
            // held against them.
            "--help" | "-h" => {
                print!("{}", help());
                return Ok(());
            }
            "--keep" => {
                i += 1;
                keeps.push(parse_range(args.get(i).context("--keep needs a range")?)?);
            }
            "--cut" => {
                i += 1;
                cuts.push(parse_range(args.get(i).context("--cut needs a range")?)?);
            }
            "--join" => {
                i += 1;
                joined.push(args.get(i).context("--join needs a recording")?.clone());
            }
            "--master" => {
                i += 1;
                let v = args.get(i).context("--master needs a number")?;
                master = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n >= 1)
                    .with_context(|| format!("--master counts from 1, got {v:?}"))?
                    - 1;
            }
            "--transition" => {
                i += 1;
                let v = args.get(i).context("--transition needs a name")?;
                crossing = smartcut_core::transition::Crossing::parse(v)
                    .with_context(|| format!("--transition does not know {v:?}"))?;
            }
            "--transition-seconds" => {
                i += 1;
                let v = args.get(i).context("--transition-seconds needs a number")?;
                crossing_secs = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| *s > 0.0 && *s <= smartcut_core::transition::LONGEST)
                    .with_context(|| {
                        format!(
                            "--transition-seconds wants more than 0 and up to {}, got {v:?}",
                            smartcut_core::transition::LONGEST
                        )
                    })?;
            }
            "--join-fade-out" | "--join-fade-in" => {
                let which = args[i].clone();
                i += 1;
                let v = args.get(i).with_context(|| format!("{which} needs a length"))?;
                let secs = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| (0.0..=10.0).contains(s))
                    .with_context(|| format!("{which} wants 0..10 seconds, got {v:?}"))?;
                if which.ends_with("out") {
                    join_fade_out = secs;
                } else {
                    join_fade_in = secs;
                }
            }
            "--transition-image" => {
                i += 1;
                crossing_image =
                    Some(args.get(i).context("--transition-image needs a file")?.clone());
            }
            "--transition-easing" => {
                i += 1;
                let v = args.get(i).context("--transition-easing needs a curve")?;
                let (curve, mode) = v.split_once(':').unwrap_or((v.as_str(), "in"));
                easing = smartcut_core::transition::Easing::try_parse(curve, mode)
                    .with_context(|| {
                        format!(
                            "--transition-easing does not know {v:?}: a curve (none, back, \
                             bounce, circle, elastic, exponential, power, sine, quadratic, \
                             cubic, quartic, quintic) and :in, :out, :in-out or :out-in"
                        )
                    })?;
            }
            "--no-open-gop" => allow_open_gop = false,
            "--clean-joins" => clean_join = true,
            "--detect-cm" => detect_cm = true,
            "--scenes" => scenes = true,
            "--audio-es" => audio_es = true,
            "--sound-only" => sound_only = true,
            "--cut-near" => {
                i += 1;
                cut_near = Some(parse_time(args.get(i).context("--cut-near needs a time")?)?);
            }
            "--logo" => use_logo = true,
            "--inserts" => find_inserts = true,
            "--title" => {
                i += 1;
                title = Some(
                    args.get(i)
                        .context("--title needs a number or a name")?
                        .clone(),
                );
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
            "--audio-fade" => {
                i += 1;
                let v = args
                    .get(i)
                    .context("--audio-fade needs a length in seconds")?;
                audio_fade = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| (0.0..=10.0).contains(s))
                    .with_context(|| format!("--audio-fade wants 0..10 seconds, got {v:?}"))?;
            }
            "--vc1-quant" => {
                i += 1;
                let v = args.get(i).context("--vc1-quant needs a step")?;
                vc1_quant = Some(
                    v.parse::<u8>()
                        .ok()
                        .filter(|&q| (3..=31).contains(&q))
                        .with_context(|| format!("--vc1-quant wants 3..31, got {v:?}"))?,
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
                index_kind = args
                    .get(i)
                    .context("--index needs auto|disc|scan|container")?
                    .clone();
            }
            "--seek-index" => {
                i += 1;
                seek_index = Some(args.get(i).context("--seek-index needs a path")?.clone());
            }
            "--drop-stream" => {
                i += 1;
                let v = args.get(i).context("--drop-stream needs a stream index")?;
                drop_streams.push(
                    v.parse::<usize>()
                        .with_context(|| format!("--drop-stream wants a number, got {v:?}"))?,
                );
            }
            "--subtitles" => {
                i += 1;
                let v = args
                    .get(i)
                    .context("--subtitles needs pgs, beside or sup")?;
                subtitles_given = true;
                subtitles = match v.as_str() {
                    "beside" => smartcut_core::cut::Subtitles::Beside,
                    "pgs" => smartcut_core::cut::Subtitles::Pgs,
                    "sup" => smartcut_core::cut::Subtitles::Sup,
                    other => bail!("--subtitles wants pgs, beside or sup, got {other:?}"),
                };
            }
            "--drop-subpicture" => {
                i += 1;
                let v = args
                    .get(i)
                    .context("--drop-subpicture needs a substream id, as 0x20")?;
                let n = v.strip_prefix("0x").unwrap_or(v);
                drop_subpictures.push(
                    i32::from_str_radix(n, 16)
                        .with_context(|| format!("--drop-subpicture wants an id, got {v:?}"))?,
                );
            }
            "--tables" => {
                i += 1;
                let v = args
                    .get(i)
                    .context("--tables needs partial, broadcast or muxer")?;
                tables = Some(match v.as_str() {
                    "partial" => smartcut_core::si::Tables::Partial,
                    "broadcast" => smartcut_core::si::Tables::Broadcast,
                    "muxer" | "none" => smartcut_core::si::Tables::Muxer,
                    other => bail!("--tables wants partial, broadcast or muxer, got {other:?}"),
                });
            }
            // What the option was called when there were only two answers.
            "--no-tables" => tables = Some(smartcut_core::si::Tables::Muxer),
            // Onto a disc of a given size, or at a share named outright.
            "--fit" => {
                i += 1;
                let v = args.get(i).context("--fit needs a size")?;
                fit = Some(disc_size(v)?);
            }
            "--video-share" => {
                i += 1;
                let v = args.get(i).context("--video-share needs a share")?;
                let share: f64 = v.parse().context("--video-share wants 0.35..1")?;
                if !(smartcut_core::fit::FLOOR..=1.0).contains(&share) {
                    bail!("--video-share wants {}..1, got {share}", smartcut_core::fit::FLOOR);
                }
                video_share = Some(share);
            }
            "--data-broadcast" => data_broadcast = Some(true),
            "--no-data-broadcast" => data_broadcast = Some(false),
            "--proxy" => make_proxy = true,
            "--as-proxy" => as_proxy = true,
            "--analyze" => analyze = true,
            "-o" | "--output" => {
                i += 1;
                output = Some(args.get(i).context("-o needs a path")?.clone());
            }
            // Onto a disc rather than into a file. The name of the file is
            // the disc's to decide -- a recording on one is `00001.m2ts` --
            // and what the programme is called goes in the index beside it.
            "--bdav" => {
                i += 1;
                // Without a separator on the end: the image is named by
                // appending `.iso` to this, and `disc/` would make it
                // `disc/.iso`, inside the folder it is an image of.
                let v = args.get(i).context("--bdav needs a folder")?;
                let trimmed = v.trim_end_matches(['/', '\\']);
                bdav = Some(if trimmed.is_empty() { v.clone() } else { trimmed.to_string() });
            }
            // And whether to wrap the finished disc in an image. The
            // folder is what is written either way; this is the burner's
            // copy of it.
            "--iso" => {
                i += 1;
                let v = args.get(i).context("--iso needs a UDF revision")?;
                iso = Some(
                    smartcut_core::udfw::Revision::parse(v)
                        .with_context(|| format!("--iso wants 2.50 or 2.60, got {v:?}"))?,
                );
            }
            // The image on its own: the folder is still written, still what
            // the image is made of, and taken away once the image holds it.
            "--iso-only" => iso_only = true,
            // What the burned disc is to say about itself. Read-only unless
            // asked, which is what a burned disc is.
            "--iso-access" => {
                i += 1;
                let v = args.get(i).context("--iso-access needs read-only or overwritable")?;
                iso_access = smartcut_core::udfw::Access::parse(v).with_context(|| {
                    format!("--iso-access wants read-only or overwritable, got {v:?}")
                })?;
                iso_access_given = true;
            }
            "--disc-title" => {
                i += 1;
                disc_title = Some(args.get(i).context("--disc-title needs a name")?.clone());
            }
            "--programme" => {
                i += 1;
                programme = Some(args.get(i).context("--programme needs a name")?.clone());
            }
            // The other two things a disc's index says about a recording,
            // for a stream that no longer carries them: a recording that has
            // been through tools that kept none of its tables still belongs
            // on a disc under the channel it came off.
            "--channel" => {
                i += 1;
                let v = args.get(i).context("--channel needs a name")?.clone();
                // `衛星第一` or `衛星第一,161`: the number is what a viewer
                // knows the channel by, and is optional because only
                // satellite has one this can be sure of.
                match v.split_once(',') {
                    Some((name, n)) => {
                        given_channel = Some(name.to_string());
                        given_number = Some(n.trim().parse().with_context(|| {
                            format!("--channel {v:?}: the number after the comma is not one")
                        })?);
                    }
                    None => given_channel = Some(v),
                }
            }
            "--about" => {
                i += 1;
                about = Some(args.get(i).context("--about needs some text")?.clone());
            }
            "--made" => {
                i += 1;
                let v = args.get(i).context("--made needs a date and time")?;
                given_made = Some(
                    smartcut_core::si::Began::parse(v)
                        .with_context(|| format!("--made wants 2026-08-17 01:00:00, got {v:?}"))?,
                );
            }
            a if a.starts_with('-') => bail!("unknown option {a}"),
            // One recording per run; the rest of a join is named by `--join`.
            // Taken quietly, the second name replaced the first and a
            // different recording was cut from the one the user began with.
            a if input.is_some() => bail!(
                "one recording at a time: {a:?} follows {:?} -- to join several, name the \
                 others with --join",
                input.as_deref().unwrap_or_default()
            ),
            a => input = Some(a.to_string()),
        }
        i += 1;
    }
    // What a disc's index says, which a run that writes no disc would take
    // and do nothing with.
    if bdav.is_none() {
        let disc_only = [
            ("--iso", iso.is_some()),
            ("--disc-title", disc_title.is_some()),
            ("--programme", programme.is_some()),
            ("--channel", given_channel.is_some()),
            ("--about", about.is_some()),
            ("--made", given_made.is_some()),
        ];
        if let Some((name, _)) = disc_only.iter().find(|(_, given)| *given) {
            bail!("{name} describes a disc and needs --bdav <folder>");
        }
    }
    // A disc names its own recordings, so `-o` would be overwritten by it.
    if bdav.is_some() && output.is_some() {
        bail!("-o and --bdav are two places to write the same cut: give one");
    }
    // The elementary stream would land in the disc's STREAM folder, and from
    // there in its image, as a file no player asked for.
    if bdav.is_some() && audio_es {
        bail!("--audio-es writes a file beside the cut, which does not go onto a disc");
    }
    // The size is worked out from the first recording's ranges before the
    // others are opened, so it would leave them out of the sum and say a
    // join fits that does not. And a cut of sound alone has no pictures to
    // shrink.
    if fit.is_some() && !joined.is_empty() {
        bail!("--fit cannot yet count the recordings of a --join: fit them one at a time");
    }
    // What happens between recordings, asked of a run that has only one:
    // nothing reads it, and a run that ignores what it was told should say
    // so rather than succeed.
    if joined.is_empty() {
        let join_only = [
            ("--transition", crossing != smartcut_core::transition::Crossing::None),
            ("--transition-seconds", crossing_secs != 1.0),
            ("--transition-easing", easing != smartcut_core::transition::Easing::default()),
            ("--transition-image", crossing_image.is_some()),
            ("--join-fade-out", join_fade_out > 0.0),
            ("--join-fade-in", join_fade_in > 0.0),
        ];
        if let Some((name, _)) = join_only.iter().find(|(_, given)| *given) {
            bail!("{name} is about the joins between recordings and needs --join");
        }
    }
    // One past the last recording was taken as the last, without a word.
    if master > joined.len() {
        bail!(
            "--master {} names a recording there is not: there are {}",
            master + 1,
            joined.len() + 1
        );
    }
    // Two answers to one question: the share would have been applied where
    // the cut already fits, and replaced without a word where it does not.
    if fit.is_some() && video_share.is_some() {
        bail!("--fit works the share out and --video-share names it: give one");
    }
    if sound_only && (video_share.is_some() || audio_es) {
        bail!("--sound-only writes the sound alone: --video-share and --audio-es have nothing to act on");
    }
    if fit.is_some() && sound_only {
        bail!("--fit shrinks the pictures, and --sound-only writes none");
    }
    // The rest of what only a file with pictures in it has: its tables, its
    // subtitles, its data broadcast and how its pictures are joined.
    if sound_only {
        let moot = [
            (tables.is_some(), "--tables"),
            (data_broadcast.is_some(), "--data-broadcast / --no-data-broadcast"),
            (subtitles_given, "--subtitles"),
            (!drop_subpictures.is_empty(), "--drop-subpicture"),
            (vc1_quant.is_some(), "--vc1-quant"),
            (clean_join, "--clean-joins"),
            (!allow_open_gop, "--no-open-gop"),
        ];
        if let Some((_, name)) = moot.iter().find(|(given, _)| *given) {
            bail!("--sound-only writes the sound alone: {name} has nothing to act on");
        }
    }
    // Two ways of reading the recording for commercials, which only the
    // detection does.
    if (use_logo || find_inserts) && !detect_cm {
        bail!("--logo and --inserts are ways of running --detect-cm: give it too");
    }
    // Nothing to take the folder away *for*: the image is what is kept in
    // its place, and without one this would be a run that deletes its own
    // output.
    if iso_only && iso.is_none() {
        bail!("--iso-only needs --iso 2.50|2.60: the image is made of the folder");
    }
    if iso_access_given && iso.is_none() {
        bail!("--iso-access needs --iso 2.50|2.60: it is a thing the image says");
    }
    // A disc is a list of recordings with pictures in them; a clip of sound
    // alone would be written into its folder with nothing to describe it.
    if sound_only && bdav.is_some() {
        bail!("--sound-only writes an audio file, which does not go onto a disc: use -o");
    }
    let Some(input) = input else { bail!(usage()) };
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
    // What the disc this came off said the programme was, for a cut of it
    // being written onto a disc of its own.
    let mut off_a_disc: Option<smartcut_core::disc::Entry> = None;
    let input = match on_a_disc(&input)? {
        // A recording is not a disc, and there is nothing on it to pick
        // between. Carried silently, `--title` then named nothing and the
        // whole recording was cut instead -- which is a different job from
        // the one that was asked for.
        None if title.is_some() => bail!(
            "--title {:?}: {input} is a recording rather than a disc, so there is nothing on \
             it to choose between",
            title.unwrap_or_default()
        ),
        None => input,
        Some(disc) => match pick(&disc.entries, title.as_deref())? {
            Some(entry) => {
                println!("title : {}", entry.label);
                // What the disc's own playlist says about the recording
                // beside its name. Worth printing because it is what a cut
                // of this recording carries onto a disc of its own -- see
                // `--bdav` -- and because a disc is the only place a
                // recording is described at all.
                let mut about: Vec<String> = Vec::new();
                if let Some(channel) = &entry.channel {
                    about.push(match entry.channel_number {
                        0 => channel.clone(),
                        n => format!("{channel} ({n})"),
                    });
                }
                if let Some(made) = entry.made {
                    about.push(made.to_string());
                }
                if !about.is_empty() {
                    println!("        {}", about.join("  "));
                }
                if let Some(text) = &entry.description {
                    // One line of it: a description runs to several hundred
                    // characters and this is a heading, not the programme
                    // guide.
                    let flat = text.replace('\n', " ");
                    let short: String = flat.chars().take(100).collect();
                    let more = if flat.chars().count() > 100 {
                        "…"
                    } else {
                        ""
                    };
                    println!("        {short}{more}");
                }
                chapters = entry.marks.iter().map(|m| entry.start + m).collect();
                off_a_disc = Some(entry.clone());
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

    // `auto` asks whoever already knows before reading anything: the disc's
    // own entry-point map, then the container's seek table, and the walk over
    // the packets last. Which one answered is printed with the access points,
    // so a run that took a second instead of nine minutes says why.
    let index_source: Box<dyn index::IndexSource> = match index_kind.as_str() {
        // A recording that is not a clip on a disc has no disc map to ask,
        // and asking opened it and read its head before saying so -- once for
        // the map, once for the container, once more for the walk.
        "auto" if smartcut_core::disc::clip_on_a_disc(&input).is_none() => {
            Box::new(index::ContainerIndex)
        }
        "auto" | "disc" => Box::new(index::DiscIndex),
        "scan" => Box::new(index::PacketScan),
        "container" => Box::new(index::ContainerIndex),
        other => bail!("unknown --index {other}; want auto, disc, scan or container"),
    };
    let fall_back_to_the_walk = index_kind == "auto";
    // Where the first choice was the container's table, the fallback goes
    // straight to the walk: asking the container again is the same answer.
    let asked_the_container =
        index_kind == "auto" && smartcut_core::disc::clip_on_a_disc(&input).is_none();
    // A seek index written by an earlier run stands in for the walk over the
    // packets. Reading it back is the whole point: it is the same answer, and
    // it did not cost a pass over the recording to get.
    let index_file = seek_index.as_ref().map(std::path::PathBuf::from);
    // One that cannot be read -- written by another version, or damaged --
    // is written again rather than stopping the run: it is a cache, and the
    // walk it stands in for is still there to be done.
    let held = match &index_file {
        Some(p) if p.is_file() => match smartcut_core::SeekIndex::load(p) {
            Ok(held) => Some(held),
            Err(e) => {
                eprintln!("note: {e}; it is made again");
                None
            }
        },
        _ => None,
    };
    // `--as-proxy` reads the input as a proxy of something else: same file,
    // but its timestamps are the recording's and must not be rebased again.
    let mut src = if as_proxy {
        smartcut_core::proxy::open(&input)?
    } else if let Some(ix) = &held {
        smartcut_core::scan_with(&input, ix)?
    } else {
        match smartcut_core::scan_with(&input, index_source.as_ref()) {
            Ok(src) => src,
            Err(_) if fall_back_to_the_walk && asked_the_container => {
                smartcut_core::scan_with(&input, &index::PacketScan)?
            }
            Err(_) if fall_back_to_the_walk => {
                smartcut_core::scan_with(&input, &index::ContainerIndex)
                    .or_else(|_| smartcut_core::scan_with(&input, &index::PacketScan))?
            }
            Err(e) => return Err(e),
        }
    };
    // Written straight away, so that a run which never builds a thumbnail
    // track still leaves the expensive half behind. The `--scenes` path
    // writes it again with the track once it has one.
    let writing = index_file.filter(|_| held.is_none() && !as_proxy);
    if let Some(p) = &writing {
        src.input.refuse_as_output(&p.to_string_lossy())?;
        smartcut_core::SeekIndex::of(&src, None).save(p)?;
    }
    let v = &src.video;
    println!("input : {}", src.path);
    println!(
        "        {} {}x{} {:.3}fps{}  has_b_frames={}  dur={:.3}s  start={:.3}s",
        v.codec,
        v.width,
        v.height,
        v.frame_rate,
        // The rate is the container's, and where the pictures do not keep to
        // it that is worth saying beside it rather than leaving the reader
        // to wonder why the frame counts below do not add up. See
        // [`smartcut_core::VideoInfo::variable_rate`].
        if v.variable_rate { " (declared; the pictures vary)" } else { "" },
        v.has_b_frames,
        src.duration,
        src.start_time
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
    let form = match smartcut_core::aac::of_source(&src) {
        Some(f) => format!("  {}", f.as_str()),
        None => String::new(),
    };
    for (n, a) in src.audios.iter().enumerate() {
        // The stream index is what names a track to `--drop-stream`, so it
        // is printed even when there is only one.
        let main = if src
            .audio
            .as_ref()
            .is_some_and(|m| m.stream_index == a.stream_index)
        {
            "  main"
        } else {
            ""
        };
        // The broadcaster's own name for the track where it gave one, and
        // the language code otherwise. Both are printed when they are not
        // the same thing: a recording in two languages says `eng 英語` on
        // its second track and one carrying commentary for a viewer who
        // cannot see the picture says `jpn 音声解説`, and the language alone
        // would have called those two the same.
        let said = a.said.as_ref().and_then(|s| s.name.as_deref());
        let lang = match (a.language.as_deref(), said) {
            (Some(code), Some(name)) => format!("  {code} {name}"),
            (Some(code), None) => format!("  {code}"),
            (None, Some(name)) => format!("  {name}"),
            (None, None) => String::new(),
        };
        // The PID only where there is one. A recording out of an MP4 has a
        // track number in that field and calling it a PID would name it
        // something it is not; the stream index names it either way, and is
        // what `--drop-stream` takes.
        let pid = if src.on_a_ts {
            format!(" pid 0x{:04x}", a.pid)
        } else {
            String::new()
        };
        println!(
            "audio{}: {} {}Hz {}{lang}{}{main}   [stream {}{pid}]",
            if src.audios.len() > 1 {
                format!(" {}", n + 1)
            } else {
                "  ".to_string()
            },
            a.codec,
            a.sample_rate,
            smartcut_core::audio::channels_named(a.channels),
            if n == 0 { form.as_str() } else { "" },
            a.stream_index,
        );
    }
    for c in &src.captions {
        let lang = c
            .language
            .as_deref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let pid = if src.on_a_ts {
            format!(" pid 0x{:04x}", c.pid)
        } else {
            String::new()
        };
        // What it is and what it is written in. A station's crawl is named
        // apart from the programme's own subtitles: both are carried, and
        // only one of them is the programme's.
        let what = match c.kind {
            smartcut_core::TextKind::Caption => "caption",
            smartcut_core::TextKind::Superimpose => "crawl  ",
        };
        let form = match c.format {
            smartcut_core::TextFormat::Arib => "ARIB STD-B24",
            smartcut_core::TextFormat::Ttml => "ARIB-TTML",
        };
        println!("{what}:{lang} {form}   [stream {}{pid}]", c.stream_index);
    }
    for g in &src.graphics {
        let lang = g
            .language
            .as_deref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let pid = if src.on_a_ts {
            format!(" pid 0x{:04x}", g.pid)
        } else {
            String::new()
        };
        println!("subtitle:{lang} PGS   [stream {}{pid}]", g.stream_index);
    }
    // Named by the id the disc gives them, which is all they have: see
    // `smartcut_core::SubpictureInfo`. Said to be beside the cut rather than
    // in it, because that is where they go.
    for s in &src.subpictures {
        let lang = s
            .language
            .as_deref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        println!("subtitle:{lang} subpicture   [id 0x{:02x}]", s.id);
    }
    // The data broadcast, unless it has been turned down or the cut is
    // being written in a shape that cannot hold one -- a disc's stream, an
    // MP4, a run that leaves the tables to the muxer. Asked here rather
    // than assumed, because listing a carousel among the streams that
    // travel and then not writing it is the one answer that misleads; where
    // it cannot travel it is said to be left behind, with the rest.
    //
    // A run with nothing to write is only being asked what the recording
    // holds, and the answer to that does not depend on a shape nobody has
    // named. A disc's stream is Blu-ray's own framing whatever the folder
    // is called, which is settled here rather than waiting for the name the
    // disc gives the file.
    let data_travels = data_broadcast != Some(false)
        && match (&bdav, &output) {
            (Some(_), _) => false,
            (None, Some(out)) => smartcut_core::can_carry_data_broadcast(out, tables),
            (None, None) => true,
        };
    if data_travels {
        for d in src.dropped.iter().filter(|d| d.what == "data") {
            println!(
                "data   : carousel   [{}]",
                smartcut_core::track_name(src.on_a_ts, d.pid, d.stream_index)
            );
        }
    }
    // Said out loud rather than dropped in silence: these are streams a cut
    // has no way to carry. See `smartcut_core::DroppedStream`.
    for d in src
        .dropped
        .iter()
        .filter(|d| !(data_travels && d.what == "data"))
    {
        println!(
            "        not carried: {} on {}",
            d.describe(),
            smartcut_core::track_name(src.on_a_ts, d.pid, d.stream_index)
        );
    }

    let open = src.points.iter().filter(|p| p.open_gop()).count();
    let droppable = src
        .points
        .iter()
        .filter(|p| p.open_gop() && p.droppable)
        .count();
    let gaps: Vec<f64> = src
        .points
        .windows(2)
        .map(|w| w[1].time - w[0].time)
        .collect();
    let mean_gop = if gaps.is_empty() {
        0.0
    } else {
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    // An index that did not read the pictures cannot say which GOPs are open,
    // and the points it hands over say "closed" because that is the value a
    // field nobody filled in holds. Only the ones a boundary lands on are
    // measured afterwards -- see `index::refine_leading` -- so "all closed"
    // here would be a claim about a stream nothing has looked at.
    let note = if !src.leading_known && open == 0 {
        "open GOPs measured only where the cut lands".to_string()
    } else if open == 0 {
        "all closed".to_string()
    } else if droppable == open {
        format!("{open} open (leading pictures, droppable)")
    } else if droppable == 0 {
        format!("{open} open (leading pictures, referenced -- cannot start a copy there)")
    } else {
        format!(
            "{open} open ({droppable} droppable, {} referenced)",
            open - droppable
        )
    };
    println!(
        "        {} access points, mean GOP {mean_gop:.3}s, {note}  [{}]",
        src.points.len(),
        src.index_name
    );

    if let Some(at) = cut_near {
        for w in [0.5, 1.0, 2.0] {
            let t = smartcut_core::thumbs::cut_near(&src, at, w, 0.08)?;
            println!("  ±{w:.1}s の窓: {}  ({:+.3}s)", fmt_hms(t), t - at);
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
            src.input.refuse_as_output(&p.to_string_lossy())?;
            smartcut_core::SeekIndex::of(&src, Some(track)).save(p)?;
            println!(
                "シーク用インデックス : {} ({:.1} MB)",
                p.display(),
                std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as f64 / 1e6
            );
        }
        if let Ok(path) = std::env::var("SMARTCUT_SCENES_OUT") {
            let dump: String = track.scenes.iter().map(|t| format!("{t:.3}\n")).collect();
            std::fs::write(path, dump)?;
        }
        for t in track
            .scenes
            .iter()
            .take(if std::env::var_os("SMARTCUT_SCENES_OUT").is_some() {
                0
            } else {
                24
            })
        {
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
        let opts = smartcut_core::DetectOptions {
            find_inserts,
            ..Default::default()
        };
        // The sound and the caption stream out of one read of the recording,
        // as the window reads them. See `cm::silences_and_resets`.
        // A recording with no sound still has captions to read; one whose
        // sound could not be read is an error, not a recording with no breaks.
        let (silences, found) = if src.audio.is_none() {
            (Vec::new(), smartcut_core::caption::resets(&src))
        } else {
            smartcut_core::cm_silences_and_resets(&src, &opts, None)?
        };
        let resets = match found {
            // And read instead of the other two only where the station marks
            // every junction rather than only the places its programme stops
            // and starts; see `cm_marks_every_junction`.
            Ok(r) if smartcut_core::cm_marks_every_junction(&r) => {
                println!("\n字幕リセット : {} 箇所", r.len());
                Some(r)
            }
            Ok(r) => {
                println!(
                    "\n字幕リセット : {} 箇所 — 継ぎ目ごとには打たれていないので、ロゴと無音で判定します",
                    r.len()
                );
                None
            }
            Err(e) => {
                println!("\n字幕リセット : ありません（{e}）");
                None
            }
        };
        // The logo costs two more reads and is the weaker signal where the
        // resets exist, so it is not paid for then.
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
        let cands = smartcut_core::cm_candidates(&silences, &opts);
        // The same reading the window makes, arrived at the same way, so
        // that the two do not answer differently about one recording. The
        // arm that used to be missing here is the third: a logo that was
        // found and never went away is a recording that never left the air,
        // which is an answer -- and falling back to the silences instead
        // gave a programme with no commercials in it a block the window
        // would not have offered.
        let (blocks, how) = match (&resets, &logo) {
            (Some(r), _) => (
                smartcut_core::cm_blocks_from_resets(r, src.duration),
                "（字幕リセット）",
            ),
            (None, Some(l)) if !l.absent.is_empty() || (opts.find_inserts && !l.brief.is_empty()) => (
                smartcut_core::cm_blocks_from_logo(
                    &cands,
                    &l.absent,
                    &l.brief,
                    &opts,
                    3.0,
                    src.duration,
                    Some(&src),
                ),
                "（ロゴ＋無音）",
            ),
            (None, Some(_)) => (Vec::new(), "（ロゴが一度も消えない）"),
            _ => (smartcut_core::cm_blocks(&cands, &opts, 0.6), "（無音のみ）"),
        };
        // Same treatment the window gives them, so what is printed here is
        // what would be marked there.
        let mut blocks = blocks;
        smartcut_core::cm_refine_boundaries(&src, &mut blocks, 0.5, 0.08);
        println!("\nCM ブロック : {} 個{how}", blocks.len());
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
            bail!("--proxy on a recording inside a disc needs -o");
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
        // Not over the recording: `-o` naming it wrote a JPEG where it was.
        src.input.refuse_as_output(&path)?;
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
        merge_ranges(keeps)
    } else {
        vec![(0.0, src.duration)]
    };
    // Cutting the whole of a recording away leaves nothing to write. Said
    // here rather than carried: a plan of no ranges went all the way to a
    // nought-byte file reported as "wrote", which is the same mistake
    // [`parse_range`] stopped a backwards range making.
    if ranges.is_empty() {
        bail!(
            "the cuts cover the whole of {}, so there is nothing left to write",
            fmt_hms(src.duration)
        );
    }
    // A range that starts after the last picture selects nothing at all.
    // Said here, where the recording's length is finally known, rather than
    // left to the encoder to discover: that happened after the output file
    // had been created and a picture pushed through, and came out as
    // "no pictures decoded", which describes the symptom and not the cause.
    if let Some(&(start, end)) = ranges.iter().find(|(start, _)| *start >= src.duration) {
        bail!(
            "range {}-{} begins after the recording ends at {}",
            fmt_hms(start),
            fmt_hms(end),
            fmt_hms(src.duration)
        );
    }

    // A precomputed index knows where the entry points are but not what
    // hangs off them, so measure that for the ones this cut will use.
    if !src.leading_known {
        index::refine_leading(
            &src.input.url,
            &src.video,
            src.start_time,
            src.byte_seekable,
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

    let opts = PlanOptions {
        allow_open_gop,
        clean_join: clean_join.then_some(2.0),
        ..Default::default()
    };
    let plans = plan_on(&src, &ranges, &opts);

    // What is said, as against what is planned. A range asked for past the
    // end of the recording is planned as asked -- the copy stops where the
    // packets do, and a container's length is not always to be believed --
    // but saying so printed a 99899-second output for a two-hour recording.
    // Said up to the recording's end, and never below nought (a start a hair
    // under it printed as -0.000).
    let said = |t: f64| {
        let t = if src.duration > 0.0 { t.min(src.duration) } else { t };
        t.max(0.0)
    };
    let span = |a: f64, b: f64| (said(b) - said(a)).max(0.0);
    let copied: f64 = plans
        .iter()
        .flat_map(|p| &p.segments)
        .filter(|s| s.kind == smartcut_core::SegmentKind::Copy)
        .map(|s| span(s.start, s.end))
        .sum::<f64>()
        + 0.0;
    let enc: f64 = plans
        .iter()
        .flat_map(|p| &p.segments)
        .filter(|s| s.kind != smartcut_core::SegmentKind::Copy)
        .map(|s| span(s.start, s.end))
        // An empty sum of floats is -0.0, which prints with its sign.
        .sum::<f64>()
        + 0.0;
    let total = copied + enc;
    println!("\nplan  : {} range(s), {total:.3}s output", plans.len());
    for p in &plans {
        println!("  keep {:.3} -> {:.3}", said(p.t_in), said(p.t_out));
        for s in &p.segments {
            println!(
                "    {:>8}  {:8.3} -> {:8.3}  ({:6.3}s, {} frames)",
                s.kind.as_str(),
                said(s.start),
                said(s.end),
                span(s.start, s.end),
                s.frames
            );
        }
    }
    if total > 0.0 {
        println!(
            "        copied {copied:.3}s ({:.1}%), re-encoded {enc:.3}s ({:.1}%)",
            100.0 * copied / total,
            100.0 * enc / total
        );
    }
    // What the pictures have to come to, where a size was named. Worked out
    // from the ranges actually being kept rather than from the recording, and
    // said out loud: a run that is about to rewrite every picture of a
    // four-hour recording should say so before it starts.
    let video_share = match fit {
        Some(capacity) => {
            // How much of the recording is really being kept. A range asked
            // for past the end of it is planned as it was asked for -- the
            // copy simply stops when the packets do -- so the recording's own
            // length is what the arithmetic is done against.
            let kept: f64 = plans
                .iter()
                .map(|p| {
                    let (a, b) = (p.t_in.min(src.duration), p.t_out.min(src.duration));
                    (b - a).max(0.0)
                })
                .sum();
            let going = if bdav.is_some()
                || output.as_deref().is_some_and(|o| o.to_ascii_lowercase().ends_with(".m2ts"))
            {
                smartcut_core::fit::Going::Disc
            } else {
                smartcut_core::fit::Going::File
            };
            // The sound as it will be written: a track dropped or turned
            // into linear PCM changes what the disc holds as much as the
            // pictures do.
            let to_ts = going == smartcut_core::fit::Going::Disc
                || output.as_deref().is_some_and(|o| {
                    let o = o.to_ascii_lowercase();
                    [".ts", ".m2ts", ".mts", ".m2t"].iter().any(|e| o.ends_with(e))
                });
            let rates = smartcut_core::fit::rates_as_written(
                &src,
                &smartcut_core::fit::SoundAsked {
                    codec: audio_codec,
                    channels: audio_channels,
                    sample_rate: audio_sample_rate,
                    bits: audio_bits,
                    bit_rate: audio_bit_rate,
                    dropped: drop_streams.clone(),
                    to_ts,
                },
            );
            let mut estimate = smartcut_core::fit::estimate_at(rates, kept, going);
            // The file's own rate stands in for everything a cut into a file
            // carries that was never counted -- see `fit::estimate`.
            if going == smartcut_core::fit::Going::File {
                let whole = smartcut_core::fit::estimate(&src, &[], going);
                let share = if src.duration > 0.0 { kept / src.duration } else { 1.0 };
                estimate.bytes = estimate.bytes.max((whole.bytes as f64 * share) as u64);
            }
            let f = smartcut_core::fit::fit(&[estimate], capacity, smartcut_core::fit::MARGIN);
            println!(
                "fit   : {} MB onto {} MB usable of {} MB",
                f.bytes / 1_000_000,
                f.usable / 1_000_000,
                f.capacity / 1_000_000
            );
            if f.fits {
                println!("        it fits as it is");
                video_share
            } else if !f.reachable && f.video_bytes == 0 {
                bail!(
                    "this will not fit, and only MPEG-2 pictures can be written smaller: a \
                     larger disc, or less of the recording."
                );
            } else if !f.reachable {
                bail!(
                    "this will not fit: the pictures would have to be written at {:.1}% of their \
                     own size, and below {:.0}% they stop being the same pictures. A larger disc, \
                     or less of the recording.",
                    f.share * 100.0,
                    smartcut_core::fit::FLOOR * 100.0
                );
            } else {
                println!(
                    "        the pictures will be written at {:.1}% of their own size",
                    f.share * 100.0
                );
                Some(f.share)
            }
        }
        None => video_share,
    };

    // `--analyze` stops here, before the disc is touched. Reserving a slot on
    // one makes its directories and takes a number the next run counts past,
    // and asking what a cut would do is not asking for either.
    if analyze {
        return Ok(());
    }
    // A recording on a disc is `BDAV/STREAM/00001.m2ts`, and which number it
    // is depends on what is on the disc already. So the name is the disc's
    // to give, not `-o`'s.
    // The image goes beside the folder under the folder's name, and that can
    // be the very image the recording is being read out of. It would be
    // replaced by one of the new disc once the cut was done.
    if let (Some(at), Some(_)) = (&bdav, iso) {
        let image = format!("{at}.iso");
        let reads = std::iter::once(src.input.file.clone()).chain(
            joined
                .iter()
                .filter_map(|j| smartcut_core::input::Input::parse(j).ok().map(|i| i.file)),
        );
        for file in reads {
            if smartcut_core::input::same_file(&file, std::path::Path::new(&image)) {
                bail!(
                    "{image} is the image being read; making the new disc's image there would \
                     replace it. Give --bdav another folder"
                );
            }
        }
    }
    // A disc's slot is taken before the cut, as an empty stream (see
    // `bdav::prepare`). A run that stops before the cut is written leaves it,
    // or half a recording, on the disc under a number nothing names.
    let mut slot = Slot(None);
    let onto = match &bdav {
        Some(at) => {
            let at = std::path::PathBuf::from(at);
            let clip = smartcut_core::bdav::prepare(&at, 1)?.remove(0);
            let stream = smartcut_core::bdav::stream_of(&at, &clip);
            slot = Slot(Some(stream.clone()));
            println!("\ndisc  : {} -- recording {clip}", at.display());
            output = Some(stream.to_string_lossy().into_owned());
            Some((at, clip))
        }
        None => None,
    };
    if output.is_none() {
        eprintln!("\n(no -o given; nothing written)");
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
    // Whose encoder writes the partial GOPs. Worth saying for the one codec
    // libavcodec cannot encode, because the answer is "this program's own"
    // and because the step it writes at is the one setting that changes what
    // comes out of it.
    if matches!(src.video.codec.as_str(), "vc1" | "wmv3") {
        println!(
            "\nvideo :  partial GOPs written as VC-1 intra pictures at quantizer {} \
             (this program's own encoder; libavcodec has none)",
            vc1_quant.unwrap_or(smartcut_core::cut::VC1_DEFAULT_QUANT),
        );
    }
    println!(
        "\nrender:  audio {}{}{}{}{}{}",
        if asked.is_some() || resampled.is_some() || requantised.is_some() || recoded {
            "reencode"
        } else {
            audio_mode.as_str()
        },
        if recoded {
            format!(", as {}", audio_codec.as_str())
        } else {
            String::new()
        },
        asked.map_or(String::new(), |c| format!(", downmixed to {c}ch")),
        resampled.map_or(String::new(), |r| format!(", at {r} Hz")),
        requantised.map_or(String::new(), |b| format!(", {b} bit")),
        audio_bit_rate.map_or(String::new(), |b| format!(", {} kbit/s", b / 1000)),
    );
    // What is going out beside the pictures, and what is not.
    let to_ts = std::path::Path::new(&out)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ts" | "m2ts" | "mts" | "m2t"
            )
        });
    // A number that names no track that can be dropped would drop nothing,
    // and the run would go ahead as though it had been obeyed.
    let droppable = |n: usize| {
        src.audios.iter().any(|a| a.stream_index == n)
            || src.captions.iter().any(|c| c.stream_index == n)
            || src.graphics.iter().any(|g| g.stream_index == n)
    };
    if let Some(n) = drop_streams.iter().find(|&&n| !droppable(n)) {
        bail!(
            "--drop-stream {n}: {} has no sound, caption or subtitle stream {n} (the pictures cannot be dropped)",
            src.path
        );
    }
    let kept_audio = src
        .audios
        .iter()
        .filter(|a| !drop_streams.contains(&a.stream_index))
        .count();
    let kept_caps = if to_ts {
        src.captions
            .iter()
            .filter(|c| !drop_streams.contains(&c.stream_index))
            .count()
    } else {
        0
    };
    // A disc's own subtitles are kept where a transport stream is being
    // written, which can carry them, and wherever the pair beside the cut was
    // asked for, which anything can be written next to.
    let beside = subtitles != smartcut_core::cut::Subtitles::Pgs;
    let kept_graphics = if to_ts || beside {
        src.graphics
            .iter()
            .filter(|g| !drop_streams.contains(&g.stream_index))
            .count()
    } else {
        0
    };
    // Said only where there are any: a broadcast recording would otherwise
    // carry a line about a kind of subtitle no broadcast sends.
    let graphics = if src.graphics.is_empty() {
        String::new()
    } else {
        format!(
            ", {kept_graphics} of {} subtitle stream(s){}",
            src.graphics.len(),
            match subtitles {
                smartcut_core::cut::Subtitles::Beside => " beside the cut, as a pair",
                smartcut_core::cut::Subtitles::Sup => " beside the cut, as a .sup",
                smartcut_core::cut::Subtitles::Pgs => "",
            },
        )
    };
    // A DVD's subtitles are counted apart from the rest: they are kept, and
    // they are kept somewhere else. See `smartcut_core::vobsub`.
    let subpictures = if src.subpictures.is_empty() {
        String::new()
    } else {
        let kept = src
            .subpictures
            .iter()
            .filter(|s| !drop_subpictures.contains(&s.id))
            .count();
        let where_to = match (subtitles, to_ts) {
            (smartcut_core::cut::Subtitles::Pgs, true) => "converted into the cut",
            (smartcut_core::cut::Subtitles::Sup, _) => "converted, into a .sup beside the cut",
            _ => "beside the cut",
        };
        format!(
            ", {kept} of {} subtitle stream(s) {where_to}",
            src.subpictures.len()
        )
    };
    // Said out loud only where it is actually going in: asked for on a
    // recording that carries none, or in a shape that cannot hold one, the
    // engine says so itself rather than this promising it here.
    let data = if data_broadcast != Some(false)
        && smartcut_core::can_carry_data_broadcast(&out, tables)
        && src.dropped.iter().any(|d| d.what == "data")
    {
        ", the data broadcast"
    } else {
        ""
    };
    println!(
        "         {kept_audio} of {} sound track(s), {kept_caps} of {} caption \
         stream(s){graphics}{subpictures}{data}{}",
        src.audios.len(),
        src.captions.len(),
        match (to_ts, smartcut_core::tables_for(&out, tables)) {
            (true, smartcut_core::si::Tables::Partial) => ", written as a partial transport stream",
            (true, smartcut_core::si::Tables::Broadcast) => ", the broadcast's own tables",
            (true, smartcut_core::si::Tables::Muxer) => ", tables left to the muxer",
            (false, _) => "",
        },
    );
    // The recordings written after this one, each taken whole. What --keep
    // and --cut name is a range of the first; a command line that gave every
    // file its own ranges would be a project file with a worse syntax, and
    // the window is where a list with cuts in it belongs.
    let mut joined_src: Vec<smartcut_core::Source> = Vec::new();
    for path in &joined {
        let at = smartcut_core::netpath::resolve(path)?
            .to_string_lossy()
            .into_owned();
        // Chosen for this recording, as the first one's was: a disc map for
        // a clip on a disc, the container's table for anything else.
        let own: Box<dyn index::IndexSource> = match index_kind.as_str() {
            "auto" if smartcut_core::disc::clip_on_a_disc(&at).is_some() => {
                Box::new(index::DiscIndex)
            }
            "auto" | "container" => Box::new(index::ContainerIndex),
            "scan" => Box::new(index::PacketScan),
            _ => Box::new(index::DiscIndex),
        };
        let on_a_disc = smartcut_core::disc::clip_on_a_disc(&at).is_some();
        let mut also = match smartcut_core::scan_with(&at, own.as_ref()) {
            Ok(s) => s,
            // The container's table was the first choice and has answered
            // already; the walk is what is left.
            Err(_) if !on_a_disc && matches!(index_kind.as_str(), "auto" | "container") => {
                smartcut_core::scan_with(&at, &index::PacketScan)?
            }
            Err(_) => smartcut_core::scan_with(&at, &index::ContainerIndex)
                .or_else(|_| smartcut_core::scan_with(&at, &index::PacketScan))?,
        };
        let whole = vec![(0.0, also.duration)];
        if !also.leading_known {
            index::refine_leading(
                &also.input.url.clone(),
                &also.video.clone(),
                also.start_time,
                also.byte_seekable,
                &mut also.points,
                &whole,
            )?;
        }
        println!(
            "join  : {}  {} {}x{} {:.3}fps  dur={:.3}s",
            also.path,
            also.video.codec,
            also.video.width,
            also.video.height,
            also.video.frame_rate,
            also.duration,
        );
        joined_src.push(also);
    }
    // The sound and no pictures. Taken here, once every recording of the run
    // is open: what the sound writer wants is the ranges and the joins, both
    // of which are in hand, and nothing the planner works out.
    if sound_only {
        // A .wav is linear PCM, and the usage says so. Left to the recording,
        // a broadcast's AAC would be copied into a RIFF header few players
        // open.
        let audio_codec = if out.to_ascii_lowercase().ends_with(".wav")
            && audio_codec == smartcut_core::AudioCodec::Source
        {
            smartcut_core::AudioCodec::Lpcm
        } else {
            audio_codec
        };
        let between = smartcut_core::transition::Transition {
            kind: crossing,
            seconds: crossing_secs,
            easing,
            overlay: crossing_image.clone(),
            fade_out: join_fade_out,
            fade_in: join_fade_in,
        };
        let mut pieces = vec![smartcut_core::sound::Piece {
            src: &src,
            ranges: &ranges,
            after: between.clone(),
        }];
        let whole: Vec<Vec<(f64, f64)>> = joined_src
            .iter()
            .map(|s| vec![(0.0, s.duration)])
            .collect();
        for (s, r) in joined_src.iter().zip(&whole) {
            pieces.push(smartcut_core::sound::Piece {
                src: s,
                ranges: r,
                after: between.clone(),
            });
        }
        if let Some(last) = pieces.last_mut() {
            last.after = Default::default();
        }
        smartcut_core::sound::write(
            &pieces,
            master,
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
                audio_fade,
                ..Default::default()
            },
        )?;
        println!("wrote {out} (sound only)");
        return Ok(());
    }
    let joined_plans: Vec<Vec<smartcut_core::RangePlan>> = joined_src
        .iter()
        .map(|s| plan_on(s, &[(0.0, s.duration)], &opts))
        .collect();
    // On every reel but the last: a transition belongs to the clip that
    // gives way, and the last one gives way to nothing. The window is where
    // a fade at the end of the file is asked for.
    let between = smartcut_core::transition::Transition {
        kind: crossing,
        seconds: crossing_secs,
        easing,
        overlay: crossing_image,
        fade_out: join_fade_out,
        fade_in: join_fade_in,
    };
    let mut reels = vec![smartcut_core::cut::Reel {
        src: &src,
        plans: &plans,
        after: between.clone(),
    }];
    for (s, p) in joined_src.iter().zip(&joined_plans) {
        reels.push(smartcut_core::cut::Reel {
            src: s,
            plans: p,
            after: between.clone(),
        });
    }
    if let Some(last) = reels.last_mut() {
        last.after = Default::default();
    }
    if between.happens() && reels.len() > 1 {
        println!(
            "        {} between the clips, {:.2}s",
            between.kind.as_str(),
            between.seconds,
        );
    }
    if reels.len() > 1 {
        println!(
            "        {} recording(s) into one file, shaped like {}",
            reels.len(),
            reels[master.min(reels.len() - 1)].src.path,
        );
    }
    let written = smartcut_core::cut::join(
        &reels,
        master,
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
            drop_subpictures,
            subtitles,
            tables,
            data_broadcast,
            vc1_quant,
            video_share,
            audio_fade,
            plan: opts.clone(),
            ..Default::default()
        },
    );
    written?;
    slot.keep();
    // The sidecar exists for the ARIB workflow, where what is wanted beside
    // the video is an AAC elementary stream. A cut written in another codec
    // has no AAC in it to put there, and a `.aac` holding AC-3 would be worse
    // than no file at all.
    //
    // Asked for by name, the sound is AAC because that is what was asked for.
    // Left as the recording's own, it is AAC only if the recording's was: a
    // cut of a disc is AC-3 or LPCM, and `--audio-es` on one used to write a
    // nought-byte `.aac` and then stop with "Invalid argument", the muxer
    // having been handed a stream it has no header for.
    let es_is_aac = match audio_codec {
        smartcut_core::AudioCodec::Aac => true,
        smartcut_core::AudioCodec::Source => src.audio.as_ref().is_some_and(|a| a.codec == "aac"),
        _ => false,
    };
    if audio_es && es_is_aac {
        let beside = std::path::Path::new(&out).with_extension("aac");
        let n = smartcut_core::write_audio_es(&out, &beside.to_string_lossy(), aac)?;
        println!("wrote {} ({n} packets)", beside.display());
    } else if audio_es {
        // Named by what it actually is, not by the setting: "source" tells
        // nobody why the sidecar was declined.
        let is = match audio_codec {
            smartcut_core::AudioCodec::Source => src
                .audio
                .as_ref()
                .map(|a| a.codec.clone())
                .unwrap_or_else(|| "nothing".into()),
            other => other.as_str().to_string(),
        };
        eprintln!(
            "note: --audio-es writes the sound out as an AAC elementary stream, and this cut's \
             sound is {is}. No sidecar was written.",
        );
    }
    if let Some((at, clip)) = onto {
        // What the recording knew about itself. The disc it came off first,
        // where there was one -- a playlist's name is a name somebody has
        // already read and accepted -- and otherwise what the broadcast
        // itself carried. Each field on its own: a disc that named the
        // programme and not the channel should still take the channel from
        // the stream.
        let said = smartcut_core::si::programme(&src.input, 0).unwrap_or_default();
        let was = off_a_disc.as_ref();
        let name = programme
            .or_else(|| was.map(|e| e.label.clone()))
            .or_else(|| said.name.clone())
            .unwrap_or_else(|| {
                std::path::Path::new(&input)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| clip.clone())
            });
        let made = given_made
            .or_else(|| was.and_then(|e| e.made))
            .or(said.began);
        // How long it ran on air, which nothing on the command line offers:
        // it is the listing's own length and not a thing anybody types.
        let ran = was.and_then(|e| e.ran).or(said.ran);
        let description = about
            .or_else(|| was.and_then(|e| e.description.clone()))
            .or(said.description);
        let channel = given_channel
            .or_else(|| was.and_then(|e| e.channel.clone()))
            .or(said.channel);
        let channel_number = given_number
            .or_else(|| was.map(|e| e.channel_number).filter(|n| *n > 0))
            .unwrap_or(said.channel_number);
        // A chapter point where each kept range begins, which is where the
        // cuts are: the one place a viewer would want to skip to.
        let mut at_out = 0.0;
        let mut marks = Vec::new();
        for plan in &plans {
            marks.push(at_out);
            at_out += plan.t_out - plan.t_in;
        }
        // What to call the disc, when nobody said: the series this
        // recording is an episode of, which is what a run of them written
        // one after another onto the same disc has in common. The channel
        // only where the name says nothing at all -- see
        // [`smartcut_core::series`].
        let title = disc_title
            .or_else(|| smartcut_core::series::shared(std::slice::from_ref(&name)))
            .or_else(|| channel.clone())
            .unwrap_or_else(|| name.clone());
        smartcut_core::bdav::write(
            &at,
            &title,
            &[smartcut_core::bdav::Recording {
                clip: clip.clone(),
                name: name.clone(),
                made,
                ran,
                description,
                channel,
                channel_number,
                marks,
            }],
            None,
        )?;
        println!("wrote {} -- {name}", at.join("BDAV").display());
        if let Some(revision) = iso {
            // Beside the folder and named after it. The folder stays unless
            // `--iso-only` says otherwise: it is what the image was made of,
            // and deleting somebody's disc because they asked for an image of
            // it is not this program's decision to make on its own.
            // Appended rather than `with_extension`, which would take a
            // folder called `2026.09` and write `2026.iso`.
            let image = std::path::PathBuf::from(format!("{}.iso", at.display()));
            let bytes =
                smartcut_core::udfw::write(&at, &image, revision, iso_access, &title, None)?;
            println!(
                "wrote {} ({:.1} MB, UDF {}, {})",
                image.display(),
                bytes as f64 / 1e6,
                revision.as_str(),
                iso_access.as_str()
            );
            // And the folder, now that the image holds all of it. After the
            // image and never instead of it: a folder taken away before
            // there is an image is the disc lost.
            if iso_only {
                smartcut_core::bdav::remove_disc(&at)?;
                println!("removed {}", at.display());
            }
        }
        return Ok(());
    }
    println!("wrote {out}");
    Ok(())
}

/// A disc's stream taken for this run, given back unless the run keeps it.
struct Slot(Option<std::path::PathBuf>);

impl Slot {
    fn keep(&mut self) {
        self.0 = None;
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_never_reads_sixty() {
        assert_eq!(fmt_hms(0.0), "00:00:00.000");
        assert_eq!(fmt_hms(3661.5), "01:01:01.500");
        assert_eq!(fmt_hms(90.09), "00:01:30.090");
        // A hair under a minute is that minute, not its sixtieth second.
        assert_eq!(fmt_hms(59.9999), "00:01:00.000");
        assert_eq!(fmt_hms(3599.9999), "01:00:00.000");
        // And the sign goes in front of the whole reading, not into each
        // field of it.
        assert_eq!(fmt_hms(-12.34), "-00:00:12.340");
    }

    #[test]
    fn a_range_has_to_be_one() {
        assert_eq!(parse_range("3-9").unwrap(), (3.0, 9.0));
        assert_eq!(parse_range("1:30-2:00").unwrap(), (90.0, 120.0));
        // A start at or after the end selects nothing, which is not a cut.
        assert!(parse_range("9-3").is_err());
        assert!(parse_range("2-2").is_err());
        assert!(parse_range("3").is_err());
    }

    #[test]
    fn overlapping_ranges_become_one() {
        assert_eq!(merge_ranges(vec![(1.0, 3.0), (2.0, 4.0)]), [(1.0, 4.0)]);
        // Touching counts as overlapping: two ranges that meet describe one
        // stretch, and splicing it to itself is not what was asked for.
        assert_eq!(merge_ranges(vec![(0.0, 5.0), (5.0, 9.0)]), [(0.0, 9.0)]);
        // One inside another leaves the outer one.
        assert_eq!(merge_ranges(vec![(0.0, 9.0), (3.0, 4.0)]), [(0.0, 9.0)]);
        // And ranges that do not meet are left alone, in order.
        assert_eq!(
            merge_ranges(vec![(8.0, 9.0), (1.0, 2.0)]),
            [(1.0, 2.0), (8.0, 9.0)]
        );
    }

    #[test]
    fn cut_ranges_are_the_complement_of_the_kept_ones() {
        assert_eq!(
            complement(&mut [(3.0, 5.0)], 10.0),
            [(0.0, 3.0), (5.0, 10.0)]
        );
        // A cut running to the end leaves only what is in front of it.
        assert_eq!(complement(&mut [(8.0, 20.0)], 10.0), [(0.0, 8.0)]);
        // Overlapping cuts are one cut.
        assert_eq!(
            complement(&mut [(3.0, 6.0), (5.0, 8.0)], 10.0),
            [(0.0, 3.0), (8.0, 10.0)]
        );
    }
}

