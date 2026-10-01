//! Where to divide a clip into parts: a number of equal ones, one every so
//! many seconds, or one every so many bytes of output.
//!
//! The answer is a list of instants in source time, each the start of a part
//! and the end of the one before it. The list makes the parts out of them --
//! a row each, every one the same recording with the rest cut away -- so the
//! cutting itself is the cut every row already goes through, and a part is
//! something that can be opened in the editor and moved like any other.
//!
//! **Measured along the output, not the recording.** What is divided is what
//! survives the cuts: an hour and a half of programme out of a two-hour
//! capture in three parts is three half hours of programme, not three
//! forty-minute stretches of the file with the commercials still counted.
//!
//! **On an access point where there is one near enough.** A part that begins
//! on one is copied from its first picture on, and the part before it ends
//! where a copy can stop -- so a division there re-encodes nothing but the
//! pictures an open GOP leads with (one, on a broadcast), where one a few
//! frames off it re-encodes a GOP either side. Near enough is
//! half a part either way for a count or a length, which keeps every part on
//! its own side of the next; a size is a ceiling, so there the point is the
//! last one *before* the size runs out, and the parts come out a little
//! short rather than a little over.

use anyhow::{bail, Result};

/// The most parts a clip is divided into. A rule that asks for more is a
/// length typed in the wrong unit, and a thousand rows is a list nobody can
/// use.
pub const MOST_PARTS: usize = 999;

/// How to divide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// Into this many parts of the same length.
    Parts(usize),
    /// A part every this many seconds, the last one whatever is left.
    Every(f64),
    /// A part every this many bytes, as [`weight`] reckons them, the last
    /// one whatever is left.
    Size(u64),
}

/// Where the divisions fall.
#[derive(Debug, Clone, PartialEq)]
pub struct Division {
    /// The instant each part after the first begins, in source time.
    pub at: Vec<f64>,
    /// How long each part runs in the output: one more than `at`.
    pub lengths: Vec<f64>,
    /// How many of `at` are not on an access point, and so re-encode a little
    /// either side of them.
    pub off_point: usize,
}

/// The surviving stretches laid end to end along the output.
struct Line<'a> {
    keeps: &'a [(f64, f64)],
    /// Where each keep begins in the output.
    at: Vec<f64>,
    total: f64,
}

impl<'a> Line<'a> {
    fn new(keeps: &'a [(f64, f64)]) -> Self {
        let mut at = Vec::with_capacity(keeps.len());
        let mut total = 0.0;
        for &(a, b) in keeps {
            at.push(total);
            total += (b - a).max(0.0);
        }
        Line { keeps, at, total }
    }

    /// The source instant at output time `t`. An instant on a join is the
    /// start of the keep after it: that is where the part beginning there
    /// begins.
    fn source(&self, t: f64) -> f64 {
        for (i, &(a, b)) in self.keeps.iter().enumerate() {
            if t < self.at[i] + (b - a) - 1e-9 {
                return a + (t - self.at[i]).max(0.0);
            }
        }
        self.keeps.last().map_or(0.0, |&(_, b)| b)
    }
}

/// Divide `keeps` (what survives the cuts, in source time and in order) by
/// `rule`.
///
/// `points` are the access points' instants in source time, sorted, and are
/// only looked at where `on_points` asks. `frame` is a frame's duration and
/// `head` an instant on the frame grid, so that a division off a point still
/// falls on a frame. `rate` is what the recording weighs, in bytes a second,
/// for [`Rule::Size`].
pub fn divide(
    keeps: &[(f64, f64)],
    points: &[f64],
    rule: Rule,
    on_points: bool,
    frame: f64,
    head: f64,
    rate: f64,
) -> Result<Division> {
    let frame = if frame > 0.0 { frame } else { 1.0 / 30.0 };
    let line = Line::new(keeps);
    let total = line.total;
    if total <= 2.0 * frame {
        bail!("there is nothing left to divide");
    }
    // Every place a part could begin without a re-encode, in output time: the
    // access points inside a keep, and the joins, which are a cut already.
    let mut clean: Vec<f64> = Vec::new();
    for (i, &(a, b)) in keeps.iter().enumerate() {
        if i > 0 {
            clean.push(line.at[i]);
        }
        for &p in points {
            if p > a + frame / 2.0 && p < b - frame / 2.0 {
                clean.push(line.at[i] + (p - a));
            }
        }
    }
    clean.sort_by(f64::total_cmp);
    clean.dedup_by(|x, y| (*x - *y).abs() < frame / 2.0);
    // The latest a part may begin: the last one is at least a frame long.
    let last = total - frame;
    // A division off a point, put on the frame it falls in.
    let exact = |t: f64| {
        let s = line.source(t);
        let on = head + ((s - head) / frame).round() * frame;
        // Back into the keep it was in, if the rounding took it across a cut.
        let src = if (s - on).abs() < frame { on } else { s };
        (t + (src - s), src)
    };

    let mut cuts: Vec<(f64, f64, bool)> = Vec::new(); // (output, source, on a point)
    let place = |t: f64, ceiling: bool, reach: f64, prev: f64| -> Option<(f64, f64, bool)> {
        let lo = prev + frame / 2.0;
        if on_points {
            let near = if ceiling {
                clean.iter().copied().rfind(|&c| c > lo && c <= t + 1e-9)
            } else {
                clean
                    .iter()
                    .copied()
                    .filter(|&c| c > lo && c < last && (c - t).abs() <= reach)
                    .min_by(|x, y| (x - t).abs().total_cmp(&(y - t).abs()))
            };
            if let Some(c) = near {
                return Some((c, line.source(c), true));
            }
        }
        let (o, s) = exact(t);
        (o > lo && o < last + frame / 2.0).then_some((o, s, false))
    };

    match rule {
        Rule::Parts(n) => {
            if n < 2 {
                bail!("divide into at least two parts");
            }
            if n > MOST_PARTS {
                bail!("divide into at most {MOST_PARTS} parts");
            }
            if total / n as f64 <= frame {
                bail!("{n} parts would be less than a frame each");
            }
            let each = total / n as f64;
            for k in 1..n {
                let prev = cuts.last().map_or(0.0, |c| c.0);
                if let Some(c) = place(each * k as f64, false, each / 2.0, prev) {
                    cuts.push(c);
                }
            }
        }
        Rule::Every(len) => {
            if len.is_nan() || len < 1.0 {
                bail!("a part must be at least a second long");
            }
            if (total / len).ceil() > MOST_PARTS as f64 {
                bail!("parts of that length would be more than {MOST_PARTS}");
            }
            let mut k = 1;
            while len * k as f64 <= last {
                let prev = cuts.last().map_or(0.0, |c| c.0);
                if let Some(c) = place(len * k as f64, false, len / 2.0, prev) {
                    cuts.push(c);
                }
                k += 1;
            }
        }
        Rule::Size(bytes) => {
            if rate.is_nan() || rate <= 0.0 {
                bail!("how much the recording weighs is not known");
            }
            let len = bytes as f64 / rate;
            if len < 1.0 {
                bail!("a part of that size would be less than a second long");
            }
            if (total / len).ceil() > MOST_PARTS as f64 {
                bail!("parts of that size would be more than {MOST_PARTS}");
            }
            // One at a time from the last: a part that came out short on a
            // point is followed by one measured from where it really ended.
            let mut prev = 0.0;
            while prev + len <= last {
                let Some(c) = place(prev + len, true, 0.0, prev) else { break };
                prev = c.0;
                cuts.push(c);
            }
        }
    }

    let mut lengths = Vec::with_capacity(cuts.len() + 1);
    let mut from = 0.0;
    for c in &cuts {
        lengths.push(c.0 - from);
        from = c.0;
    }
    lengths.push(total - from);
    Ok(Division {
        off_point: cuts.iter().filter(|c| !c.2).count(),
        at: cuts.into_iter().map(|c| c.1).collect(),
        lengths,
    })
}

/// [`divide`] for a recording that has been read: its access points, its
/// frame and what it weighs.
pub fn divide_source(
    src: &crate::Source,
    keeps: &[(f64, f64)],
    rule: Rule,
    on_points: bool,
) -> Result<Division> {
    let points: Vec<f64> = src.points.iter().map(|p| p.time).collect();
    let head = points.first().copied().unwrap_or(0.0);
    let rate = match rule {
        Rule::Size(_) => weight(src)?,
        _ => 0.0,
    };
    divide(keeps, &points, rule, on_points, src.video.frame_duration(), head, rate)
}

/// What a part weighs, in bytes a second of it.
///
/// The pictures as the index counted them and the sound as the recording
/// declares it, with a twentieth more for what a container wraps them in --
/// a transport stream's packet headers and tables come to about that, and
/// the other containers to less, which on a ceiling is the side to err on.
///
/// Not the file over its length, which is what is left where the pictures
/// were never counted. A broadcast written at a constant rate is padded out
/// with packets that carry nothing, and a part is written without them: on
/// a recording that was half padding, every part came out half the size
/// asked for.
pub fn weight(src: &crate::Source) -> Result<f64> {
    let sound: f64 = src
        .audios
        .iter()
        .map(|a| match a.bit_rate {
            Some(r) if r > 0 => r as f64,
            _ => f64::from(a.sample_rate) * f64::from(a.channels) * f64::from(a.bits),
        })
        .sum();
    if let Some(pictures) = src.video.bit_rate.filter(|r| *r > 0.0) {
        return Ok((pictures + sound) * 1.05 / 8.0);
    }
    let bytes = src.input.bytes()?;
    if src.duration <= 0.0 || bytes == 0 {
        bail!("how much the recording weighs is not known");
    }
    Ok(bytes as f64 / src.duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FD: f64 = 1.0 / 30.0;

    fn points(every: f64, until: f64) -> Vec<f64> {
        (0..).map(|k| k as f64 * every).take_while(|&t| t < until).collect()
    }

    #[test]
    fn equal_parts_land_on_the_nearest_point() {
        let keeps = [(0.0, 100.0)];
        let d = divide(&keeps, &points(2.0, 100.0), Rule::Parts(3), true, FD, 0.0, 0.0).unwrap();
        // 33.3 and 66.7, each to the nearest of the points two seconds apart.
        assert_eq!(d.at, vec![34.0, 66.0]);
        assert_eq!(d.off_point, 0);
        assert_eq!(d.lengths, vec![34.0, 32.0, 34.0]);
    }

    #[test]
    fn exact_divisions_fall_on_frames() {
        let keeps = [(0.0, 100.0)];
        let d = divide(&keeps, &[], Rule::Parts(3), false, FD, 0.0, 0.0).unwrap();
        assert_eq!(d.at.len(), 2);
        assert_eq!(d.off_point, 2);
        for t in &d.at {
            let n = t / FD;
            assert!((n - n.round()).abs() < 1e-6, "{t} is not on a frame");
        }
    }

    #[test]
    fn divided_along_what_survives_the_cuts() {
        // Two keeps of 30 s each, a minute out of the middle taken away. Two
        // parts meet at the join, which is a cut already.
        let keeps = [(0.0, 30.0), (90.0, 120.0)];
        let d = divide(&keeps, &points(5.0, 120.0), Rule::Parts(2), true, FD, 0.0, 0.0).unwrap();
        assert_eq!(d.at, vec![90.0]);
        assert_eq!(d.lengths, vec![30.0, 30.0]);
    }

    #[test]
    fn a_length_leaves_the_rest_to_the_last_part() {
        let keeps = [(0.0, 250.0)];
        let d = divide(&keeps, &points(1.0, 250.0), Rule::Every(60.0), true, FD, 0.0, 0.0).unwrap();
        assert_eq!(d.at, vec![60.0, 120.0, 180.0, 240.0]);
        assert_eq!(d.lengths.last(), Some(&10.0));
    }

    #[test]
    fn a_size_is_never_passed() {
        // A megabyte a second, 25 MB parts, points every 4 s: each part ends
        // on the last point before 25 s of it.
        let keeps = [(0.0, 100.0)];
        let d = divide(&keeps, &points(4.0, 100.0), Rule::Size(25_000_000), true, FD, 0.0, 1e6)
            .unwrap();
        assert_eq!(d.at, vec![24.0, 48.0, 72.0, 96.0]);
        assert!(d.lengths.iter().all(|&l| l <= 25.0));
    }

    #[test]
    fn a_long_gop_falls_back_to_the_frame() {
        // Points a minute apart and parts of twenty seconds: no point is near
        // enough, so the division is exact.
        let keeps = [(0.0, 60.0)];
        let d = divide(&keeps, &[0.0], Rule::Every(20.0), true, FD, 0.0, 0.0).unwrap();
        assert_eq!(d.at.len(), 2);
        assert_eq!(d.off_point, 2);
    }

    #[test]
    fn refuses_what_cannot_be_done() {
        let keeps = [(0.0, 10.0)];
        assert!(divide(&keeps, &[], Rule::Parts(1), true, FD, 0.0, 0.0).is_err());
        assert!(divide(&keeps, &[], Rule::Every(0.5), true, FD, 0.0, 0.0).is_err());
        assert!(divide(&keeps, &[], Rule::Size(1000), true, FD, 0.0, 0.0).is_err());
        assert!(divide(&[(0.0, 2000.0)], &[], Rule::Every(1.0), true, FD, 0.0, 0.0).is_err());
        assert!(divide(&[], &[], Rule::Parts(2), true, FD, 0.0, 0.0).is_err());
    }
}
