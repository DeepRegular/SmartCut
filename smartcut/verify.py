"""Prove a cut is correct by comparing decoded frames against the source.

A smart-rendered cut makes two promises: the output holds exactly the frames
that were asked for, and the frames outside the partial GOPs are the *same
bits* the source had.  Both are checkable -- decode each side to per-frame
hashes and line them up.
"""
from __future__ import annotations

import subprocess
from dataclasses import dataclass


@dataclass
class VerifyResult:
    expected: int
    produced: int
    identical: int
    offset: int

    @property
    def frame_count_ok(self) -> bool:
        return self.expected == self.produced

    @property
    def aligned(self) -> bool:
        # With nothing stream-copied there is no bit-identical frame to line
        # up against, so offset carries no information.
        return self.identical == 0 or self.offset == 0

    def __str__(self) -> str:
        pct = 100.0 * self.identical / self.produced if self.produced else 0.0
        lines = [
            f"frames      : {self.produced} produced, {self.expected} expected"
            f" {'OK' if self.frame_count_ok else 'MISMATCH'}",
            (f"alignment   : n/a (nothing stream-copied)" if self.identical == 0
             else f"alignment   : offset {self.offset:+d}"
                  f" {'OK' if self.offset == 0 else 'MISMATCH'}"),
            f"bit-identical: {self.identical}/{self.produced} frames ({pct:.1f}%)"
            f" -- these were stream-copied, not re-encoded",
        ]
        return "\n".join("  " + l for l in lines)


def _frame_entries(args: list[str]) -> list[tuple[float, str]]:
    """(presentation time, frame hash) for every decoded frame.

    Times come from the stream itself rather than from a frame counter.
    Real recordings drop frames -- a broadcast TS routinely does -- so
    `index / fps` is not the time a frame is shown at, and slicing a
    reference by index would compare the wrong pictures.
    """
    cmd = (["ffmpeg", "-hide_banner", "-nostdin", "-v", "error", "-y"] + args
           # passthrough, or the default CFR sync drops a frame wherever the
           # synthesised timeline jitters
           + ["-map", "0:v:0", "-fps_mode", "passthrough",
              "-f", "framehash", "-hash", "md5", "-"])
    out = subprocess.run(cmd, capture_output=True, text=True).stdout

    tb = 1.0
    entries: list[tuple[float, str]] = []
    for line in out.splitlines():
        if line.startswith("#tb"):
            _, _, rate = line.partition(":")
            num, _, den = rate.strip().partition("/")
            try:
                tb = int(num) / int(den)
            except ValueError:
                pass
            continue
        if line.startswith("#") or not line.strip():
            continue
        parts = [f.strip() for f in line.split(",")]
        if len(parts) < 6:
            continue
        try:
            entries.append((int(parts[2]) * tb, parts[-1]))
        except ValueError:
            continue
    return entries


def _frame_hashes(args: list[str]) -> list[str]:
    return [h for _, h in _frame_entries(args)]


def _start_time(path: str, args: list[str]) -> float:
    out = subprocess.run(["ffprobe", "-v", "error"] + args + ["-of", "csv=p=0", path],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        try:
            return float(line.strip().rstrip(","))
        except ValueError:
            continue
    return 0.0


def _decode_origin_shift(path: str) -> float:
    """How far the decoder's clock sits ahead of the cutter's.

    A cut is expressed against the presentation timeline -- the format's
    start_time, which is where a player shows 00:00. ffmpeg rebases decoded
    frames against the *video stream's* start_time instead. On a broadcast
    recording those differ, because the audio stream begins first; ignoring
    the gap compares the wrong pictures by exactly that much.
    """
    fmt = _start_time(path, ["-show_entries", "format=start_time"])
    vid = _start_time(path, ["-select_streams", "v:0", "-show_entries", "stream=start_time"])
    return vid - fmt


def verify(src: str, out: str, ranges: list[tuple[float, float]],
           search: int = 5) -> VerifyResult:
    # Decode the source in full and pick frames by presentation time. Seeking
    # to build the reference would be faster but is not trustworthy: on an
    # open-GOP source ffmpeg's accurate seek discards the pictures it cannot
    # decode and starts the output up to a GOP late, which would make a
    # correct cut look wrong.
    truth = _frame_entries(["-i", src])
    shift = _decode_origin_shift(src)
    # Only enough slack to absorb timestamp rounding. A frame-sized margin
    # would over-reach: real streams put their frames at an arbitrary phase,
    # not at exact multiples of the frame duration.
    eps = 1e-4
    ref: list[str] = []
    for t_in, t_out in ranges:
        ref += [h for t, h in truth if t_in - eps <= t + shift < t_out - eps]
    got = _frame_hashes(["-i", out])

    best = (0, -1)
    for off in range(-search, search + 1):
        m = sum(1 for i, h in enumerate(got) if 0 <= i + off < len(ref) and ref[i + off] == h)
        if m > best[1]:
            best = (off, m)
    return VerifyResult(expected=len(ref), produced=len(got),
                        identical=best[1], offset=best[0])
