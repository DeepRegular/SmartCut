"""Execute a cut plan with ffmpeg.

Video pieces are written as raw Annex-B elementary streams and joined by
plain byte concatenation.  That is deliberate:

* An elementary stream carries SPS/PPS (or the MPEG-2 sequence header)
  in-band before every IDR, so a re-encoded piece and a copied piece may
  legally carry *different* parameter sets -- which they always do, because
  no encoder reproduces the source's SPS bit-for-bit.
* There are no container timestamps to splice, so no seam drift.  The final
  mux stamps one uniform timeline over the whole concatenated stream.

The price is that output is CFR at the source's average frame rate.
For MP4 we request the `avc3` / `hev1` sample entry so the in-band
parameter sets survive muxing instead of collapsing into a single `avcC`.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from dataclasses import dataclass, field

from .bitstream import drop_access_units
from .planner import RangePlan, Segment
from .probe import MediaInfo

VIDEO_ENCODERS = {
    "h264": ["libx264", "h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"],
    "hevc": ["libx265", "hevc_nvenc", "hevc_qsv", "hevc_videotoolbox"],
    "mpeg2video": ["mpeg2video"],
    "mpeg4": ["mpeg4"],
}
# codec -> (elementary muxer, file extension)
VIDEO_ES = {
    "h264": ("h264", "h264"),
    "hevc": ("hevc", "hevc"),
    "mpeg2video": ("mpeg2video", "m2v"),
    "mpeg4": ("m4v", "m4v"),
}
AUDIO_ENCODERS = {
    "aac": "aac", "ac3": "ac3", "eac3": "eac3", "mp2": "mp2",
    "mp3": "libmp3lame",
}
AUDIO_ES = {"aac": "adts", "ac3": "ac3", "eac3": "eac3", "mp2": "mp2", "mp3": "mp3"}

H264_PROFILES = {
    "baseline": "baseline", "constrained baseline": "baseline",
    "main": "main", "high": "high", "high 10": "high10",
    "high 4:2:2": "high422", "high 4:4:4 predictive": "high444",
}


class RenderError(RuntimeError):
    pass


@dataclass
class RenderOptions:
    video_encoder: str | None = None       # override auto-detection
    bitrate_scale: float = 1.15            # headroom over the source bitrate
    audio_mode: str = "copy"               # "copy" | "reencode"
    extra_video_args: list[str] = field(default_factory=list)
    audio_encoder: str | None = None
    dry_run: bool = False
    verbose: bool = False


def available_encoders() -> set[str]:
    out = subprocess.run(["ffmpeg", "-hide_banner", "-encoders"],
                         capture_output=True, text=True).stdout
    names = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and len(parts[0]) == 6 and parts[0][0] in "VAS":
            names.add(parts[1])
    return names


def pick_video_encoder(info: MediaInfo, opts: RenderOptions) -> str:
    if opts.video_encoder:
        return opts.video_encoder
    have = available_encoders()
    for cand in VIDEO_ENCODERS.get(info.video.codec_name, []):
        if cand in have:
            return cand
    raise RenderError(f"no encoder available for {info.video.codec_name!r}; "
                      f"pass --video-encoder to choose one")


def _target_bitrate(info: MediaInfo, opts: RenderOptions) -> int:
    v = info.video
    br = v.bit_rate
    if not br and info.bit_rate:
        audio_br = info.audio.bit_rate if info.audio and info.audio.bit_rate else 192_000
        br = max(info.bit_rate - audio_br, 100_000)
    if not br:
        br = int(v.width * v.height * float(v.avg_frame_rate) * 0.08)
    return int(br * opts.bitrate_scale)


def video_encode_args(info: MediaInfo, opts: RenderOptions) -> list[str]:
    """Encoder settings chosen so a re-encoded piece splices onto a copied one."""
    v = info.video
    enc = pick_video_encoder(info, opts)
    br = _target_bitrate(info, opts)
    args = ["-c:v", enc,
            "-b:v", str(br), "-maxrate", str(int(br * 1.6)), "-bufsize", str(int(br * 2.5)),
            "-pix_fmt", v.pix_fmt]

    # Keep the decoder's view of the stream identical across the splice.
    if v.codec_name == "h264" and v.profile:
        p = H264_PROFILES.get(v.profile.lower())
        if p:
            args += ["-profile:v", p]
        if v.level:
            args += ["-level", f"{v.level / 10:.1f}"]
    elif v.codec_name == "hevc" and v.profile:
        args += ["-profile:v", v.profile.lower().replace(" ", "")]

    flags = ["+cgop"]
    if v.interlaced:
        flags += ["+ilme", "+ildct"]
        args += ["-top", "1" if v.top_field_first else "0"]
    args += ["-flags", "".join(flags)]
    if enc in ("mpeg2video", "mpeg4"):
        # these encoders refuse a closed GOP while scene-change detection is
        # live; a partial GOP is short enough that losing it costs nothing
        args += ["-sc_threshold", "1000000000"]

    for opt, val in (("-color_primaries", v.color_primaries),
                     ("-color_trc", v.color_transfer),
                     ("-colorspace", v.color_space),
                     ("-color_range", v.color_range)):
        if val and val not in ("unknown", "reserved"):
            args += [opt, val]

    if v.sar and v.sar != 1:
        args += ["-vf", f"setsar={v.sar.numerator}/{v.sar.denominator}"]

    # Same reordering depth as the source: the elementary stream carries no
    # timestamps, so the final mux derives them from the coding structure.
    if enc in ("libx264", "libx265", "mpeg2video", "mpeg4"):
        args += ["-bf", str(v.has_b_frames)]
    args += ["-r", f"{float(v.avg_frame_rate):.10f}"]
    args += opts.extra_video_args
    return args


def audio_encode_args(info: MediaInfo, opts: RenderOptions) -> list[str]:
    a = info.audio
    if a is None:
        return []
    enc = opts.audio_encoder or AUDIO_ENCODERS.get(a.codec_name, "aac")
    br = a.bit_rate or {1: 96_000, 2: 192_000}.get(a.channels, 96_000 * a.channels)
    return ["-c:a", enc, "-b:a", str(br), "-ar", str(a.sample_rate), "-ac", str(a.channels)]


def _run_ffmpeg(args: list[str], opts: RenderOptions) -> None:
    cmd = ["ffmpeg", "-hide_banner", "-nostdin", "-y",
           "-loglevel", "info" if opts.verbose else "error"] + args
    if opts.verbose or opts.dry_run:
        print("  $ " + " ".join(cmd))
    if opts.dry_run:
        return
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RenderError("ffmpeg failed:\n" + proc.stderr.strip()[-2000:])


# --------------------------------------------------------------------------- video

def _render_video_segment(info: MediaInfo, seg: Segment, out: str,
                          opts: RenderOptions) -> None:
    fd = info.video.frame_duration
    muxer, _ = VIDEO_ES[info.video.codec_name]
    if seg.kind == "copy":
        # Nudge past the keyframe so float rounding cannot seek one GOP early.
        ss = seg.start + fd / 4
        args = ["-ss", f"{ss:.6f}", "-i", info.path]
        if seg.packets is not None:
            # Bound the copy by an exact packet count rather than a duration.
            # A duration would be judged on DTS -- the display timeline shifted
            # back by the reorder depth -- and would also have to absorb the
            # container's start_time, which ffmpeg leaves as a residual offset
            # on the output.  Counting packets sidesteps both.
            args += ["-frames:v", str(seg.packets)]
        args += ["-map", "0:v:0", "-c", "copy"]
    else:
        pre = seg.seek_from if seg.seek_from is not None else seg.start
        pre = min(pre, seg.start)
        args = ["-ss", f"{pre:.6f}", "-i", info.path]
        if seg.start - pre > 1e-6:
            # output-side seek: decode from `pre`, then discard up to the
            # first frame we actually want (same residual offset as above)
            args += ["-ss", f"{seg.start - pre + info.start_time:.6f}"]
        args += ["-frames:v", str(seg.frames), "-map", "0:v:0"]
        args += video_encode_args(info, opts)
    args += ["-f", muxer, out]
    _run_ffmpeg(args, opts)

    if seg.drop_leading and not opts.dry_run:
        # ffmpeg cannot filter a stream copy by presentation time, so the
        # open-GOP entry point's leading pictures came along.  Cut them out.
        n = drop_access_units(out, list(seg.drop_leading), info.video.codec_name)
        if opts.verbose:
            print(f"    dropped {n} leading picture(s)")


# --------------------------------------------------------------------------- audio

def _render_audio(info: MediaInfo, plans: list[RangePlan], out: str,
                  opts: RenderOptions) -> None:
    """One audio stream for the whole output.

    Audio is not GOP-structured, so it is cut per keep-range rather than per
    video segment.  "copy" keeps the source frames (boundaries snap to the
    nearest audio frame, <=~24 ms); "reencode" is sample-exact.
    """
    a = info.audio
    assert a is not None
    if opts.audio_mode == "copy" and a.codec_name in AUDIO_ES:
        muxer = AUDIO_ES[a.codec_name]
        parts = []
        for i, p in enumerate(plans):
            piece = f"{out}.{i:03d}"
            _run_ffmpeg(["-ss", f"{p.t_in:.6f}", "-i", info.path,
                         "-t", f"{p.t_out - p.t_in:.6f}", "-map", "0:a:0",
                         "-c", "copy", "-f", muxer, piece], opts)
            parts.append(piece)
        if not opts.dry_run:
            with open(out, "wb") as dst:
                for piece in parts:
                    with open(piece, "rb") as src:
                        shutil.copyfileobj(src, dst)
                    os.remove(piece)
        return

    # Sample-exact path: trim + concat inside one filtergraph.
    enc = opts.audio_encoder or AUDIO_ENCODERS.get(a.codec_name, "aac")
    muxer = AUDIO_ES.get(a.codec_name if enc != "aac" else "aac", "adts")
    chains, labels = [], []
    for i, p in enumerate(plans):
        chains.append(f"[0:a:0]atrim=start={p.t_in:.6f}:end={p.t_out:.6f},"
                      f"asetpts=PTS-STARTPTS[a{i}]")
        labels.append(f"[a{i}]")
    graph = ";".join(chains)
    if len(plans) > 1:
        graph += ";" + "".join(labels) + f"concat=n={len(plans)}:v=0:a=1[aout]"
        label = "[aout]"
    else:
        label = labels[0]
    _run_ffmpeg(["-i", info.path, "-filter_complex", graph, "-map", label]
                + audio_encode_args(info, opts) + ["-f", muxer, out], opts)


# --------------------------------------------------------------------------- mux

def _mux(info: MediaInfo, video_es: str, audio_es: str | None, output: str,
         opts: RenderOptions, tmpdir: str) -> None:
    """Elementary stream -> container.

    Always goes through MP4 first.  An elementary stream carries no
    timestamps, so ffmpeg synthesises them from -r plus the H.264/HEVC
    parser's picture-order counts -- and the first `has_b_frames` packets
    emerge with no PTS at all, because the parser cannot reorder until its
    window fills.  Only the MP4 muxer tolerates that (given make_zero);
    Matroska and MPEG-TS reject the packets outright.  So MP4 is built first
    and anything else is remuxed from it.
    """
    v = info.video
    ext = os.path.splitext(output)[1].lower()
    direct = ext in (".mp4", ".m4v", ".mov")
    stage = output if direct else os.path.join(tmpdir, "staged.mp4")

    args = ["-r", f"{float(v.avg_frame_rate):.10f}", "-i", video_es]
    if audio_es:
        args += ["-i", audio_es]
    args += ["-map", "0:v:0", "-c", "copy"]
    if audio_es:
        args += ["-map", "1:a:0"]
    # in-band parameter sets: the copied and re-encoded pieces carry different
    # SPS/PPS, which only `avc3`/`hev1` sample entries allow
    tag = {"h264": "avc3", "hevc": "hev1"}.get(v.codec_name)
    if tag:
        args += ["-tag:v", tag]
    args += ["-avoid_negative_ts", "make_zero", "-video_track_timescale", "90000"]
    if direct:
        args += ["-movflags", "+faststart"]
    if v.sar and v.sar != 1:
        args += ["-aspect", f"{v.width * v.sar.numerator}:{v.height * v.sar.denominator}"]
    args += [stage]
    _run_ffmpeg(args, opts)

    if not direct:
        _run_ffmpeg(["-i", stage, "-map", "0", "-c", "copy", output], opts)


def render(info: MediaInfo, plans: list[RangePlan], output: str,
           opts: RenderOptions | None = None, workdir: str | None = None) -> str:
    opts = opts or RenderOptions()
    if info.video.codec_name not in VIDEO_ES:
        raise RenderError(
            f"{info.video.codec_name!r} has no elementary-stream form; "
            f"smart rendering supports {', '.join(sorted(VIDEO_ES))}")

    tmp = workdir or tempfile.mkdtemp(prefix="smartcut-")
    owns_tmp = workdir is None
    _, vext = VIDEO_ES[info.video.codec_name]
    try:
        pieces = []
        n = 0
        for p in plans:
            for seg in p.segments:
                piece = os.path.join(tmp, f"seg{n:04d}_{seg.kind}.{vext}")
                print(f"  [{n}] {seg.kind:>8}  {seg.start:8.3f} -> {seg.end:8.3f}"
                      f"  ({seg.duration:6.3f}s)")
                _render_video_segment(info, seg, piece, opts)
                pieces.append(piece)
                n += 1

        video_es = os.path.join(tmp, f"all.{vext}")
        if not opts.dry_run:
            with open(video_es, "wb") as dst:
                for piece in pieces:
                    with open(piece, "rb") as src:
                        shutil.copyfileobj(src, dst)

        audio_es = None
        if info.audio is not None:
            audio_es = os.path.join(tmp, "all.audio")
            print(f"  [a] {opts.audio_mode:>8}  {len(plans)} range(s)")
            _render_audio(info, plans, audio_es, opts)

        _mux(info, video_es, audio_es, output, opts, tmp)
        return output
    finally:
        if owns_tmp and not opts.verbose:
            shutil.rmtree(tmp, ignore_errors=True)
