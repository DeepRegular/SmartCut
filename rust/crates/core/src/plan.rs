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

use crate::restamp::Seam;
use crate::input::ReadPackets;
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

/// What a transition does to one stretch of pictures.
///
/// Hung off the segment rather than off the range, because a transition is
/// a stretch *within* a range and the range is what the sound is cut
/// against: a range split in two to make room for a fade would put a seam in
/// the sound where the picture has none. See [`crate::transition`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Retouch {
    /// Taken down to a colour across this segment, or brought up out of one.
    Tint {
        shade: crate::transition::Shade,
        /// True where the pictures go into the colour, false where they
        /// come out of it.
        going_in: bool,
        easing: crate::transition::Easing,
    },
    /// Shown together with the reel that follows, whose own pictures from
    /// `theirs` onwards are read alongside these.
    Cross {
        kind: crate::transition::Crossing,
        easing: crate::transition::Easing,
        theirs: f64,
    },
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
    /// Set where this stretch is a transition. Always a re-encode: a
    /// transition asks for pictures that are in neither recording, so there
    /// is nothing here a copy could carry. `None` for every segment of an
    /// ordinary cut.
    pub retouch: Option<Retouch>,
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
        self.segments
            .iter()
            .filter(|s| s.kind == kind)
            .map(Segment::duration)
            .sum()
    }
}

/// An access point far enough before `target` to decode into it cleanly.
fn safe_seek(points: &[AccessPoint], target: f64, back: usize) -> f64 {
    let earlier: Vec<f64> = points
        .iter()
        .filter(|p| p.time <= target + 1e-6)
        .map(|p| p.time)
        .collect();
    match earlier.len() {
        0 => 0.0,
        n => earlier[n.saturating_sub(1 + back)],
    }
}

#[derive(Debug, Clone)]
pub struct PlanOptions {
    /// Allow open-GOP access points to start a copy when their leading
    /// pictures are droppable.
    pub allow_open_gop: bool,
    /// Shorter copies buy nothing but a seam; re-encode instead.
    pub min_copy: Option<f64>,
    /// How much more of a range to re-encode to reach an entry point a copy
    /// can be joined onto cleanly, in seconds.
    ///
    /// **Off unless asked for, because what it buys is smaller than what it
    /// costs.** See [`clean_the_join`] for both halves of that.
    pub clean_join: Option<f64>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            allow_open_gop: true,
            min_copy: None,
            clean_join: None,
        }
    }
}

/// A range with nothing copied in it: one segment, written afresh from end
/// to end.
///
/// **What a plan is for does not arise here.** Everything above divides a
/// range into the stretch that can be copied and the partial GOPs at its
/// ends, because the copy is the point and the re-encoding is the price. A
/// clip being written into another recording's shape has no copy available
/// at any point of it -- every picture has to be decoded and made again --
/// so there is nothing to divide and no entry point to reach for. See
/// [`crate::conform`].
///
/// The seek still goes back two entry points, for the same reason it does
/// anywhere: the first picture wanted may not be one that can be decoded on
/// its own.
pub fn reencode_range(points: &[AccessPoint], t_in: f64, t_out: f64) -> RangePlan {
    let t_in = t_in.max(points.first().map_or(0.0, |p| p.time));
    // Nothing left once the start is where the pictures start -- a range
    // wholly in front of the first entry point, or one [`plan_range`] had
    // already found empty. A segment of no length asks the cutter for
    // pictures that are not there, and it stops the whole run saying so.
    if t_out <= t_in {
        return RangePlan {
            t_in,
            t_out,
            segments: Vec::new(),
        };
    }
    RangePlan {
        t_in,
        t_out,
        segments: vec![Segment {
            kind: SegmentKind::Reencode,
            start: t_in,
            end: t_out,
            // Counted by the caller where it is counted at all: how many
            // pictures this comes to is a question about the rate of the
            // file it is going into, not about the recording being read.
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, t_in, 2),
            retouch: None,
        }],
    }
}

/// How many pictures are on screen from `a` up to `b`, not counting a picture
/// that begins at `b`.
///
/// Counted on the grid the pictures really sit on, which is the one the
/// nearest entry point is on. A recording's pictures start wherever its clock
/// started, not on multiples of the frame duration, and rounding each bound
/// to the nearest multiple instead miscounts by one whenever the two bounds
/// round different ways -- a re-encode of fourteen pictures reported as
/// thirteen on the output screen, measured against the file written.
///
/// `duration` is the recording's length. A range asked for to the end of it
/// runs past the start of a picture that is not there -- the last one began a
/// frame before the end -- so the count stops half a frame short of it.
///
/// `video` says how finely the container keeps time: see [`coarse_tick`].
fn frames_in(
    video: &VideoInfo,
    points: &[AccessPoint],
    fps: f64,
    a: f64,
    b: f64,
    duration: f64,
) -> usize {
    let b = if duration > 0.0 { b.min(duration - 0.5 / fps) } else { b };
    if b <= a || fps <= 0.0 {
        return 0;
    }
    let phase = phase_near(points, a);
    // A bound within a hundredth of a picture of one is taken as that
    // picture, which is what a time read off a picture comes back as after a
    // trip through floating point. On a coarse clock, within a tick and a
    // half: an entry point's stored time -- where a copy starts or stops --
    // and the phase are each rounded to a tick, so a picture sits up to a
    // tick either side of the grid, and the bounds [`on_the_pictures`] moved
    // are two ticks ahead of it. With a hundredth, a Matroska file's
    // leading picture stored 33 ms before its entry point (0.989 of a frame
    // at 29.97) was counted as the entry point's own, a tail holding just
    // that picture counted none, and [`plan_range`] dropped it from the cut.
    let slack = coarse_tick(video).map_or(0.01, |tick| (1.5 * tick * fps).max(0.01));
    // The first picture at or after `t`.
    let first = |t: f64| ((t - phase) * fps - slack).ceil();
    (first(b) - first(a)).max(0.0) as usize
}

/// The container's tick, where it keeps time too coarsely for a picture's
/// stored time to be told from a bound a fraction of a millisecond away.
/// See [`on_the_pictures`]. `None` on a fine clock, and on a recording whose
/// pictures do not come at a rate.
fn coarse_tick(video: &VideoInfo) -> Option<f64> {
    let fd = video.frame_duration();
    // Capped so that the two ticks either side of a grid point stay well
    // inside a frame: the picture before has to be clear of the moved bound.
    let tick = video.time_base.min(fd / 12.0);
    if video.variable_rate || !(tick.is_finite() && fd.is_finite()) || tick <= fd * 0.01 {
        return None;
    }
    Some(tick)
}

/// The time of the entry point nearest `t`, which is the phase of the grid
/// the pictures around `t` sit on. 0 where there are none.
fn phase_near(points: &[AccessPoint], t: f64) -> f64 {
    let at = points.partition_point(|p| p.time < t);
    [at.checked_sub(1), Some(at)]
        .into_iter()
        .flatten()
        .filter_map(|i| points.get(i))
        .map(|p| p.time)
        .min_by(|x, y| (x - t).abs().total_cmp(&(y - t).abs()))
        .unwrap_or(0.0)
}

/// The ranges, with every bound that sits on a picture moved to just ahead
/// of it, where the container keeps time too coarsely for the picture's
/// stored time and the bound to be told apart.
///
/// **Matroska and WebM count in milliseconds**, and so does an MP4 written
/// with a timescale of 1000. A picture whose true time is 10.0544 is stored
/// as 10.054, and a bound worked out from the picture before it -- the
/// editor ends a cut one frame after the picture it was stood on, which is
/// 10.021 + 1/29.97 = 10.054367 -- lands a third of a millisecond *past* the
/// picture it means. Everything after this compares the two with a slack
/// meant for a 90 kHz clock (a thousandth of a frame in the cutter and the
/// check, a hundredth in [`frames_in`]), so the picture fell out of the range
/// about half the time, and the check, asking the same question the same
/// way, agreed with the loss.
///
/// So the question is settled once, here, for every one of them: a bound
/// within two ticks of the clock (`tick` below) of where a picture sits on
/// the grid the pictures are on is that picture's bound, and is moved to two
/// ticks ahead of the grid point. A stored time is the true one rounded to a
/// tick, and the grid's phase is an entry point's stored time, so the
/// picture is within a tick of the grid point either way: two ticks ahead of
/// the grid point is ahead of the picture by at least one, and behind the
/// picture before it by most of a frame. Every comparison after this one
/// then has a clear answer at its own slack, and the same answer. A bound
/// further from the grid than that was between two pictures already.
///
/// Only where the clock is coarser than a hundredth of a frame, which a
/// transport stream's, a disc's and an ordinary MP4's are not: their bounds
/// are passed through as they are. Nor on a recording whose pictures do not
/// come at a rate (the grid is not where they are), nor at or past the end
/// of the recording, which means "to the end" and is held to as that.
///
/// What moves with the bound is the sound, which is anchored to it: by at
/// most three ticks against the picture, three milliseconds on Matroska.
fn on_the_pictures(
    video: &VideoInfo,
    duration: f64,
    points: &[AccessPoint],
    ranges: &[(f64, f64)],
) -> Vec<(f64, f64)> {
    let fd = video.frame_duration();
    let Some(tick) = coarse_tick(video) else {
        return ranges.to_vec();
    };
    // The grid is of fields wherever a picture can be held for three of
    // them: soft telecine (which a remux of MPEG-2 film need not say -- an
    // index read off the container leaves `pulldown` unset), and an
    // interlaced broadcast that repeats a field now and then. After such a
    // picture the ones that follow sit half a frame off the frame grid, and
    // a bound on one of them was left where it was -- a fraction of a
    // millisecond past the picture after OUT, which the cut then lost (137
    // of 1004 OUT positions on a soft-telecine clip remuxed to Matroska).
    // A field point no picture sits on is half a frame from every picture,
    // so a bound moved off it by two ticks stays between the same two.
    let fields = video.pulldown
        || video.interlaced()
        || matches!(video.codec.as_str(), "mpeg1video" | "mpeg2video");
    let step = if fields { fd / 2.0 } else { fd };
    let near = |t: f64| {
        if !t.is_finite() || (duration > 0.0 && t >= duration) {
            return t;
        }
        let phase = phase_near(points, t);
        let grid = phase + ((t - phase) / step).round() * step;
        if (t - grid).abs() < 2.0 * tick {
            grid - 2.0 * tick
        } else {
            t
        }
    };
    ranges.iter().map(|&(a, b)| (near(a), near(b))).collect()
}

/// The start of the picture on screen at `t`, as a bound: where a stretch
/// written after a range's body has to begin for the two to meet on a
/// picture. `t` itself where it is on one already, and on a recording whose
/// pictures do not come at a rate.
///
/// **Two segments that meet part-way through a picture both write it**,
/// because they ask different questions of it. The body of a range writes
/// the pictures that *begin* inside it -- a re-encoded tail stops short of
/// its end, a copy stops at an entry point's leading pictures -- and a
/// transition written after it ([`crate::cut_conform`] laying out a crossing
/// or a fade) opens on the picture *on screen* at its first instant. A bound
/// inside a picture is both: the body wrote it last and the transition wrote
/// it first, every picture of the transition after it came a frame late, and
/// the range's own last picture was the one left out. That is every
/// transition whose length is not a whole number of frames on a range the
/// editor ended on a picture -- the default second at 29.97 is 29.97 frames.
/// And where a copy then ended on an entry point just past the bound, the
/// crossing came out a frame shorter than the clip after it gives up, and a
/// picture of that clip was lost instead.
///
/// On the grid [`frames_in`] counts on, with the coarse-clock margin
/// [`on_the_pictures`] gives a bound on a picture. Soft telecine puts its
/// pictures on field points; everything else is on frames.
pub(crate) fn on_screen_at(video: &VideoInfo, points: &[AccessPoint], t: f64) -> f64 {
    let fd = video.frame_duration();
    if video.variable_rate || !(fd.is_finite() && fd > 0.0) || !t.is_finite() {
        return t;
    }
    let step = if video.pulldown { fd / 2.0 } else { fd };
    let tick = coarse_tick(video);
    let phase = phase_near(points, t);
    // A bound already on a picture -- within a thousandth of a frame of one,
    // or the two ticks ahead of it [`on_the_pictures`] puts one -- stays.
    let ahead = tick.map_or(fd * 1e-3, |k| 2.5 * k);
    let grid = phase + ((t - phase + ahead) / step).floor() * step;
    let on = match tick {
        Some(k) => grid - 2.0 * k,
        None => grid,
    };
    if on >= t - fd * 1e-3 {
        t
    } else {
        on
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
    let fps = if video.frame_rate > 0.0 {
        video.frame_rate
    } else {
        30.0
    };

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
            s.frames = frames_in(video, points, fps, s.start, s.end, duration);
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
        RangePlan {
            t_in,
            t_out,
            segments,
        }
    };
    let full_reencode = || {
        finish(vec![Segment {
            kind: SegmentKind::Reencode,
            start: t_in,
            end: t_out,
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, t_in, 2),
                        retouch: None,
                    }])
    };

    if t_out <= t_in {
        return RangePlan {
            t_in,
            t_out,
            segments: Vec::new(),
        };
    }

    let usable: Vec<&AccessPoint> = points
        .iter()
        .filter(|p| opts.allow_open_gop || !p.open_gop())
        .collect();

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
    let Some(&(copy_end, copy_until)) = stops
        .iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
    else {
        return full_reencode();
    };

    if copy_end - k_first.time < min_copy {
        return full_reencode();
    }

    let mut segments = Vec::new();
    let body_start = k_first.time;
    // A head only exists if a whole picture fits before the entry point.
    // Pictures sit one frame duration apart ending at `k_first.time`, so a
    // gap shorter than that contains none -- and asking the cutter to
    // re-encode an empty window is an error, not an empty segment.
    //
    // Where there is none, the range simply begins at the entry point
    // instead, and the audio is anchored there with it: that is what
    // `effective_in` below reports back.
    if body_start - t_in >= fd - 1e-9 {
        segments.push(Segment {
            kind: SegmentKind::Reencode,
            start: t_in,
            end: body_start,
            frames: 0,
            copy_until: None,
            seek_from: safe_seek(points, t_in, 2),
                          retouch: None,
                      });
    }
    segments.push(Segment {
        kind: SegmentKind::Copy,
        start: body_start,
        end: copy_end,
        frames: 0,
        copy_until,
        seek_from: k_first.time,
                      retouch: None,
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
                          retouch: None,
                      });
    }
    // Report the bounds the output actually covers, so audio lines up with
    // the video that was really produced -- after `finish`, which drops a
    // tail with no picture in it: taken before, the range's sound ran on to
    // where that tail would have ended.
    let mut plan = finish(segments);
    plan.t_in = plan.segments.first().map_or(t_in, |s| s.start);
    plan.t_out = plan.segments.last().map_or(t_out, |s| s.end);
    plan
}

pub fn plan(
    video: &VideoInfo,
    duration: f64,
    points: &[AccessPoint],
    ranges: &[(f64, f64)],
    opts: &PlanOptions,
) -> Vec<RangePlan> {
    ranges
        .iter()
        .map(|&(a, b)| plan_range(video, duration, points, a, b, opts))
        .collect()
}

/// As [`plan`], and then moved off the entry points a copy cannot be joined
/// onto cleanly.
///
/// The reading of the recording is what [`plan`] cannot do for itself: which
/// entry points restart the coded video sequence is not in the index, and on
/// a disc the index was never walked at all -- it came off the disc's own
/// table. So this is the entry point for a caller that has the recording in
/// hand, and [`plan`] stays the arithmetic.
pub fn plan_on(src: &crate::Source, ranges: &[(f64, f64)], opts: &PlanOptions) -> Vec<RangePlan> {
    let seams = seam_times(&src.joins);
    let ranges = on_the_pictures(&src.video, src.duration, &src.points, ranges);
    let ranges = at_the_seams(&ranges, &src.joins, &src.points, &src.video);
    let ranges = past_the_seam(&ranges, &seams, &src.points, &src.video);
    let mut plans: Vec<RangePlan> = ranges
        .iter()
        .map(|&(a, b)| {
            let plan = plan_range(&src.video, src.duration, &src.points, a, b, opts);
            match own_points(src, &plan) {
                Some(own) => plan_range(&src.video, src.duration, &own, a, b, opts),
                None => plan,
            }
        })
        .collect();
    for p in &mut plans {
        clean_the_join(src, p, opts);
    }
    plans
}

/// The entry points of the stretch `plan` lies in, where its copy was
/// planned to end on an entry point of the next stretch and leave a
/// re-encode behind it; `None` otherwise, which is every recording without
/// seams and nearly every range of one with them.
///
/// **A copy never reaches an entry point of the next stretch.** The cutter
/// stops reading a stretch at the seam's byte (`stretch_bytes` in
/// [`crate::cut`]), so a copy told to run to such a point runs to the seam
/// instead. The next stretch can open on leading pictures stamped in front
/// of its seam, though, and its first entry point then ends a copy by
/// coverage: the copy was planned to end at those pictures and a re-encode
/// to fill from there to the seam. On a recorder BD-RE whose stretch stops
/// half a second before its seam, that re-encode had none of the stretch's
/// pictures to write and wrote the one already up a second time -- after
/// the copy had written it -- and the check found a picture more than the
/// recording has. Where the stretch does run on to the seam, the copy and
/// the re-encode would both write its last pictures. Planned on its own
/// entry points, the copy ends on its last one and the re-encode holds the
/// pictures after it, once.
///
/// A copy that runs to the seam with nothing after it is planned the same
/// way where the stretch runs on past the seam's time. Stopping at the
/// seam's byte is not stopping at the seam's time: a recorder's stretch can
/// hold pictures stamped up to a third of a second past the time the next
/// one begins at on the joined clock, and the copy wrote them all -- three
/// pictures past the end of the range, in front of the next stretch's first,
/// and the check found three more than were asked for. Ended on the
/// stretch's own last entry point, the re-encode after it stops where the
/// range does. Where the stretch ends where its seam says, the copy to the
/// seam's byte is the range's own pictures and nothing else, and is left as
/// it is: re-planned, the last GOP before every seam was re-encoded for
/// nothing (5.45 s of a recorder title became 8.67 s).
fn own_points(src: &crate::Source, plan: &RangePlan) -> Option<Vec<AccessPoint>> {
    let fd = src.video.frame_duration();
    let eps = fd / 2.0;
    let k = src.joins.iter().position(|j| j.time >= plan.t_out - eps)?;
    let seam = &src.joins[k];
    let wall = seam.at;
    let beyond = |p: &AccessPoint| p.pos >= 0 && p.pos as u64 >= wall;
    let reaches = |s: &Segment| {
        s.kind == SegmentKind::Copy
            && s.copy_until
                .is_some_and(|u| src.points.iter().any(|p| p.time == u && beyond(p)))
    };
    let foreign_tail = plan
        .segments
        .windows(2)
        .any(|w| reaches(&w[0]) && w[1].kind == SegmentKind::Reencode);
    let to_the_wall = !foreign_tail && plan.segments.iter().any(reaches);
    if !foreign_tail && !to_the_wall {
        return None;
    }
    if to_the_wall {
        let lo = k.checked_sub(1).map_or(0, |i| src.joins[i].at);
        let stop = seam.ends.min(seam.time);
        // Unread is taken as running past: a GOP re-encoded that need not
        // have been costs less than pictures written past the range.
        let last = stretch_last_picture(src, lo, wall);
        if last.is_some_and(|t| t < stop - eps) {
            return None;
        }
    }
    Some(src.points.iter().filter(|p| !beyond(p)).cloned().collect())
}

/// How much of a stretch's end is read for its last picture; as
/// `crate::index`'s own reading of a stretch's tail.
const TAIL_BYTES: u64 = 8 << 20;

/// The instant of the last picture of the stretch `lo..hi` (bytes), read off
/// its tail; `None` where it could not be read.
///
/// Kept per clip and stretch for as long as the program runs: the editor
/// plans on every edit, and the answer is the same every time.
fn stretch_last_picture(src: &crate::Source, lo: u64, hi: u64) -> Option<f64> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Key = (String, u64, u64, usize, u64);
    static SEEN: OnceLock<Mutex<HashMap<Key, Option<f64>>>> = OnceLock::new();
    let key: Key = (
        src.input.url.clone(),
        lo,
        hi,
        src.points.len(),
        src.duration.to_bits(),
    );
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&known) = seen.lock().ok()?.get(&key) {
        return known;
    }
    let found = (|| {
        let mut ictx = crate::input::demux(&src.input.url).ok()?;
        let idx = src.video.stream_index;
        crate::input::keep_only(&mut ictx, &[idx]);
        let from = hi.saturating_sub(TAIL_BYTES).max(lo);
        let placed = unsafe {
            ffmpeg_next::ffi::av_seek_frame(
                ictx.as_mut_ptr(),
                -1,
                from as i64,
                ffmpeg_next::ffi::AVSEEK_FLAG_BYTE,
            ) >= 0
        };
        if !placed {
            return None;
        }
        let tb = src.video.time_base;
        let mut last = f64::NEG_INFINITY;
        let mut read_at = from;
        let mut reading = ictx.read_packets();
        for (stream, packet) in reading.by_ref() {
            if packet.position() >= 0 {
                read_at = packet.position() as u64;
            }
            if read_at >= hi {
                break;
            }
            if stream.index() != idx || read_at < from {
                continue;
            }
            if let Some(pts) = packet.pts() {
                last = last.max(pts as f64 * tb - src.start_time);
            }
        }
        if reading.finished().is_err() {
            return None;
        }
        last.is_finite().then_some(last)
    })();
    if let Ok(mut seen) = seen.lock() {
        seen.insert(key, found);
    }
    found
}

/// Cut every kept range at the seams the recording carries.
///
/// **A seam is a cut somebody else already made.** A recorder that stops and
/// starts writes each stretch with a clock of its own, and joining them --
/// see [`crate::restamp`] -- puts the times in one line but not the pictures:
/// the first pictures of a stretch reference pictures from before the
/// recorder stopped, which are minutes of broadcast away and were never
/// written down. Copied straight across, a decoder shows the wreckage until
/// the next picture that restarts it, which on a recorder's own stream can be
/// a minute later.
///
/// So the seam is planned as a cut. That is the operation this program
/// already does correctly at every boundary a person draws: the far side
/// opens with a re-encoded head that starts a coded video sequence of its
/// own, and the output timeline closes up behind it because a segment
/// occupies the fields it writes and not the times it came from.
///
/// Costs a second or two of re-encoding per seam, which is what a cut costs
/// anywhere. A recording with no seams is planned exactly as it was.
fn at_the_seams(
    ranges: &[(f64, f64)],
    joins: &[Seam],
    points: &[AccessPoint],
    video: &VideoInfo,
) -> Vec<(f64, f64)> {
    if joins.is_empty() {
        return ranges.to_vec();
    }
    let eps = video.frame_duration() / 2.0;
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let mut at = a;
        for seam in joins {
            // Every seam inside the range, however near either end of it. A
            // range left whole across a seam a few pictures from its end
            // copied or re-encoded straight over it, and wrote the next
            // stretch's leading pictures -- the ones whose references were
            // never recorded -- as the wreckage they decode to. An OUT on the
            // last picture of a stretch is drawn a picture past the seam and
            // was exactly that. The piece in front of a seam is kept however
            // short (it is pictures the person chose); the piece behind one
            // is dropped below when it ends before the stretch's first entry
            // point, since what lies there ahead of it cannot be decoded.
            if seam.time > at + 1e-3 && seam.time < b {
                // `ends` and `time` are the same instant on every recording
                // whose sequence table describes the pictures it has. Where
                // they are not, what lies between them is a stretch of the
                // joined clock the recording holds nothing at -- see
                // [`crate::restamp::Seam`] -- and the range stops at the last
                // picture rather than reaching past it.
                let stops = seam.ends.min(seam.time);
                // A mend can pull `ends` back past the start of the piece
                // being cut -- a whole stretch whose pictures end before the
                // range reached it. There is nothing to keep there, and a
                // range of no length is not one: the cutter would be asked
                // for a segment with no picture in it.
                if stops > at {
                    out.push((at, stops));
                }
                at = seam.time;
            }
        }
        // Kept from the stretch's first entry point on where that is inside
        // it, as [`past_the_seam`] then begins it: an OUT on the second or
        // third picture after a seam is those pictures, and they decode.
        // Asked of the entry point rather than of the piece's length -- a
        // piece a few pictures long was dropped whole, and with it the
        // pictures from the entry point to the OUT. Half a picture of it at
        // least, so that an OUT drawn on the entry point itself is no piece.
        let decodes = || {
            points
                .iter()
                .find(|p| p.time >= at - eps)
                .is_some_and(|p| p.time < b - eps)
        };
        if at == a || decodes() {
            out.push((at, b));
        }
    }
    out
}

/// The seams, as the times the planner works in.
fn seam_times(joins: &[Seam]) -> Vec<f64> {
    joins.iter().map(|s| s.time).collect()
}

/// Begin a range that begins at a seam at the first entry point past it.
///
/// **What a stretch holds in front of its first entry point cannot be
/// decoded.** Those are the pictures the recorder wrote before it had coded
/// anything to predict them from -- they reference pictures from before it
/// stopped, which are minutes of broadcast away and were never written down.
/// Where a stretch begins is read off an index that does not state that
/// window, so the seam lands inside it or a little in front of it, and either
/// way there is nothing between the seam and the entry point that this can
/// re-encode.
///
/// Planned as an ordinary head, that window asked the cutter for pictures
/// that are not there, and the cut stopped with nothing decoded in it. So the
/// range starts at the first entry point instead.
///
/// A player shows no more of it than this does: a disc's own play item starts
/// at an entry point for the same reason. What the range gives up with them
/// is the sound recorded alongside -- a third of a second on the recordings
/// measured here -- because sound kept past the picture it belongs with is
/// sound the rest of the range is out of step with.
fn past_the_seam(
    ranges: &[(f64, f64)],
    joins: &[f64],
    points: &[AccessPoint],
    video: &VideoInfo,
) -> Vec<(f64, f64)> {
    if joins.is_empty() {
        return ranges.to_vec();
    }
    let eps = video.frame_duration() / 2.0;
    ranges
        .iter()
        .map(|&(a, b)| {
            // The seam of the stretch this range begins in. Not only a range
            // that begins on the seam: one that begins a few frames past it,
            // still in front of the stretch's first entry point, has nothing
            // there to write either. The decoder drops those pictures, the
            // head's first picture is the entry point's, and the range's
            // sound and captions -- anchored where it was asked to begin --
            // ran that much behind its pictures to the end of the range
            // (0.17 s, measured on a recorder's disc).
            let Some(j) = joins.iter().copied().filter(|&j| j - eps <= a).reduce(f64::max) else {
                return (a, b);
            };
            match points.iter().find(|p| p.time >= j - eps) {
                Some(p) if p.time > a && p.time < b => (p.time, b),
                // The whole of the range is in that window: a range ended
                // within a GOP of the seam. Nothing in it can be written, and
                // left as it was it asked the cutter for those pictures and
                // stopped the run. Of no length, it plans to nothing, as a
                // range shorter than a picture does.
                Some(p) if p.time > a && p.time >= b => (b, b),
                _ => (a, b),
            }
        })
        .collect()
}

/// Move the start of a range's copied body onto an entry point that restarts
/// the coded video sequence, re-encoding the little more that takes.
///
/// **What this fixes is a picture out of order at the seam.** A decoder hands
/// its pictures back in picture-order-count order, and the counts either side
/// of a splice are not one another's: the re-encoded head opens a coded video
/// sequence of its own and counts from nought, while a copied segment that
/// begins on an I picture with a recovery point -- which is what a Blu-ray
/// mostly puts at an entry point -- carries the counts the recording gave it
/// and restarts nothing. Where the head's last count lands above the copied
/// picture's, the two come out of the decoder the wrong way round. Joining
/// onto an IDR instead settles it, because an IDR restarts the counting. On
/// one disc that took a set of twelve cuts from five pictures out of order to
/// two, the two being joins with no IDR within reach.
///
/// **What it costs is exactness.** The stretch between the two entry points
/// stops being copied and starts being re-encoded, and measured against a
/// decode of the same pictures with their own references, a copy of them is
/// bit-exact where a re-encode of them is 51 dB. That is a good re-encode and
/// it is still not the recording. A program whose whole purpose is to copy
/// what it can does not pay that by default -- and the picture the copy makes
/// is *right*, checked against the recording, so the pictures after a
/// recovery point are not mispredicted the way the seam suggests. All that is
/// wrong is the order one of them comes out in.
///
/// So this is off unless the caller asks, and then only while the reach is
/// short: see [`PlanOptions::clean_join`]. Where the next clean entry point is
/// further off than that -- on one disc of the four measured, the whole title
/// held a single IDR and the median wait was thirteen minutes -- the join is
/// left as it was.
///
/// Only a range whose head is already being re-encoded is moved. A range the
/// caller placed exactly on an entry point is a range the caller meant, and
/// re-encoding where none was asked for would be a surprise.
pub(crate) fn clean_the_join(src: &crate::Source, plan: &mut RangePlan, opts: &PlanOptions) {
    let Some(budget) = opts.clean_join.filter(|b| *b > 0.0) else {
        return;
    };
    if plan.segments.len() < 2
        || plan.segments[0].kind != SegmentKind::Reencode
        || plan.segments[1].kind != SegmentKind::Copy
    {
        return;
    }
    let was = plan.segments[1].start;
    let Some(clean) = clean_start(src, was, was + budget) else {
        return;
    };
    if clean <= was + 1e-6 {
        return;
    }
    // A body shortened past what a copy is worth is a body that should have
    // been re-encoded whole, and that is [`plan_range`]'s decision, not this
    // one's. Left alone rather than second-guessed.
    let fd = src.video.frame_duration();
    let min_copy = opts.min_copy.unwrap_or_else(|| (2.0 * fd).max(0.5));
    if plan.segments[1].end - clean < min_copy {
        return;
    }
    let fps = if src.video.frame_rate > 0.0 {
        src.video.frame_rate
    } else {
        30.0
    };
    let frames = |a: f64, b: f64| frames_in(&src.video, &src.points, fps, a, b, src.duration);
    plan.segments[0].end = clean;
    plan.segments[0].frames = frames(plan.segments[0].start, clean);
    plan.segments[1].start = clean;
    plan.segments[1].frames = frames(clean, plan.segments[1].end);
}

/// The first entry point in `from..=until` that a copy can be joined onto
/// cleanly, if there is one.
///
/// Reads the recording, one entry point at a time and no more of it than the
/// pictures at those points: the byte each of them begins at is in the index,
/// so this is a handful of seeks rather than a pass. See
/// [`crate::bitstream::starts_a_sequence`] for what makes a start clean.
pub fn clean_start(src: &crate::Source, from: f64, until: f64) -> Option<f64> {
    let eps = src.video.frame_duration() / 2.0;
    let wanted: Vec<f64> = src
        .points
        .iter()
        .filter(|p| p.time >= from - eps && p.time <= until + eps)
        .map(|p| p.time)
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let mut ictx = crate::input::demux(&src.input.url).ok()?;
    let idx = src.video.stream_index;
    // Only the pictures are read, and the seeks below are aimed by them.
    // See [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[idx]);
    let tb = src.video.time_base;
    for want in wanted {
        // By the byte where the index holds one, and by the clock where it
        // does not -- an MP4 or a Matroska file. Asked with `?`, as this used
        // to be, the first entry point without a byte ended the search, and
        // `--clean-joins` did nothing at all on those containers.
        if crate::index::seek_to_entry(&mut ictx, src, want).is_none() {
            let target = ((want + src.start_time) * ffmpeg_next::ffi::AV_TIME_BASE as f64) as i64;
            if ictx.seek(target, ..target).is_err() {
                continue;
            }
        }
        let mut clean = false;
        for (stream, packet) in ictx.read_packets() {
            if stream.index() != idx {
                continue;
            }
            let Some(pts) = packet.pts() else { continue };
            let t = pts as f64 * tb - src.start_time;
            // Past the picture asked about, which means the seek landed late
            // and this entry point cannot be judged. Judged as unclean, so
            // that a misread never moves a boundary.
            if t > want + eps {
                break;
            }
            if (t - want).abs() <= eps && packet.is_key() {
                clean = crate::bitstream::starts_a_sequence(
                    packet.data().unwrap_or(&[]),
                    &src.video.codec,
                    src.video.framing,
                );
                break;
            }
        }
        if clean {
            return Some(want);
        }
    }
    None
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
            shape: Default::default(),
            pulldown: false,
            variable_rate: false,
            base_rate: 0.0,
            field_order: 2,
            bit_rate: None,
            vc1: None,
            field_shape: None,
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
                    measured: true,
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
        assert!(
            plan.segments.iter().all(|s| s.frames > 0),
            "{:?}",
            plan.segments
        );
        assert_eq!(plan.segments.last().unwrap().kind, SegmentKind::Copy);
        // And the range is said to end where its pictures do.
        assert_eq!(plan.t_out, plan.segments.last().unwrap().end);
    }

    /// A range in front of the first entry point has nothing to write afresh
    /// either, and says so with no segment rather than one of no length.
    #[test]
    fn a_range_before_the_pictures_is_not_written_afresh() {
        let video = video();
        let fd = video.frame_duration();
        let mut points = points(40, fd);
        for p in &mut points {
            p.time += 0.5;
            p.lead_start += 0.5;
        }
        assert!(reencode_range(&points, 0.0, 0.2).segments.is_empty());
        assert_eq!(reencode_range(&points, 0.0, 2.0).segments.len(), 1);
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

    /// Entry points as Matroska stores them: the true time rounded to the
    /// millisecond, with the phase the remuxed broadcast had (0.011).
    fn stored_points(n: usize, fd: f64) -> Vec<AccessPoint> {
        let ms = |t: f64| (t * 1000.0).round() / 1000.0;
        points(n, fd)
            .into_iter()
            .map(|mut p| {
                p.time = ms(p.time + 0.011);
                p.lead_start = ms(p.lead_start + 0.011);
                p
            })
            .collect()
    }

    /// A bound a fraction of a millisecond past a picture's stored time --
    /// the editor's one-frame-after-the-picture bound on a Matroska file --
    /// is that picture's, at both ends of a range, and the plan counts it so.
    #[test]
    fn a_bound_on_a_coarse_clock_is_the_pictures() {
        let mut video = video();
        video.time_base = 1.0 / 1000.0;
        let fd = video.frame_duration();
        let points = stored_points(40, fd);
        // Pictures sit at 0.011 + n*fd, stored rounded to the millisecond.
        let stored = |n: usize| ((0.011 + n as f64 * fd) * 1000.0).round() / 1000.0;
        // 46 is a picture whose stored time is below its true one; the bound
        // the editor makes for it is the picture before plus a frame.
        let bound = stored(45) + fd;
        assert!(bound > stored(46));
        let moved = on_the_pictures(&video, 300.0, &points, &[(bound, 3.0), (1.0, bound)]);
        let (a, b) = (moved[0].0, moved[1].1);
        // Ahead of the picture by at least a tick, behind the one before by
        // most of a frame: every comparison after this has a clear answer.
        for t in [a, b] {
            assert!(t < stored(46) - 0.0009 && t > stored(45) + fd / 2.0, "{t}");
        }
        // Counted from the picture it means, and to it at the other end.
        let first = plan_range(&video, 300.0, &points, a, 3.0, &PlanOptions::default());
        let last = plan_range(&video, 300.0, &points, 1.0, b, &PlanOptions::default());
        let count = |p: &RangePlan| p.segments.iter().map(|s| s.frames).sum::<usize>();
        // The first picture at or after a time that is not one.
        let from = |t: f64| ((t - 0.011) / fd).ceil() as usize;
        assert_eq!(count(&first), from(3.0) - 46);
        assert_eq!(count(&last), 46 - from(1.0));
        // The picture's own stored time is the same bound.
        let same = on_the_pictures(&video, 300.0, &points, &[(stored(46), 3.0)]);
        assert_eq!(same[0].0, a);
        // A bound between two pictures, and one at or past the end, stay.
        // (A quarter of a frame: MPEG-2's grid is of fields, see above.)
        let between = stored(46) + fd / 4.0;
        assert_eq!(on_the_pictures(&video, 300.0, &points, &[(between, 300.0)]), [(between, 300.0)]);
    }

    /// A tail of one picture -- an entry point's leading picture, the range
    /// ending on it -- is counted as one on a coarse clock too, and kept.
    #[test]
    fn a_one_picture_tail_on_a_coarse_clock_is_kept() {
        let mut video = video();
        video.time_base = 1.0 / 1000.0;
        let fd = video.frame_duration();
        let points = stored_points(40, fd);
        for k in 2..39 {
            // The editor's bound for the leading picture: one frame after it.
            let t_out = points[k].lead_start + fd;
            let ranges = on_the_pictures(&video, 300.0, &points, &[(0.0, t_out)]);
            let plan = plan_range(&video, 300.0, &points, ranges[0].0, ranges[0].1, &PlanOptions::default());
            let tail = plan.segments.last().unwrap();
            assert_eq!(tail.kind, SegmentKind::Reencode, "point {k}: {:?}", plan.segments);
            assert_eq!(tail.frames, 1, "point {k}: {tail:?}");
            let count: usize = plan.segments.iter().map(|s| s.frames).sum();
            assert_eq!(count, 15 * k, "point {k}: {:?}", plan.segments);
        }
    }

    /// After a picture held for three fields the next ones sit half a frame
    /// off the frame grid; a bound on one of them is that picture's too.
    #[test]
    fn a_bound_half_a_frame_off_the_grid_is_the_pictures() {
        let mut video = video();
        video.time_base = 1.0 / 1000.0;
        let fd = video.frame_duration();
        let points = stored_points(40, fd);
        let ms = |t: f64| (t * 1000.0).round() / 1000.0;
        // A picture at 0.011 + 45.5 frames, and the editor's bound after the
        // picture before it, a frame earlier: past the stored time.
        let picture = ms(0.011 + 45.5 * fd);
        let bound = ms(0.011 + 44.5 * fd) + fd;
        let moved = on_the_pictures(&video, 300.0, &points, &[(1.0, bound)]);
        assert!(moved[0].1 < picture - 0.0009 && moved[0].1 > picture - fd / 2.0, "{moved:?}");
        // Not where the codec has no fields to repeat.
        video.codec = "h264".into();
        video.field_order = 1;
        let kept = on_the_pictures(&video, 300.0, &points, &[(1.0, bound)]);
        assert_eq!(kept[0].1, bound);
    }

    /// A clock fine enough to tell a bound from a picture leaves every bound
    /// as it was asked, which keeps every such cut as it was.
    #[test]
    fn a_fine_clock_moves_no_bound() {
        let video = video();
        let fd = video.frame_duration();
        let points = stored_points(40, fd);
        let ranges = [(0.011 + 46.0 * fd + 0.0004, 3.0), (0.5, 0.011 + 10.0 * fd)];
        assert_eq!(on_the_pictures(&video, 300.0, &points, &ranges), ranges);
        let mut varies = video.clone();
        varies.time_base = 1.0 / 1000.0;
        varies.variable_rate = true;
        assert_eq!(on_the_pictures(&varies, 300.0, &points, &ranges), ranges);
    }

    /// A kept range that runs across a seam is planned as two, because a copy
    /// cannot be carried across one. See [`at_the_seams`].
    #[test]
    fn a_range_is_cut_at_the_seams() {
        fn seam(at: u64, time: f64) -> Seam {
            Seam {
                at,
                time,
                ends: time,
            }
        }

        let video = video();
        // An entry point a picture after each seam, as a recorder's stretch
        // opens on leading pictures stamped ahead of its first I.
        let pt = |time: f64| AccessPoint {
            time,
            lead_start: time,
            lead_indices: Vec::new(),
            droppable: true,
            pos: -1,
            measured: true,
        };
        let points = [pt(0.0), pt(10.026), pt(25.026)];
        let at_the_seams = |r: &[(f64, f64)], j: &[Seam]| at_the_seams(r, j, &points, &video);
        let joins = [seam(0, 10.0), seam(0, 25.0)];
        assert_eq!(
            at_the_seams(&[(0.0, 40.0)], &joins),
            vec![(0.0, 10.0), (10.0, 25.0), (25.0, 40.0)]
        );
        // A seam outside the range changes nothing.
        assert_eq!(at_the_seams(&[(12.0, 20.0)], &joins), vec![(12.0, 20.0)]);
        // Nor does one that falls where the range already begins or ends.
        assert_eq!(at_the_seams(&[(10.0, 25.0)], &joins), vec![(10.0, 25.0)]);
        // A recording with no seams is planned exactly as it was.
        assert_eq!(at_the_seams(&[(0.0, 40.0)], &[]), vec![(0.0, 40.0)]);
        // And several ranges are each cut where they need it.
        assert_eq!(
            at_the_seams(&[(5.0, 15.0), (30.0, 35.0)], &joins),
            vec![(5.0, 10.0), (10.0, 15.0), (30.0, 35.0)]
        );
        // An OUT a picture past a seam ends the range at the seam: what lies
        // behind it is the next stretch's undecodable leading pictures.
        assert_eq!(at_the_seams(&[(5.0, 10.026)], &joins), vec![(5.0, 10.0)]);
        // An IN a picture or two before a seam keeps those pictures, and the
        // rest begins at the seam.
        assert_eq!(
            at_the_seams(&[(9.95, 15.0)], &joins),
            vec![(9.95, 10.0), (10.0, 15.0)]
        );
        // A range shorter than a sliver on no seam is still the person's.
        assert_eq!(at_the_seams(&[(12.0, 12.05)], &joins), vec![(12.0, 12.05)]);
        // An OUT on the second picture after a seam keeps the entry point's
        // picture: the piece behind the seam is planned, and begins on it.
        assert_eq!(
            at_the_seams(&[(5.0, 10.06)], &joins),
            vec![(5.0, 10.0), (10.0, 10.06)]
        );
    }

    /// A stretch whose sequence table claims more time than it holds
    /// pictures: the range before the seam stops at the last picture, and the
    /// one after it still begins where the next stretch does. What lies
    /// between is a stretch of clock the recording has nothing at.
    /// See [`crate::index::mend_stretches`].
    #[test]
    fn a_stretch_that_ends_early_ends_the_range_early() {
        let video = video();
        let points = [AccessPoint {
            time: 25.0,
            lead_start: 25.0,
            lead_indices: Vec::new(),
            droppable: true,
            pos: -1,
            measured: true,
        }];
        let at_the_seams = |r: &[(f64, f64)], j: &[Seam]| at_the_seams(r, j, &points, &video);
        let joins = [Seam {
            at: 0,
            time: 25.0,
            ends: 22.0,
        }];
        assert_eq!(
            at_the_seams(&[(0.0, 40.0)], &joins),
            vec![(0.0, 22.0), (25.0, 40.0)]
        );
        // Never past the seam, whatever a mend came back with.
        let joins = [Seam {
            at: 0,
            time: 25.0,
            ends: 30.0,
        }];
        assert_eq!(
            at_the_seams(&[(0.0, 40.0)], &joins),
            vec![(0.0, 25.0), (25.0, 40.0)]
        );
        // Pulled back past the start of the piece it is cutting, there is
        // nothing to keep in front of the seam and no range is made for it.
        let joins = [Seam {
            at: 0,
            time: 25.0,
            ends: 5.0,
        }];
        assert_eq!(at_the_seams(&[(20.0, 40.0)], &joins), vec![(25.0, 40.0)]);
    }

    /// And the far side of a seam begins at the first entry point, because
    /// the pictures in front of it reference a recording that is not here.
    /// See [`past_the_seam`].
    #[test]
    fn a_range_at_a_seam_begins_at_the_first_entry_point() {
        let video = video();
        let fd = video.frame_duration();
        let points = points(40, fd);
        // A seam a little in front of the fourth entry point, which is where
        // one lands: a stretch is placed by a reading of where it starts that
        // is made wide on purpose.
        let seam = points[4].time - 0.3;
        let joins = [seam];
        assert_eq!(
            past_the_seam(&[(0.0, seam), (seam, 20.0)], &joins, &points, &video),
            vec![(0.0, seam), (points[4].time, 20.0)]
        );
        // A range that ends before the stretch's first entry point keeps
        // nothing, and plans to no segment rather than to pictures that are
        // not there.
        let short = past_the_seam(&[(seam, seam + 0.2)], &joins, &points, &video);
        assert_eq!(short, vec![(seam + 0.2, seam + 0.2)]);
        let (a, b) = short[0];
        assert!(plan_range(&video, 300.0, &points, a, b, &PlanOptions::default())
            .segments
            .is_empty());
        // So does one that begins a few frames past the seam, still in
        // front of that entry point: nothing there can be written either.
        assert_eq!(
            past_the_seam(&[(seam + 0.1, 20.0)], &joins, &points, &video),
            vec![(points[4].time, 20.0)]
        );
        // ...but not one that begins past the entry point.
        assert_eq!(
            past_the_seam(&[(points[4].time + 0.2, 20.0)], &joins, &points, &video),
            vec![(points[4].time + 0.2, 20.0)]
        );
        // A range that begins anywhere else is left where the caller put it,
        // seam or no seam.
        assert_eq!(
            past_the_seam(&[(1.0, 20.0)], &joins, &points, &video),
            vec![(1.0, 20.0)]
        );
        // A recording with no seams is planned exactly as it was.
        assert_eq!(
            past_the_seam(&[(1.0, 20.0)], &[], &points, &video),
            vec![(1.0, 20.0)]
        );
        // A seam already on an entry point moves nothing.
        let on = [points[4].time];
        assert_eq!(
            past_the_seam(&[(points[4].time, 20.0)], &on, &points, &video),
            vec![(points[4].time, 20.0)]
        );
    }
}
