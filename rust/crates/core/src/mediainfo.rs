//! What a recording is, as libavformat reads it: the container and then each
//! stream, a line per fact.
//!
//! The clip list's メディア情報. Not MediaInfoLib, which would be a second
//! reader of every format beside the one that is actually used -- and where
//! the two disagree, it is libav's reading that the cut is made from. So the
//! answer here is that one, laid out for a person rather than for the planner:
//! names rather than numbers, and nothing the demuxer did not say.
//!
//! Each fact is a key and a value. The key is what the window translates; the
//! value is a codec's or a format's own name, which is the same in every
//! language.

use crate::input;
use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

/// One line: which fact, and what it is. For `scan`, `range` and `flags` the value is
/// a word (or words joined by commas) for the window to translate too.
pub struct Fact {
    pub key: &'static str,
    pub value: String,
}

/// One stream's lines.
pub struct StreamFacts {
    /// `video`, `audio`, `subtitle`, `data` or `attachment`.
    pub kind: &'static str,
    /// Its place in the container, which is what ffprobe and every other
    /// tool number it by.
    pub index: usize,
    pub facts: Vec<Fact>,
}

pub struct MediaInfo {
    pub general: Vec<Fact>,
    pub streams: Vec<StreamFacts>,
}

/// Read what the container states about `path`, and nothing further: no
/// packet is read past the probe, so a recording on a share costs one open --
/// and, for a transport stream, a read of its head for the service's name.
pub fn media_info(path: &str) -> Result<MediaInfo> {
    crate::init()?;
    let input = input::Input::parse(path)?;
    let ictx = input::demux(&input.url).map_err(|e| anyhow!("cannot open {path}: {e}"))?;
    let mut general = Vec::new();

    let format = ictx.format();
    let long = format.description();
    push(
        &mut general,
        "format",
        if long.is_empty() || long == format.name() {
            format.name().to_string()
        } else {
            format!("{long} ({})", format.name())
        },
    );
    let (size, duration, bit_rate, programs, chapters) = unsafe {
        let p = ictx.as_ptr();
        let size = if (*p).pb.is_null() { -1 } else { ff::ffi::avio_size((*p).pb) };
        (size, (*p).duration, (*p).bit_rate, (*p).nb_programs, (*p).nb_chapters)
    };
    if size > 0 {
        push(&mut general, "size", bytes(size as u64));
    }
    if duration != ff::ffi::AV_NOPTS_VALUE && duration > 0 {
        push(&mut general, "duration", clock(duration as f64 / ff::ffi::AV_TIME_BASE as f64));
    }
    // The container's figure where it has one; worked out from the size and
    // the length where it has not, which is what an MPEG stream leaves it at.
    let rate = if bit_rate > 0 {
        bit_rate as f64
    } else if size > 0 && duration > 0 {
        size as f64 * 8.0 / (duration as f64 / ff::ffi::AV_TIME_BASE as f64)
    } else {
        0.0
    };
    if rate > 0.0 {
        push(&mut general, "bitrate", bits(rate));
    }
    let start = crate::container_start(&ictx);
    if start != 0.0 {
        push(&mut general, "start", format!("{start:.6} s"));
    }
    if programs > 1 {
        push(&mut general, "programs", programs.to_string());
    }
    if chapters > 0 {
        push(&mut general, "chapters", chapters.to_string());
    }
    let tags = ictx.metadata();
    for (key, tag) in [("title", "title"), ("encoder", "encoder"), ("created", "creation_time")] {
        if let Some(v) = text(&tags, tag) {
            push(&mut general, key, v);
        }
    }
    // A broadcast's own name for itself, read as SmartCut reads it everywhere
    // else (see [`crate::si::programme`]) rather than as libavformat's
    // `service_name`: that is the descriptor's bytes as they came, which off a
    // Japanese network are ARIB's eight-unit code -- mojibake with escape
    // codes in it -- and it is the first programme's, which on a recording
    // of the whole multiplex is as likely to be a neighbour's as its own.
    if format.name().split(',').any(|n| n.trim() == "mpegts") {
        // Through the same hands as a tag: the name is decoded out of the
        // broadcast's own bytes, and a control code in them is a line break
        // in the table and in the text copied out of it.
        if let Some(v) = crate::si::programme(&input, 0).ok().and_then(|p| p.channel) {
            push(&mut general, "service", one_line(&v));
        }
    }

    let mut streams = Vec::new();
    for s in ictx.streams() {
        let par = s.parameters();
        let raw = unsafe { &*par.as_ptr() };
        let kind = match unsafe { crate::raw_enum(std::ptr::addr_of!(raw.codec_type)) } {
            0 => "video",
            1 => "audio",
            3 => "subtitle",
            4 => "attachment",
            _ => "data",
        };
        let mut facts = Vec::new();
        let id = par.id();
        push(&mut facts, "codec", codec_name(id));
        let codec_id: ff::ffi::AVCodecID = id.into();
        if raw.profile >= 0 {
            let name = unsafe { name_of(ff::ffi::avcodec_profile_name(codec_id, raw.profile)) };
            push(&mut facts, "profile", name.unwrap_or_else(|| raw.profile.to_string()));
        }
        if raw.level > 0 {
            push(&mut facts, "level", level(id, raw.level));
        }
        match kind {
            "video" => {
                if raw.width > 0 && raw.height > 0 {
                    push(&mut facts, "resolution", format!("{} × {}", raw.width, raw.height));
                }
                let sar = raw.sample_aspect_ratio;
                if sar.num > 0 && sar.den > 0 && raw.width > 0 && raw.height > 0 {
                    push(&mut facts, "sar", format!("{}:{}", sar.num, sar.den));
                    // In 64 bits: a SAR is whatever the container states, and
                    // 4K times a large one leaves an `int`.
                    let (w, h) = (
                        i64::from(raw.width) * i64::from(sar.num),
                        i64::from(raw.height) * i64::from(sar.den),
                    );
                    let g = gcd(w, h);
                    push(&mut facts, "dar", format!("{}:{}", w / g, h / g));
                }
                let pix = unsafe { crate::raw_enum(std::ptr::addr_of!(raw.format)) };
                if pix >= 0 {
                    push(
                        &mut facts,
                        "pixels",
                        unsafe { name_of(named::av_get_pix_fmt_name(pix)) }
                            .unwrap_or_else(|| pix.to_string()),
                    );
                }
                if raw.bits_per_raw_sample > 0 {
                    push(&mut facts, "depth", format!("{} bit", raw.bits_per_raw_sample));
                }
                push(
                    &mut facts,
                    "scan",
                    match unsafe { crate::raw_enum(std::ptr::addr_of!(raw.field_order)) } {
                        // Words for the window to translate, as `flags` is.
                        1 => "progressive",
                        2 => "tt",
                        3 => "bb",
                        4 => "tb",
                        5 => "bt",
                        _ => "",
                    }
                    .to_string(),
                );
                // MPEG-1 and MPEG-2 state their rate in the sequence header,
                // and that is the rate the rest of SmartCut plans on: the
                // average is the container's arithmetic, which on soft
                // telecine remuxed into an MP4 comes to 25.46 for a stream at
                // 29.97. See `outline_of`.
                let stated = ff::Rational::from(raw.framerate);
                let avg = if matches!(id, ff::codec::Id::MPEG1VIDEO | ff::codec::Id::MPEG2VIDEO)
                    && fps(stated).is_some()
                {
                    stated
                } else {
                    s.avg_frame_rate()
                };
                let base = s.rate();
                if let Some(r) = fps(avg).or_else(|| fps(base)) {
                    push(&mut facts, "fps", r);
                }
                if avg != base {
                    if let (Some(_), Some(b)) = (fps(avg), fps(base)) {
                        push(&mut facts, "fpsBase", b);
                    }
                }
                push(&mut facts, "colour", colour(raw));
                push(
                    &mut facts,
                    "range",
                    match unsafe { crate::raw_enum(std::ptr::addr_of!(raw.color_range)) } {
                        // libav's own words, `tv` and `pc`, for the window to
                        // translate.
                        1 => "tv",
                        2 => "pc",
                        _ => "",
                    }
                    .to_string(),
                );
                if s.frames() > 0 {
                    push(&mut facts, "frames", s.frames().to_string());
                }
            }
            "audio" => {
                if raw.sample_rate > 0 {
                    push(&mut facts, "sampleRate", format!("{} Hz", raw.sample_rate));
                }
                if raw.ch_layout.nb_channels > 0 {
                    let mut buf = [0 as c_char; 64];
                    let n = unsafe {
                        ff::ffi::av_channel_layout_describe(&raw.ch_layout, buf.as_mut_ptr(), buf.len())
                    };
                    let layout = if n > 0 { unsafe { name_of(buf.as_ptr()) } } else { None };
                    push(
                        &mut facts,
                        "channels",
                        match layout {
                            Some(l) => format!("{} ({l})", raw.ch_layout.nb_channels),
                            None => raw.ch_layout.nb_channels.to_string(),
                        },
                    );
                }
                let fmt = unsafe { crate::raw_enum(std::ptr::addr_of!(raw.format)) };
                if fmt >= 0 {
                    if let Some(n) = unsafe { name_of(named::av_get_sample_fmt_name(fmt)) } {
                        push(&mut facts, "sampleFormat", n);
                    }
                }
                if raw.bits_per_raw_sample > 0 {
                    push(&mut facts, "depth", format!("{} bit", raw.bits_per_raw_sample));
                }
            }
            _ => {}
        }
        if raw.bit_rate > 0 {
            push(&mut facts, "bitrate", bits(raw.bit_rate as f64));
        }
        let length = s.duration();
        if length != ff::ffi::AV_NOPTS_VALUE && length > 0 {
            push(&mut facts, "duration", clock(length as f64 * f64::from(s.time_base())));
        }
        let meta = s.metadata();
        if let Some(v) = text(&meta, "language").filter(|v| v != "und") {
            push(&mut facts, "language", v);
        }
        // MP4 names its tracks by handler, and a muxer that was told nothing
        // writes its own placeholder there -- libavformat's `VideoHandler`,
        // Apple's `Core Media Video` -- which is not a name anybody gave it.
        let named = text(&meta, "title").or_else(|| {
            text(&meta, "handler_name")
                .filter(|h| !h.ends_with("Handler") && !h.starts_with("Core Media"))
        });
        if let Some(v) = named {
            push(&mut facts, "name", v);
        }
        // The stream's id is a PID on a transport stream and a stream id on a
        // program stream, which is how a recorder's own tools name it.
        if s.id() != 0 {
            push(&mut facts, "id", format!("{0} (0x{0:X})", s.id()));
        }
        let flags = disposition(s.disposition());
        push(&mut facts, "flags", flags);
        streams.push(StreamFacts { kind, index: s.index(), facts });
    }
    Ok(MediaInfo { general, streams })
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// A tag as a line can show it: checked as UTF-8 (see [`crate::tag`]), on
/// one line, and of a length a table cell can hold -- a tag is as long as
/// the file says, and a crafted title of megabytes would go whole to the
/// window and into the copied text.
fn text(dict: &ff::DictionaryRef, key: &str) -> Option<String> {
    Some(one_line(&crate::tag(dict, key)?))
}

/// What [`text`] makes of a tag, for any text the file wrote.
fn one_line(v: &str) -> String {
    const MOST: usize = 1000;
    let mut line: String = v
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MOST)
        .collect();
    if v.trim().chars().nth(MOST).is_some() {
        line.push('…');
    }
    line
}

/// A line, where there is something to put on it.
fn push(facts: &mut Vec<Fact>, key: &'static str, value: String) {
    if !value.is_empty() {
        facts.push(Fact { key, value });
    }
}

fn codec_name(id: ff::codec::Id) -> String {
    let short = id.name();
    let desc = unsafe { ff::ffi::avcodec_descriptor_get(id.into()) };
    let long = if desc.is_null() { None } else { unsafe { name_of((*desc).long_name) } };
    match long {
        Some(l) if !l.is_empty() && l != short => format!("{short} ({l})"),
        _ => short.to_string(),
    }
}

/// The level as the codec's own documents write it.
fn level(id: ff::codec::Id, level: c_int) -> String {
    use ff::codec::Id;
    match id {
        Id::H264 if level == 9 => "1b".into(),
        Id::H264 => format!("{}.{}", level / 10, level % 10),
        // HEVC and VVC number their levels thirty to the step.
        Id::HEVC | Id::VVC => {
            // In 64 bits: the level is whatever the container states.
            let tenths = i64::from(level) * 10 / 30;
            format!("{}.{}", tenths / 10, tenths % 10)
        }
        Id::MPEG2VIDEO => match level {
            4 => "High".into(),
            6 => "High 1440".into(),
            8 => "Main".into(),
            10 => "Low".into(),
            n => n.to_string(),
        },
        _ => level.to_string(),
    }
}

fn fps(r: ff::Rational) -> Option<String> {
    if r.numerator() <= 0 || r.denominator() <= 0 {
        return None;
    }
    let v = f64::from(r);
    if !v.is_finite() || v > 1000.0 {
        return None;
    }
    Some(if r.denominator() == 1 {
        format!("{} fps", r.numerator())
    } else {
        format!("{v:.3} fps ({}/{})", r.numerator(), r.denominator())
    })
}

/// Primaries, transfer and matrix as one phrase. Empty where the stream
/// states none of them -- 2, "unspecified" in all three tables -- which is
/// what an MPEG-2 broadcast without a sequence display extension leaves them
/// at: three "unknown"s say less than no line.
fn colour(raw: &ff::ffi::AVCodecParameters) -> String {
    let [p, t, m] = unsafe {
        [
            crate::raw_enum(std::ptr::addr_of!(raw.color_primaries)),
            crate::raw_enum(std::ptr::addr_of!(raw.color_trc)),
            crate::raw_enum(std::ptr::addr_of!(raw.color_space)),
        ]
    };
    if [p, t, m].iter().all(|v| *v == 2) {
        return String::new();
    }
    let say = |f: unsafe extern "C" fn(c_int) -> *const c_char, v: i32| {
        unsafe { name_of(f(v)) }.unwrap_or_else(|| v.to_string())
    };
    format!(
        "{} / {} / {}",
        say(named::av_color_primaries_name, p),
        say(named::av_color_transfer_name, t),
        say(named::av_color_space_name, m),
    )
}

fn disposition(d: ff::format::stream::Disposition) -> String {
    use ff::format::stream::Disposition as D;
    [
        (D::DEFAULT, "default"),
        (D::FORCED, "forced"),
        (D::HEARING_IMPAIRED, "hearing"),
        (D::VISUAL_IMPAIRED, "visual"),
        (D::COMMENT, "comment"),
        (D::ATTACHED_PIC, "cover"),
    ]
    .iter()
    .filter(|(flag, _)| d.contains(*flag))
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join(",")
}

fn bytes(n: u64) -> String {
    let with_commas = n
        .to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(",");
    let gib = n as f64 / (1u64 << 30) as f64;
    if gib >= 1.0 {
        format!("{gib:.2} GiB ({with_commas} bytes)")
    } else {
        format!("{:.1} MiB ({with_commas} bytes)", n as f64 / (1u64 << 20) as f64)
    }
}

fn bits(per_second: f64) -> String {
    if per_second >= 1e6 {
        format!("{:.2} Mbps", per_second / 1e6)
    } else {
        format!("{:.0} kbps", per_second / 1e3)
    }
}

fn clock(seconds: f64) -> String {
    let ms = (seconds * 1000.0).round().max(0.0) as u64;
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

unsafe fn name_of(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok().map(str::to_owned)
}

// Taking the plain int, for the reason given in `conform::named`: a container
// may state a value no binding enum has a variant for.
mod named {
    use std::os::raw::{c_char, c_int};

    extern "C" {
        pub fn av_get_pix_fmt_name(v: c_int) -> *const c_char;
        pub fn av_get_sample_fmt_name(v: c_int) -> *const c_char;
        pub fn av_color_primaries_name(v: c_int) -> *const c_char;
        pub fn av_color_transfer_name(v: c_int) -> *const c_char;
        pub fn av_color_space_name(v: c_int) -> *const c_char;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_read_as_written() {
        use ff::codec::Id;
        assert_eq!(level(Id::H264, 41), "4.1");
        assert_eq!(level(Id::H264, 9), "1b");
        assert_eq!(level(Id::HEVC, 153), "5.1");
        assert_eq!(level(Id::HEVC, 120), "4.0");
        assert_eq!(level(Id::MPEG2VIDEO, 4), "High");
    }

    #[test]
    fn sizes_and_clocks() {
        assert_eq!(bytes(1_234_567), "1.2 MiB (1,234,567 bytes)");
        assert_eq!(clock(3723.5), "1:02:03.500");
        assert_eq!(bits(15_300_000.0), "15.30 Mbps");
        assert_eq!(bits(192_000.0), "192 kbps");
    }
}
