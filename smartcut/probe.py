"""ffprobe wrappers: stream parameters, keyframe index, open-GOP detection."""
from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass, field
from fractions import Fraction


class ProbeError(RuntimeError):
    pass


def _run(args: list[str]) -> str:
    proc = subprocess.run(args, capture_output=True, text=True)
    if proc.returncode != 0:
        raise ProbeError(f"{args[0]} failed: {proc.stderr.strip()[:800]}")
    return proc.stdout


def _frac(s: str | None, default: str = "0/1") -> Fraction:
    if not s or s in ("0/0", "N/A"):
        s = default
    try:
        return Fraction(s.replace(":", "/"))
    except (ValueError, ZeroDivisionError):
        return Fraction(default)


@dataclass
class VideoInfo:
    index: int
    codec_name: str
    profile: str | None
    level: int | None
    width: int
    height: int
    pix_fmt: str
    sar: Fraction
    time_base: Fraction
    avg_frame_rate: Fraction
    bit_rate: int | None
    field_order: str
    color_range: str | None
    color_primaries: str | None
    color_transfer: str | None
    color_space: str | None
    nb_frames: int | None
    has_b_frames: int = 0

    @property
    def frame_duration(self) -> float:
        return 1.0 / float(self.avg_frame_rate) if self.avg_frame_rate else 1.0 / 30.0

    @property
    def interlaced(self) -> bool:
        return self.field_order in ("tt", "bb", "tb", "bt")

    @property
    def top_field_first(self) -> bool:
        return self.field_order in ("tt", "tb")


@dataclass
class AudioInfo:
    index: int
    codec_name: str
    profile: str | None
    sample_rate: int
    channels: int
    channel_layout: str | None
    bit_rate: int | None


@dataclass
class MediaInfo:
    path: str
    format_name: str
    duration: float
    start_time: float
    bit_rate: int | None
    video: VideoInfo
    audio: AudioInfo | None = None
    keyframes: list[float] = field(default_factory=list)


def probe(path: str) -> MediaInfo:
    raw = json.loads(_run([
        "ffprobe", "-v", "error", "-print_format", "json",
        "-show_format", "-show_streams", path,
    ]))
    fmt = raw.get("format", {})
    streams = raw.get("streams", [])

    v = next((s for s in streams if s.get("codec_type") == "video"
              and s.get("disposition", {}).get("attached_pic", 0) == 0), None)
    if v is None:
        raise ProbeError(f"no video stream in {path}")

    video = VideoInfo(
        index=int(v["index"]),
        codec_name=v.get("codec_name", ""),
        profile=v.get("profile"),
        level=int(v["level"]) if str(v.get("level", "")).lstrip("-").isdigit() and int(v["level"]) > 0 else None,
        width=int(v["width"]),
        height=int(v["height"]),
        pix_fmt=v.get("pix_fmt", "yuv420p"),
        sar=_frac(v.get("sample_aspect_ratio"), "1/1"),
        time_base=_frac(v.get("time_base"), "1/90000"),
        avg_frame_rate=_frac(v.get("avg_frame_rate") or v.get("r_frame_rate"), "30/1"),
        bit_rate=int(v["bit_rate"]) if str(v.get("bit_rate", "")).isdigit() else None,
        field_order=v.get("field_order", "progressive"),
        color_range=v.get("color_range"),
        color_primaries=v.get("color_primaries"),
        color_transfer=v.get("color_transfer"),
        color_space=v.get("color_space"),
        nb_frames=int(v["nb_frames"]) if str(v.get("nb_frames", "")).isdigit() else None,
    )

    video.has_b_frames = int(v.get("has_b_frames", 0) or 0)

    a = next((s for s in streams if s.get("codec_type") == "audio"), None)
    audio = None
    if a is not None:
        audio = AudioInfo(
            index=int(a["index"]),
            codec_name=a.get("codec_name", ""),
            profile=a.get("profile"),
            sample_rate=int(a.get("sample_rate", 48000)),
            channels=int(a.get("channels", 2)),
            channel_layout=a.get("channel_layout"),
            bit_rate=int(a["bit_rate"]) if str(a.get("bit_rate", "")).isdigit() else None,
        )

    return MediaInfo(
        path=path,
        format_name=fmt.get("format_name", ""),
        duration=float(fmt.get("duration", 0.0) or 0.0),
        start_time=float(fmt.get("start_time", 0.0) or 0.0),
        bit_rate=int(fmt["bit_rate"]) if str(fmt.get("bit_rate", "")).isdigit() else None,
        video=video,
        audio=audio,
    )


@dataclass
class AccessPoint:
    """A random access point and the leading pictures that hang off it."""
    time: float          # presentation time of the I picture
    lead_start: float    # earliest presentation time among its leading
                         # pictures; == time when the GOP is closed
    lead_indices: tuple[int, ...] = ()   # decode-order offsets, relative to
                                         # the I picture, of those leading
                                         # pictures
    droppable: bool = True               # may those leading pictures be cut
                                         # out, i.e. is none of them a reference
    index: int = 0                       # position in decode order, so a copy
                                         # can be bounded by an exact packet
                                         # count instead of a timestamp

    @property
    def open_gop(self) -> bool:
        return bool(self.lead_indices)


def access_points(path: str, stream_index: int = 0) -> list[AccessPoint]:
    """Index random access points by scanning packets, not decoded frames.

    Packets come out in decode order and need no decoding, which matters
    twice over: `-skip_frame nokey` silently loses entry points on open-GOP
    streams (its decoder cannot output an I picture whose references are
    missing), and decoding a long file just to find its keyframes is slow.

    Decode order is also what exposes leading pictures: a picture that
    follows an I picture in decode order but presents *before* it references
    the previous GOP, so a copy starting at that I picture cannot include it.
    """
    # MPEG-TS timestamps do not start at zero, but -ss counts from the start
    # of the file.  Rebase onto the same origin ffmpeg seeks against.
    origin = 0.0
    try:
        raw = _run(["ffprobe", "-v", "error", "-show_entries", "format=start_time",
                    "-of", "csv=p=0", path]).strip().rstrip(",")
        origin = float(raw)
    except (ProbeError, ValueError):
        pass

    out = _run([
        "ffprobe", "-v", "error", "-select_streams", f"v:{stream_index}",
        "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0", path,
    ])
    packets: list[tuple[float, bool]] = []
    for line in out.splitlines():
        parts = line.strip().rstrip(",").split(",")
        if len(parts) < 2:
            continue
        try:
            pts = float(parts[0]) - origin
        except ValueError:
            continue          # N/A
        packets.append((pts, "K" in parts[1]))

    points: list[AccessPoint] = []
    for i, (pts, key) in enumerate(packets):
        if not key:
            continue
        lead = pts
        lead_idx: list[int] = []
        for j, (nxt_pts, nxt_key) in enumerate(packets[i + 1:], start=1):
            if nxt_key:
                break
            if nxt_pts < pts:
                lead = min(lead, nxt_pts)
                lead_idx.append(j)
        points.append(AccessPoint(time=pts, lead_start=lead,
                                  lead_indices=tuple(lead_idx), index=i))
    points.sort(key=lambda ap: ap.time)
    return points


def leading_droppable(path: str, point: "AccessPoint", codec: str,
                      window: float = 1.5) -> bool:
    """Can this entry point's leading pictures be cut out of a copy?

    Only safe when none of them is a reference picture.  Requires looking at
    the bitstream, so a short Annex-B window is extracted around the access
    point.  (A libav-based implementation reads nal_ref_idc straight off the
    packet during demux and needs no extra pass.)
    """
    if not point.lead_indices:
        return True
    if codec == "mpeg2video":
        return True
    muxer = {"h264": "h264", "hevc": "hevc"}.get(codec)
    if muxer is None:
        return False
    try:
        data = subprocess.run(
            ["ffmpeg", "-hide_banner", "-nostdin", "-v", "error",
             "-ss", f"{point.time:.6f}", "-i", path, "-t", f"{window:.3f}",
             "-map", "v:0", "-c", "copy", "-f", muxer, "-"],
            capture_output=True).stdout
    except OSError:
        return False
    if not data:
        return False
    from .bitstream import access_units, au_is_reference
    aus = access_units(data, codec)
    for idx in point.lead_indices:
        if idx >= len(aus) or au_is_reference(data, aus[idx], codec):
            return False
    return True


def resolve_leading_policy(path: str, points: list[AccessPoint], codec: str) -> bool:
    """Mark whether open access points may be used to start a copy.

    One open access point is sampled and its verdict applied to the rest: an
    encoder does not switch its B-pyramid strategy part-way through a file,
    and probing every GOP would cost a pass per keyframe.
    """
    opens = [p for p in points if p.open_gop]
    if not opens:
        return True
    verdict = leading_droppable(path, opens[len(opens) // 2], codec)
    for p in opens:
        p.droppable = verdict
    return verdict


def keyframe_times(path: str, stream_index: int = 0) -> list[float]:
    return [ap.time for ap in access_points(path, stream_index)]
