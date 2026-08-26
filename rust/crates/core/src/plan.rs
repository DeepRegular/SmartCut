//! Turn a keep-range into a list of copy / re-encode segments.
//!
//! ```text
//! ... I ....... I=========================I ....... I ...
//!       ^t_in   ^k_first                  ^k_term   ^t_out
//!     |<-head->|<--------- body --------->|<-tail->|
//!      re-encode      stream copy          re-encode
//! ```
//!
//! Open GOPs make both ends narrower than they look. A picture that follows
//! an I picture in decode order but presents *before* it -- a leading picture
//! -- references the previous GOP. So:
//!
//! * the copy may start at `k_first`, but that entry point's own leading
//!   pictures cannot come with it (they belong to the head, which is
//!   re-encoded anyway) -- and cutting them away is only safe when none of
//!   them is itself a reference;
//! * the copy must stop before `k_term`, so it cannot deliver the pictures
//!   presenting in `[k_term.lead_start, k_term.time)` either -- those are
//!   decoded after `k_term`. The body's display coverage ends at
//!   `k_term.lead_start`, not at `k_term.time`.
//!
//! For a closed GOP `lead_start == time` and both collapse to the simple case.

use crate::{AccessPoint, VideoInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Copy,
    Reencode,
}

impl SegmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SegmentKind::Copy => "copy",
            SegmentKind::Reencode => "reencode",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub kind: SegmentKind,
    /// Display coverage, inclusive.
    pub start: f64,
    /// Display coverage, exclusive.
    pub end: f64,
    /// Pictures this segment contributes to the output.
    pub frames: usize,
    /// Copy only: presentation time of the access point that ends the copy.
    /// The cutter reads until it reaches that picture. `None` means "to end
    /// of file".
    ///
    /// A decode-order packet count would do the same job, but only an index
    /// built by walking every packet can supply one. Blu-ray's CLIPINF EP map
    /// -- and any other precomputed index -- gives times, so times are what
    /// the plan carries.
    pub copy_until: Option<f64>,
    /// Re-encode only: start decoding here, then discard forward. Seeking
    /// straight to the first wanted frame can land in a GOP that cannot be
    /// decoded on its own.
    pub seek_from: f64,
}

impl Segment {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

#[derive(Debug, Clone)]
pub struct RangePlan {
    pub t_in: f64,
    pub t_out: f64,
    pub segments: Vec<Segment>,
}

impl RangePlan {
    pub fn copied(&self) -> f64 {
        self.sum(SegmentKind::Copy)
    }

    pub fn reencoded(&self) -> f64 {
        self.sum(SegmentKind::Reencode)
    }

    fn sum(&self, kind: SegmentKind) -> f64 {
        self.segments.iter().filter(|s| s.kind == kind).map(Segment::duration).sum()
    }
}

/// An access point far enough before `target` to decode into it cleanly.
fn safe_seek(points: &[AccessPoint], target: f64, back: usize) -> f64 {
    let earlier: Vec<f64> =
        points.iter().filter(|p| p.time <= target + 1e-6).map(|p| p.time).collect();
    match earlier.len() {
        0 => 0.0,
        n => earlier[n.saturating_sub(1 + back)],
    }
}

pub struct PlanOptions {
    /// Allow open-GOP access points to start a copy when their leading
    /// pictures are droppable.
    pub allow_open_gop: bool,
    /// Shorter copies buy nothing but a seam; re-encode instead.
    pub min_copy: Option<f64>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self { allow_open_gop: true, min_copy: None }
    }
}

pub fn plan_range(
    video: &VideoInfo,
    duration: f64,
    points: &[AccessPoint],
    t_in: f64,
    t_out: f64,
    opts: &PlanOptions,
) -> RangePlan {
    // Snap onto the frame grid first. At fractional rates (30000/1001) a
    // request stated in seconds sits between frames, and head/tail durations
    // then round to a different frame count than the caller expects.
    //
    // The requested bounds are used as given. Snapping them to multiples of
    // the frame duration used to be necessary for the frame arithmetic, but
    // that arithmetic no longer drives anything: the cutter measures each
    // segment's span from the pictures it actually emits. Snapping now only
    // does harm -- a stream's pictures sit at an arbitrary phase, so moving a
    // bound by up to half a frame can drop the picture that should have ended
    // the range.
    let fps = if video.frame_rate > 0.0 { video.frame_rate } else { 30.0 };
    let index = |t: f64| (t * fps).round();

    // Nothing before the file's first access point can be decoded -- a
    // recording that begins mid-GOP, or any byte-sliced stream, simply has no
    // pictures there. Asking for them yields an empty re-encode, so start
    // where the pictures start.
    let t_in = t_in.max(points.first().map_or(0.0, |p| p.time));

    let fd = video.frame_duration();
    let eps = fd / 2.0;
    let min_copy = opts.min_copy.unwrap_or_else(|| (2.0 * fd).max(0.5));

    let finish = |mut segments: Vec<Segment>| -> RangePlan {
        for s in &mut segments {
            s.frames = (index(s.end) - index(s.start)).max(0.0) as usize;
        }
        RangePlan { t_in, t_out, segments }
    };
    let full_reencode = || {
        finish(vec![Segment {
            kind: SegmentKind::Reencode,
            start: t_in,
            end: t_out,
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, t_in, 2),
        }])
    };

    if t_out <= t_in {
        return RangePlan { t_in, t_out, segments: Vec::new() };
    }

    let usable: Vec<&AccessPoint> =
        points.iter().filter(|p| opts.allow_open_gop || !p.open_gop()).collect();

    // Any access point can *end* a copy, but starting one at an open GOP is
    // only possible when its leading pictures can be cut away.
    let Some(k_first) = usable
        .iter()
        .find(|p| p.time >= t_in - eps && p.time <= t_out + eps && (!p.open_gop() || p.droppable))
    else {
        return full_reencode();
    };

    // (display coverage end, terminating packet index)
    let mut stops: Vec<(f64, Option<f64>)> = usable
        .iter()
        .filter(|p| p.time > k_first.time + eps && p.lead_start <= t_out + eps)
        .map(|p| (p.lead_start, Some(p.time)))
        .collect();
    if duration > 0.0 && t_out >= duration - eps {
        stops.push((t_out, None));
    }
    let Some(&(copy_end, copy_until)) =
        stops.iter().max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
    else {
        return full_reencode();
    };

    if copy_end - k_first.time < min_copy {
        return full_reencode();
    }

    let mut segments = Vec::new();
    let mut body_start = k_first.time;
    // A head only exists if a whole picture fits before the entry point.
    // Pictures sit one frame duration apart ending at `k_first.time`, so a
    // gap shorter than that contains none -- and asking the cutter to
    // re-encode an empty window is an error, not an empty segment.
    if body_start - t_in >= fd - 1e-9 {
        segments.push(Segment {
            kind: SegmentKind::Reencode,
            start: t_in,
            end: body_start,
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, t_in, 2),
        });
    } else {
        // No room for a head: the range effectively begins at the entry
        // point, and the audio has to be anchored there too.
        body_start = k_first.time;
    }
    segments.push(Segment {
        kind: SegmentKind::Copy,
        start: body_start,
        end: copy_end,
        frames: 0,
        copy_until,
        seek_from: k_first.time,
    });
    if t_out - copy_end > 1e-9 {
        segments.push(Segment {
            kind: SegmentKind::Reencode,
            start: copy_end,
            end: t_out,
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, copy_end, 2),
        });
    }
    // Report the bounds the output actually covers, so audio lines up with
    // the video that was really produced.
    let effective_in = segments.first().map_or(t_in, |s| s.start);
    let effective_out = segments.last().map_or(t_out, |s| s.end);
    let mut plan = finish(segments);
    plan.t_in = effective_in;
    plan.t_out = effective_out;
    plan
}

pub fn plan(
    video: &VideoInfo,
    duration: f64,
    points: &[AccessPoint],
    ranges: &[(f64, f64)],
    opts: &PlanOptions,
) -> Vec<RangePlan> {
    ranges.iter().map(|&(a, b)| plan_range(video, duration, points, a, b, opts)).collect()
}
