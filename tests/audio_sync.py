"""Measure A/V alignment from the impulse markers in the click fixture.

Clicks sitting right on a range boundary are excluded: an audio frame cannot
be split, so the frame straddling the boundary is dropped and its click with
it. That is a known, documented limitation -- not a sync error -- and mixing
it into the alignment figure would hide real drift.

The bar is half an audio frame. A range boundary lands wherever the video
frame grid puts it, and the nearest whole audio frame is at most half a frame
away; closing that gap would mean re-encoding. What must *not* happen is
accumulation, so the same bound has to hold on the last range as on the
first.
"""
import os, struct, subprocess, sys

SR = 48000
GUARD = 0.05          # seconds either side of a range edge to ignore
FRAME = 1024 / SR     # one AAC frame
# Copying can only land on whole frames; re-encoding trims to the sample.
TOLERANCE_MS = 1.0 if os.environ.get("SMARTCUT_AUDIO") == "reencode" else FRAME / 2 * 1000 + 1.0


def start_time(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0",
         "-show_entries", "stream=start_time", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout.strip().rstrip(",")
    try:
        return float(out)
    except ValueError:
        return 0.0


def clicks(path):
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-map", "a:0",
         "-f", "s16le", "-ac", "1", "-ar", str(SR), "-"],
        capture_output=True).stdout
    n = len(raw) // 2
    vals = struct.unpack(f"<{n}h", raw[: n * 2])
    base, out, last = start_time(path), [], -10 ** 9
    for i, v in enumerate(vals):
        if abs(v) > 12000 and i - last > SR // 10:
            out.append(base + i / SR)
            last = i
    return out


def duration(path, stream):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", stream,
         "-show_entries", "stream=duration", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout.strip().rstrip(",")
    return float(out) if out else 0.0


src, out = os.environ["SRC"], os.environ["OUT"]
pairs = [tuple(float(x) for x in r.split("-")) for r in os.environ["RANGES"].split(",")]

if not duration(out, "a:0"):
    print("BAD|no audio track in the output")
    raise SystemExit

src_clicks = clicks(src)
expected, base = [], 0.0
for t_in, t_out in pairs:
    for t in src_clicks:
        if t_in + GUARD <= t < t_out - GUARD:
            expected.append(t - t_in + base)
    base += t_out - t_in

got = clicks(out)
matched = [min(((g - e) * 1000 for e in expected), key=abs) for g in got
           if any(abs(g - e) < 0.05 for e in expected)]
missing = len(expected) - len(matched)
worst = max(matched, key=abs) if matched else 0.0
av_skew = abs(duration(out, "a:0") - duration(out, "v:0")) * 1000

ok = missing == 0 and abs(worst) <= TOLERANCE_MS and av_skew < FRAME * 1000 + 1.0
print(f"{'OK' if ok else 'BAD'}|{len(matched)} clicks, worst {worst:+.2f} ms "
      f"(limit {TOLERANCE_MS:.1f}), A/V skew {av_skew:.1f} ms"
      + (f", {missing} MISSING" if missing else ""))
