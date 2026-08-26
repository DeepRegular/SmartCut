from __future__ import annotations

import argparse
import sys

from .planner import plan as build_plan
from .probe import access_points, probe, resolve_leading_policy
from .renderer import RenderOptions, pick_video_encoder, render
from .verify import verify


def parse_time(s: str) -> float:
    s = s.strip()
    if not s:
        raise ValueError("empty timestamp")
    parts = s.split(":")
    if len(parts) > 3:
        raise ValueError(f"bad timestamp {s!r}")
    total = 0.0
    for p in parts:
        total = total * 60.0 + float(p)
    return total


def parse_range(s: str) -> tuple[float, float]:
    for sep in ("-", ".."):
        if sep in s.replace("::", ""):
            head, _, tail = s.partition(sep)
            if head and tail:
                return parse_time(head), parse_time(tail)
    raise ValueError(f"bad range {s!r} (expected START-END)")


def complement(cuts: list[tuple[float, float]], duration: float) -> list[tuple[float, float]]:
    keeps, pos = [], 0.0
    for a, b in sorted(cuts):
        if a > pos:
            keeps.append((pos, min(a, duration)))
        pos = max(pos, b)
    if pos < duration:
        keeps.append((pos, duration))
    return [(a, b) for a, b in keeps if b - a > 1e-6]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="smartcut",
        description="Cut video re-encoding only the partial GOPs at the cut points.")
    ap.add_argument("input")
    ap.add_argument("-o", "--output")
    ap.add_argument("--keep", action="append", default=[], metavar="START-END",
                    help="range to keep (repeatable); times are S, M:S or H:M:S")
    ap.add_argument("--cut", action="append", default=[], metavar="START-END",
                    help="range to remove (repeatable); mutually exclusive with --keep")
    ap.add_argument("--analyze", action="store_true",
                    help="print the cut plan and exit")
    ap.add_argument("--video-encoder")
    ap.add_argument("--bitrate-scale", type=float, default=1.15)
    ap.add_argument("--audio-mode", choices=["copy", "reencode"], default="copy",
                    help="copy: keep source audio frames (<=~24ms boundary snap); "
                         "reencode: sample-exact")
    ap.add_argument("--no-open-gop", action="store_true",
                    help="refuse open-GOP access points instead of trimming "
                         "their leading pictures")
    ap.add_argument("--verify", action="store_true",
                    help="after rendering, decode both sides and report how many "
                         "frames came through bit-identical")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args(argv)

    if args.keep and args.cut:
        ap.error("use --keep or --cut, not both")
    if not args.analyze and not args.output:
        ap.error("-o/--output is required unless --analyze is given")

    info = probe(args.input)
    v = info.video
    print(f"input : {args.input}")
    # H.264/HEVC report the level times ten; other codecs use their own scale
    level = (f"@L{v.level / 10:.1f}" if v.level and v.codec_name in ("h264", "hevc")
             else f"@L{v.level}" if v.level else "")
    print(f"        {v.codec_name} {v.profile or '-'}{level} "
          f"{v.width}x{v.height} {float(v.avg_frame_rate):.3f}fps "
          f"{v.pix_fmt} {v.field_order} sar={v.sar} "
          f"{(v.bit_rate or 0)//1000}kbps  dur={info.duration:.3f}s")
    if info.audio:
        a = info.audio
        print(f"        audio {a.codec_name} {a.sample_rate}Hz {a.channels}ch "
              f"{(a.bit_rate or 0)//1000}kbps")

    points = access_points(args.input)
    if not points:
        print("no random access points found", file=sys.stderr)
        return 2
    times = [p.time for p in points]
    gaps = [b - a for a, b in zip(times, times[1:])]
    avg_gap = sum(gaps) / len(gaps) if gaps else 0.0
    n_open = sum(1 for p in points if p.open_gop)
    if n_open:
        ok = resolve_leading_policy(args.input, points, v.codec_name)
    print(f"        {len(points)} access points, mean GOP {avg_gap:.3f}s"
          + (f", {n_open} open (leading pictures"
             + (", droppable)" if ok else ", referenced -- cannot start a copy there)")
             if n_open else ", all closed"))

    try:
        if args.cut:
            ranges = complement([parse_range(s) for s in args.cut], info.duration)
        elif args.keep:
            ranges = [parse_range(s) for s in args.keep]
        else:
            ranges = [(0.0, info.duration)]
    except ValueError as e:
        ap.error(str(e))

    plans = build_plan(info, points, ranges, allow_open_gop=not args.no_open_gop)

    total_copy = sum(p.copied for p in plans)
    total_enc = sum(p.reencoded for p in plans)
    total = total_copy + total_enc
    print(f"\nplan  : {len(plans)} range(s), {total:.3f}s output")
    for p in plans:
        print(f"  keep {p.t_in:.3f} -> {p.t_out:.3f}")
        for s in p.segments:
            print(f"    {s.kind:>8}  {s.start:8.3f} -> {s.end:8.3f}  ({s.duration:6.3f}s)")
    if total > 0:
        print(f"        copied {total_copy:.3f}s ({100*total_copy/total:.1f}%), "
              f"re-encoded {total_enc:.3f}s ({100*total_enc/total:.1f}%)")

    if args.analyze:
        return 0

    opts = RenderOptions(video_encoder=args.video_encoder,
                         bitrate_scale=args.bitrate_scale,
                         audio_mode=args.audio_mode,
                         dry_run=args.dry_run, verbose=args.verbose)
    if total_enc > 0:
        print(f"\nencoder: {pick_video_encoder(info, opts)}")
    print("render:")
    render(info, plans, args.output, opts)
    print(f"\nwrote {args.output}")

    if args.verify and not args.dry_run:
        print("\nverify:")
        result = verify(args.input, args.output, ranges)
        print(result)
        if not (result.frame_count_ok and result.aligned):
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
