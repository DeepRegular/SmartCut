"""Is every picture of a cut where the recording had it?

Lines the output's pictures up against the source's and measures the gap
between each pair, relative to the first. A cut that put its pictures on an
even grid, or that lost a held picture's time at a seam, reads here as a
displacement of a whole frame or more; the container's own millisecond
timestamps are worth a couple of milliseconds either way, so that is the
slack.

Prints one line: `ok <n> pictures, all where the recording had them`, or
`bad <what went wrong>`.
"""
import subprocess
import sys

SLACK = 0.002


def picture_times(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v",
         "-show_entries", "packet=pts_time", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout.split()
    return [float(x.rstrip(",")) for x in out if x.rstrip(",")]


source, made, a, b = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
src, out = picture_times(source), picture_times(made)
want = [t for t in src if a - 1e-9 <= t < b]

# A picture held across the range's start belongs to the range and is shown
# from the start of it, so the cut has one more picture than the source has
# inside the bounds. It is there when nothing of the source's own arrives
# within a frame of the start.
before = [t for t in src if t < a]
fd = 1.0 / 30.0
carried = 1 if before and (not want or want[0] - a >= fd) else 0

if len(out) != len(want) + carried:
    print(f"bad {len(out)} pictures, the recording has {len(want) + carried} there")
    raise SystemExit(0)
if not out:
    print("bad no pictures at all")
    raise SystemExit(0)

moved = [
    i for i in range(len(want))
    if abs((want[i] - want[0]) - (out[i + carried] - out[carried])) > SLACK
]
if moved:
    worst = max(
        abs((want[i] - want[0]) - (out[i + carried] - out[carried])) for i in moved
    )
    print(f"bad {len(moved)} of {len(want)} pictures moved, worst {worst * 1000:.0f} ms")
    raise SystemExit(0)

held = " (one held across the start)" if carried else ""
print(f"ok {len(out)} pictures, all where the recording had them{held}")
