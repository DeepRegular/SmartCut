"""Where a variable-rate recording holds a picture, for `run_vfr_tests.sh`.

Prints four figures on one line: how many pictures the recording has, how
long its longest hold is, where to find a hold worth aiming a range at, and
how long that one lasts. The last two are what the tests need -- a seam has
to land on a held picture before any of the arithmetic the tests are about
can be wrong -- and they are asked of the file rather than assumed, because
where the pictures land is the encoder's business.
"""
import subprocess
import sys


def picture_times(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v",
         "-show_entries", "packet=pts_time", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout.split()
    return [float(x.rstrip(",")) for x in out if x.rstrip(",")]


times = picture_times(sys.argv[1])
gaps = [(times[i + 1] - times[i], times[i]) for i in range(len(times) - 1)]
longest = max(g for g, _ in gaps) if gaps else 0.0
# The first hold at least half as long as the longest, far enough into the
# recording that a range can be put on either side of it.
at, held = next(((a, g) for g, a in gaps if g > longest / 2 and 20.0 < a < 40.0), (0.0, 0.0))
print(len(times), f"{longest:.3f}", f"{at:.3f}", f"{held:.3f}")
