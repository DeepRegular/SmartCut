"""Turn a keep-range into a list of copy / re-encode segments.

Smart rendering in one picture, for keep = [t_in, t_out):

    ... I ....... I=========================I ....... I ...
          ^t_in   ^k_first                  ^k_term   ^t_out
        |<-head->|<--------- body --------->|<-tail->|
         re-encode      stream copy          re-encode

`head` and `tail` are partial GOPs, so they must be decoded and re-encoded.
Everything between the first and last usable access point is copied
bit-for-bit.

Open GOPs make both ends narrower than they look.  A picture that follows an
I picture in decode order but presents *before* it -- a leading picture --
references the previous GOP.  So:

* the copy may start at I@k_first, but k_first's own leading pictures cannot
  come with it (they belong to the head, which is re-encoded anyway);
* the copy must stop before I@k_term, which means it cannot deliver the
  pictures presenting in [k_term.lead_start, k_term.time) either -- those are
  decoded after I@k_term.  So the body's display coverage ends at
  `k_term.lead_start`, not at `k_term.time`.

For a closed GOP `lead_start == time` and both collapse to the simple case.
"""
from __future__ import annotations

from dataclasses import dataclass

from .probe import AccessPoint, MediaInfo


@dataclass
class Segment:
    kind: str                        # "copy" | "reencode"
    start: float                     # display coverage, inclusive
    end: float                       # display coverage, exclusive
    copy_stop: float | None = None   # presentation time of the terminating
                                     # I picture; bounds the copy in decode
                                     # order, and differs from `end` on open GOPs
    seek_from: float | None = None   # start decoding here, then trim forward:
                                     # seeking straight to a re-encode's first
                                     # frame can land in a GOP that cannot be
                                     # decoded on its own
    frames: int = 0                  # pictures this segment contributes
    packets: int | None = None       # packets to copy (frames plus the
                                     # leading pictures that get dropped);
                                     # None means "to the end of the file"
    drop_leading: tuple[int, ...] = ()   # decode-order indices of leading
                                         # pictures to cut out of the copy

    @property
    def duration(self) -> float:
        return self.end - self.start


@dataclass
class RangePlan:
    t_in: float
    t_out: float
    segments: list[Segment]

    @property
    def copied(self) -> float:
        return sum(s.duration for s in self.segments if s.kind == "copy")

    @property
    def reencoded(self) -> float:
        return sum(s.duration for s in self.segments if s.kind == "reencode")


def _safe_seek(points: list[AccessPoint], target: float, back: int = 2) -> float:
    """An access point far enough before `target` to decode into it cleanly."""
    earlier = [p.time for p in points if p.time <= target + 1e-6]
    if not earlier:
        return 0.0
    return earlier[max(0, len(earlier) - 1 - back)]


def plan_range(info: MediaInfo, points: list[AccessPoint], t_in: float, t_out: float,
               *, allow_open_gop: bool = True, min_copy: float | None = None) -> RangePlan:
    """Build the segment list for a single keep-range."""
    if t_out <= t_in:
        raise ValueError(f"empty range {t_in}..{t_out}")

    # Snap onto the frame grid first.  At fractional rates (30000/1001) a
    # request stated in seconds sits between frames, and head/tail durations
    # then round to a different frame count than the caller expects.
    #
    # The grid used here is multiples of the frame duration.  A stream's
    # frames actually sit at whatever phase its first picture falls on, so a
    # boundary can land up to half a frame off and the range gain or lose one
    # picture at its edge.  See README, "既知の制限".
    fps = float(info.video.avg_frame_rate)
    index = lambda t: round(t * fps)
    t_in = index(t_in) / fps
    t_out = index(t_out) / fps

    fd = info.video.frame_duration
    eps = fd / 2.0
    # A copy shorter than a couple of GOPs buys nothing but a seam.
    if min_copy is None:
        min_copy = max(2.0 * fd, 0.5)

    usable = points if allow_open_gop else [p for p in points if not p.open_gop]
    # Any access point can *end* a copy, but starting one at an open GOP is
    # only possible when its leading pictures can be cut away -- they present
    # before the entry point, so they cannot be shown, yet removing a leading
    # picture that later pictures reference destroys the whole GOP.
    entries = [p for p in usable
               if t_in - eps <= p.time <= t_out + eps
               and (not p.open_gop or p.droppable)]

    def full_reencode() -> RangePlan:
        seg = Segment("reencode", t_in, t_out, seek_from=_safe_seek(points, t_in))
        seg.frames = index(t_out) - index(t_in)
        return RangePlan(t_in, t_out, [seg])

    if not entries:
        return full_reencode()
    k_first = entries[0]

    # Where can the copy stop?  Either just before a later access point, or --
    # when the range runs to the end of the file -- at the file's end.
    stops: list[tuple[float, float | None, int | None]] = [
        (p.lead_start, p.time, p.index) for p in usable
        if p.time > k_first.time + eps and p.lead_start <= t_out + eps
    ]
    if info.duration and t_out >= info.duration - eps:
        stops.append((t_out, None, None))
    if not stops:
        return full_reencode()
    copy_end, copy_stop, stop_index = max(stops, key=lambda s: s[0])

    if copy_end - k_first.time < min_copy:
        return full_reencode()

    segments: list[Segment] = []
    body_start = k_first.time
    if body_start - t_in > eps:
        segments.append(Segment("reencode", t_in, body_start,
                                seek_from=_safe_seek(points, t_in)))
    else:
        body_start = t_in
    segments.append(Segment("copy", body_start, copy_end,
                            copy_stop=copy_stop,
                            drop_leading=k_first.lead_indices,
                            packets=(None if stop_index is None
                                     else stop_index - k_first.index)))
    if t_out - copy_end > eps:
        segments.append(Segment("reencode", copy_end, t_out,
                                seek_from=_safe_seek(points, copy_end)))
    for seg in segments:
        seg.frames = index(seg.end) - index(seg.start)
    return RangePlan(t_in, t_out, segments)


def plan(info: MediaInfo, points: list[AccessPoint],
         ranges: list[tuple[float, float]], **kw) -> list[RangePlan]:
    return [plan_range(info, points, a, b, **kw) for a, b in ranges]
