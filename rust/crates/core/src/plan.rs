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
        // A re-encode window with no picture in it is not a small piece of
        // work, it is an error: the cutter decodes the window, finds nothing
        // to put in it and says so, and the whole run stops. It arises where
        // a bound lands within a rounding of an entry point -- the copy ends
        // a hair short of the bound and the sliver left over is thinner than
        // the gap between two pictures. The bounds below keep those out; this
        // is the net under them, and it also keeps the plan honest, since a
        // segment that reports no frames is not one the output needs.
        segments.retain(|s| s.kind != SegmentKind::Reencode || s.frames > 0);
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
    // Half a frame, for the same reason the head asks for a whole one: the
    // pictures the tail would have to supply sit `fd` apart, so a window
    // thinner than that holds one only if the phase is right, and one
    // thinner than half of it is a rounding rather than a picture. The
    // reference implementation draws the line in the same place.
    if t_out - copy_end > eps {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::NalFraming;

    /// Broadcast video: 29.97 fps, interlaced, open GOPs half a second apart.
    fn video() -> VideoInfo {
        VideoInfo {
            stream_index: 0,
            codec: "mpeg2video".into(),
            width: 1440,
            height: 1080,
            frame_rate: 30000.0 / 1001.0,
            has_b_frames: 1,
            time_base: 1.0 / 90000.0,
            sample_aspect_ratio: 4.0 / 3.0,
            framing: NalFraming::AnnexB,
            pulldown: false,
            field_order: 2,
            vc1: None,
        }
    }

    /// Entry points on a half-second grid, each with one leading picture --
    /// which is what puts `lead_start` one frame ahead of `time` and lets a
    /// copy stop a frame short of an entry point.
    fn points(n: usize, fd: f64) -> Vec<AccessPoint> {
        (0..n)
            .map(|i| {
                let time = i as f64 * 15.0 * fd;
                AccessPoint {
                    time,
                    lead_start: if i == 0 { time } else { time - fd },
                    lead_indices: if i == 0 { Vec::new() } else { vec![1] },
                    droppable: true,
                    pos: -1,
                }
            })
            .collect()
    }

    /// The bug this guards against: a bound landing a rounding away from
    /// where the copy has to stop left a re-encode segment with no picture
    /// in it, and the cutter -- rightly -- refuses a window it can decode
    /// nothing out of, so the whole run stopped.
    #[test]
    fn a_bound_a_hair_past_the_copy_leaves_no_empty_segment() {
        let video = video();
        let fd = video.frame_duration();
        let points = points(40, fd);
        // Ends 1/1000 of a frame after an entry point's leading picture,
        // which is where the copy's coverage ends.
        let t_out = points[10].lead_start + fd / 1000.0;
        let plan = plan_range(&video, 300.0, &points, 0.0, t_out, &PlanOptions::default());
        assert!(plan.segments.iter().all(|s| s.frames > 0), "{:?}", plan.segments);
        assert_eq!(plan.segments.last().unwrap().kind, SegmentKind::Copy);
    }

    /// And the tail is still written where there is a picture in it.
    #[test]
    fn a_bound_a_picture_past_the_copy_still_gets_its_tail() {
        let video = video();
        let fd = video.frame_duration();
        let points = points(40, fd);
        let t_out = points[10].lead_start + 2.0 * fd;
        let plan = plan_range(&video, 300.0, &points, 0.0, t_out, &PlanOptions::default());
        let tail = plan.segments.last().unwrap();
        assert_eq!(tail.kind, SegmentKind::Reencode);
        assert!(tail.frames > 0, "{tail:?}");
        assert!((plan.t_out - t_out).abs() < 1e-9);
    }
}
