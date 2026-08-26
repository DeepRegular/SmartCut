//! Finding the station logo, and the stretches where it is missing.
//!
//! Japanese broadcasters keep a small translucent logo in one corner during
//! the programme and take it down for the commercials. That makes it a
//! second, independent read on where a break is -- one that does not care
//! whether the junctions happened to be silent.
//!
//! No logo library is needed. The logo is the only thing in its corner that
//! never moves, so averaging a few thousand frames leaves it standing while
//! the pictures behind it blur away; high-pass filtering that average is the
//! template. Scoring a moment is then a correlation against it.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::Source;

pub struct LogoOptions {
    /// Correlation above which the logo counts as present.
    pub present: f64,
    /// ...and below which it counts as gone. The gap is hysteresis.
    pub absent: f64,
    /// Ignore absences shorter than this; a commercial break never is.
    pub min_absent: f64,
    /// ...except at the two ends. An absence that runs off the start or the
    /// end of the recording is not a break at all -- it is the recorder
    /// having begun before the programme did, or stopped after it ended --
    /// and those are routinely only a few seconds long.
    pub min_edge_absent: f64,
    /// A run of commercials is long. If the absences found are mostly short,
    /// what is being tracked is not a logo.
    pub typical_break: f64,
    /// More absences than a broadcast could plausibly contain means the
    /// signal is noise.
    pub max_breaks: usize,
    /// Seconds of samples averaged before scoring, to wash out the picture
    /// behind the logo. Too short and the content still shows through.
    pub window_seconds: f64,
    /// How many pixels the template keeps. A station logo is small, so a
    /// fixed count focuses on it however large the search region is -- a
    /// percentage of a big region is mostly the noise around it.
    pub mask_pixels: usize,
}

impl Default for LogoOptions {
    fn default() -> Self {
        Self {
            present: 0.05,
            absent: 0.02,
            min_absent: 20.0,
            min_edge_absent: 1.0,
            typical_break: 30.0,
            max_breaks: 12,
            window_seconds: 5.0,
            mask_pixels: 500,
        }
    }
}

/// One corner of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    const ALL: [Corner; 4] =
        [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight];

    fn origin(self, w: usize, h: usize, cw: usize, ch: usize) -> (usize, usize) {
        match self {
            Corner::TopLeft => (0, 0),
            Corner::TopRight => (w - cw, 0),
            Corner::BottomLeft => (0, h - ch),
            Corner::BottomRight => (w - cw, h - ch),
        }
    }
}

pub struct Logo {
    pub corner: Corner,
    /// How strongly the template stands out.
    pub strength: f64,
    /// Stretches, in seconds, where the logo is not on screen.
    pub absent: Vec<(f64, f64)>,
}

/// Nothing in any corner behaved like a station logo.
///
/// Not every broadcaster shows one -- several subscription channels do not --
/// and a detector that returns its best guess regardless would hand back
/// noise. Saying so lets the caller fall back to the silences alone.
#[derive(Debug)]
pub struct NoLogo;

impl std::fmt::Display for NoLogo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no station logo found")
    }
}

impl std::error::Error for NoLogo {}

struct Region {
    w: usize,
    h: usize,
}

impl Region {
    fn highpass(&self, v: &[f64]) -> Vec<f64> {
        let (w, h) = (self.w, self.h);
        let mut out = vec![0.0; v.len()];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                out[i] = v[i] - (v[i - 1] + v[i + 1] + v[i - w] + v[i + w]) / 4.0;
            }
        }
        out
    }
}

/// Pull one corner's luma out of a decoded frame.
fn crop(frame: &ff::frame::Video, corner: Corner, cw: usize, ch: usize, out: &mut [u8]) {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let (ox, oy) = corner.origin(w, h, cw, ch);
    let stride = frame.stride(0);
    let data = frame.data(0);
    for y in 0..ch {
        let src = (oy + y) * stride + ox;
        out[y * cw..(y + 1) * cw].copy_from_slice(&data[src..src + cw]);
    }
}

/// Decode the key frames and hand each one's four corners to `visit`.
fn walk_keyframes(
    src: &Source,
    cw: usize,
    ch: usize,
    mut progress: Option<&mut dyn FnMut(f64)>,
    mut visit: impl FnMut(f64, &[Vec<u8>]),
) -> Result<()> {
    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?.parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().video()?;
    // Only intra pictures are decoded: eight times faster, and a couple of
    // samples a second is far more resolution than a commercial break needs.
    unsafe {
        (*decoder.as_mut_ptr()).skip_frame = ff::ffi::AVDiscard::AVDISCARD_NONKEY;
    }

    let mut buffers: Vec<Vec<u8>> = (0..4).map(|_| vec![0u8; cw * ch]).collect();
    let mut frame = ff::frame::Video::empty();
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            if (frame.width() as usize) < cw || (frame.height() as usize) < ch {
                continue;
            }
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * src.video.time_base - src.start_time;
            for (k, corner) in Corner::ALL.iter().enumerate() {
                crop(&frame, *corner, cw, ch, &mut buffers[k]);
            }
            visit(t, &buffers);
            if let Some(f) = progress.as_mut() {
                f((t / src.duration.max(1e-9)).clamp(0.0, 1.0));
            }
        }
    }
    Ok(())
}

pub fn detect(src: &Source, opts: &LogoOptions) -> Result<Logo> {
    detect_with(src, opts, None)
}

/// As [`detect`], reporting progress across both passes over the key
/// pictures. The first builds the template, the second scores against it, so
/// each is half the work.
pub fn detect_with(
    src: &Source,
    opts: &LogoOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Logo> {
    crate::init()?;
    let (w, h) = (src.video.width as usize, src.video.height as usize);
    let (cw, ch) = ((w / 4) & !1, (h / 5) & !1);
    if cw < 16 || ch < 16 {
        return Err(anyhow!("frame too small to look for a logo"));
    }
    let region = Region { w: cw, h: ch };
    let n = cw * ch;

    // First pass: what is always there, in each corner?
    let mut sums: Vec<Vec<f64>> = (0..4).map(|_| vec![0.0; n]).collect();
    let mut count = 0usize;
    // Two passes over the key pictures, so each is half of the reported
    // progress. The adaptor is scoped to its pass: it borrows `progress`,
    // and the second pass needs it back.
    {
        let mut told = -1.0;
        let mut on = |done: f64| {
            if let Some(f) = progress.as_mut() {
                if done - told >= 0.02 {
                    told = done;
                    f(done * 0.5);
                }
            }
        };
        walk_keyframes(src, cw, ch, Some(&mut on), |_t, corners| {
        for (k, buf) in corners.iter().enumerate() {
            for (i, &v) in buf.iter().enumerate() {
                sums[k][i] += v as f64;
            }
        }
        count += 1;
        })?;
    }
    if count < 20 {
        return Err(anyhow!("only {count} key frames; not enough to find a logo"));
    }

    // Build a template for each corner. The strongest is not necessarily the
    // logo: programme branding is often bolder, but it comes and goes.
    struct Cand {
        corner: Corner,
        tmpl: Vec<f64>,
        mask: Vec<usize>,
        norm: f64,
        strength: f64,
    }
    let mut cands = Vec::new();
    for (k, corner) in Corner::ALL.iter().enumerate() {
        let avg: Vec<f64> = sums[k].iter().map(|s| s / count as f64).collect();
        let tmpl = region.highpass(&avg);
        let mut mag: Vec<f64> = tmpl.iter().map(|x| x.abs()).collect();
        mag.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let cutoff = mag[opts.mask_pixels.min(n - 1)];
        let picked: Vec<usize> = (0..n).filter(|&i| tmpl[i].abs() >= cutoff).collect();
        // A logo is one contiguous mark. Scattered survivors are whatever
        // else happened to sit still -- a caption box edge, a letterbox line
        // -- and they only add noise to the correlation.
        let mask = largest_cluster(&picked, cw, ch);
        let strength =
            mask.iter().map(|&i| tmpl[i] * tmpl[i]).sum::<f64>() / mask.len().max(1) as f64;
        let norm = mask.iter().map(|&i| tmpl[i] * tmpl[i]).sum::<f64>().sqrt();
        cands.push(Cand { corner: *corner, tmpl, mask, norm, strength });
    }

    // Second pass: score every key frame against all four templates.
    let ring_len = {
        // key frames are not evenly spaced, so size the window from the
        // recording's actual rate
        let rate = count as f64 / src.duration.max(1.0);
        ((opts.window_seconds * rate).round() as usize).clamp(2, 64)
    };
    let mut rings: Vec<std::collections::VecDeque<Vec<u8>>> =
        (0..4).map(|_| Default::default()).collect();
    let mut times: Vec<f64> = Vec::new();
    let mut scores: Vec<Vec<f64>> = vec![Vec::new(); 4];
    {
        let mut told = -1.0;
        let mut on = |done: f64| {
            if let Some(f) = progress.as_mut() {
                if done - told >= 0.02 {
                    told = done;
                    f(0.5 + done * 0.5);
                }
            }
        };
        walk_keyframes(src, cw, ch, Some(&mut on), |t, corners| {
        times.push(t);
        for k in 0..4 {
            rings[k].push_back(corners[k].clone());
            if rings[k].len() > ring_len {
                rings[k].pop_front();
            }
            let mut avg = vec![0.0; n];
            for buf in &rings[k] {
                for i in 0..n {
                    avg[i] += buf[i] as f64;
                }
            }
            for v in avg.iter_mut() {
                *v /= rings[k].len() as f64;
            }
            let hp = region.highpass(&avg);
            let c = &cands[k];
            let dot: f64 = c.mask.iter().map(|&i| hp[i] * c.tmpl[i]).sum();
            let en: f64 = c.mask.iter().map(|&i| hp[i] * hp[i]).sum::<f64>().sqrt();
            scores[k].push(if en > 0.0 && c.norm > 0.0 { dot / (en * c.norm) } else { 0.0 });
        }
        })?;
    }

    // Thresholds from the recording itself: how boldly a logo reads varies
    // by channel, but the programme always dominates the runtime, so the
    // median score is a reliable stand-in for "logo present".
    let thresholds: Vec<(f64, f64)> = scores
        .iter()
        .map(|col| {
            let mut v = col.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = v.get(v.len() / 2).copied().unwrap_or(0.0);
            ((median * 0.40).max(0.01), (median * 0.15).max(0.004))
        })
        .collect();

    // Pick the corner that behaves like a station logo: on for most of the
    // recording, and switching state only a handful of times. Programme
    // branding scores higher but flickers; the logo is steady.
    /// The corner that won, how unsettled its signal was, and what it found.
    struct Pick {
        corner_index: usize,
        transitions: usize,
        absent: Vec<(f64, f64)>,
    }
    let mut chosen: Option<Pick> = None;
    for k in 0..4 {
        let (present_t, absent_t) = thresholds[k];
        let (intervals, transitions) = intervals_from(&times, &scores[k], present_t, absent_t, opts);
        let present: usize = scores[k].iter().filter(|&&s| s >= present_t).count();
        let frac = present as f64 / scores[k].len().max(1) as f64;
        if std::env::var("SMARTCUT_DEBUG").is_ok() {
            eprintln!(
                "  corner {:?}: strength {:8.1}  present {:.3}  transitions {}  intervals {}",
                cands[k].corner,
                cands[k].strength,
                frac,
                transitions,
                intervals.len()
            );
        }
        // A logo is on screen for most of a broadcast, and the gaps are
        // commercial breaks: few of them, and long. Short flickering gaps
        // mean the template latched onto moving picture instead.
        let mut lengths: Vec<f64> = intervals.iter().map(|(a, b)| b - a).collect();
        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_len = lengths.get(lengths.len() / 2).copied().unwrap_or(f64::INFINITY);
        let plausible = frac >= 0.5
            && intervals.len() <= opts.max_breaks
            && (intervals.is_empty() || median_len >= opts.typical_break);
        if !plausible {
            continue;
        }
        if chosen.as_ref().is_none_or(|p| transitions < p.transitions) {
            chosen = Some(Pick { corner_index: k, transitions, absent: intervals });
        }
    }
    let pick = chosen.ok_or(NoLogo)?;
    let (absent, k) = (pick.absent, pick.corner_index);
    let corner = cands[k].corner;
    let strength = cands[k].strength;

    Ok(Logo { corner, strength, absent })
}


/// Turn a score timeline into "logo missing" stretches, with hysteresis so a
/// single dark frame does not end the programme. Also reports how often the
/// state flipped, which is what tells a steady logo from a flickering caption.
fn intervals_from(
    times: &[f64],
    scores: &[f64],
    present_t: f64,
    absent_t: f64,
    opts: &LogoOptions,
) -> (Vec<(f64, f64)>, usize) {
    let mut absent = Vec::new();
    let mut transitions = 0usize;
    let mut present = scores.first().copied().unwrap_or(0.0) >= present_t;
    // An absence still running from before the first sample is an edge, and
    // edges are held to a much shorter minimum than breaks are.
    let mut at_head = !present;
    let mut from = times.first().copied().unwrap_or(0.0);
    for (i, &s) in scores.iter().enumerate() {
        if present && s < absent_t {
            present = false;
            transitions += 1;
            from = times[i];
        } else if !present && s >= present_t {
            present = true;
            transitions += 1;
            let least = if at_head { opts.min_edge_absent } else { opts.min_absent };
            at_head = false;
            if times[i] - from >= least {
                absent.push((from, times[i]));
            }
        }
    }
    if !present {
        if let Some(&last) = times.last() {
            if last - from >= opts.min_edge_absent {
                absent.push((from, last));
            }
        }
    }
    (absent, transitions)
}


/// Keep only the biggest connected run of mask pixels.
fn largest_cluster(picked: &[usize], w: usize, h: usize) -> Vec<usize> {
    use std::collections::HashSet;
    let set: HashSet<usize> = picked.iter().copied().collect();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut best: Vec<usize> = Vec::new();
    for &start in picked {
        if seen.contains(&start) {
            continue;
        }
        let mut group = Vec::new();
        let mut stack = vec![start];
        seen.insert(start);
        while let Some(i) = stack.pop() {
            group.push(i);
            let (x, y) = (i % w, i / w);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if set.contains(&j) && seen.insert(j) {
                        stack.push(j);
                    }
                }
            }
        }
        if group.len() > best.len() {
            best = group;
        }
    }
    best
}
