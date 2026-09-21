"""Does a cut keep every picture, and every gap between them?

The gaps rather than the positions, because a gap is the one measurement a
constant offset cannot flatter: a range's first picture may sit anywhere
inside a frame of where it was asked for, and everything else is measured
from that. What must not change is how long each picture stays before the
next one arrives.

Written for the recordings whose pictures come closer together than the rate
their container declares -- a 23.976 programme with a burst of 59.94 in it.
There two pictures used to land on the same place on the output timeline and
the second was dropped, so the count is tested as strictly as the spacing.

Prints `ok <n> pictures, every gap within <x> ms` or `bad <what went wrong>`.
"""
import subprocess
import sys

# The containers here state their times in milliseconds, so two of them
# disagreeing by a millisecond either way is the format, not the cut.
SLACK = 0.003


def picture_times(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v",
         "-show_entries", "packet=pts_time", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout.split()
    return sorted(float(x.rstrip(",")) for x in out if x.rstrip(","))


source, made, a, b = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
want = [t for t in picture_times(source) if a - 1e-9 <= t < b]
got = picture_times(made)

if len(got) != len(want):
    print(f"bad {len(got)} pictures, the recording has {len(want)} in that range")
    raise SystemExit(0)
if len(got) < 2:
    print(f"bad only {len(got)} picture(s) to measure")
    raise SystemExit(0)

off = [
    abs((want[i + 1] - want[i]) - (got[i + 1] - got[i]))
    for i in range(len(want) - 1)
]
wrong = sum(1 for d in off if d > SLACK)
if wrong:
    print(f"bad {wrong} of {len(off)} gaps wrong, worst {max(off) * 1000:.1f} ms")
    raise SystemExit(0)

print(f"ok {len(got)} pictures, every gap within {max(off) * 1000:.2f} ms")
