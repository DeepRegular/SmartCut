"""Whether a cut's captions are the recording's own, in order and in place.

A caption statement is whole inside one packet with one presentation time, so
there is nothing to splice and nothing that can straddle a boundary: every
statement inside a kept range should arrive in the output unchanged, in the
order it was sent, moved by exactly the amount that range moved. Which makes
the check exact rather than approximate -- the payloads are compared byte for
byte, and the offset within a range has to be one number and not a spread.

    captions.py SOURCE CUT START-END [START-END...]

The ranges are the keep-ranges in source time, as the plan reports them.
Prints `key=value` lines, and says `ok=1` only if everything lines up.
"""
import subprocess
import sys


def packets(path, stream):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", stream, "-show_packets",
         "-show_data_hash", "adler32", "-of", "compact=p=0:nk=0", path],
        capture_output=True, text=True).stdout
    rows = []
    for line in out.splitlines():
        d = dict(kv.split("=", 1) for kv in line.split("|") if "=" in kv)
        if d.get("pts_time") in (None, "N/A"):
            continue
        rows.append((float(d["pts_time"]), d.get("data_hash", ""), d.get("size", "")))
    return rows


def start_time(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=start_time",
         "-of", "csv=p=0", path], capture_output=True, text=True).stdout
    return float(out.strip().split(",")[0] or 0.0)


src_path, cut_path = sys.argv[1], sys.argv[2]
ranges = []
for arg in sys.argv[3:]:
    a, b = arg.split("-", 1) if arg.count("-") == 1 else arg.rsplit("-", 1)
    ranges.append((float(a), float(b)))

base = start_time(src_path)
src = [(t - base, h, s) for t, h, s in packets(src_path, "s")]
cut = packets(cut_path, "s")
print(f"source={len(src)}")
print(f"cut={len(cut)}")
if not src:
    print("ok=0")
    print("why=the recording carries no captions")
    sys.exit(0)

# What should have come out: every statement inside a kept range, in order.
groups = [[(t, h, s) for t, h, s in src if a <= t < b] for a, b in ranges]
want = [row for g in groups for row in g]
print(f"expected={len(want)}")
if len(want) != len(cut):
    print("ok=0")
    print(f"why=expected {len(want)} statements, got {len(cut)}")
    sys.exit(0)

bad = [i for i, (w, c) in enumerate(zip(want, cut)) if w[1] != c[1] or w[2] != c[2]]
if bad:
    print("ok=0")
    print(f"why={len(bad)} statement(s) came out different, first at {bad[0]}")
    sys.exit(0)

# One offset per range, and it has to be one number: a spread inside a range
# would mean the statements were placed individually rather than moved with
# the pictures. A microsecond of it is the 90 kHz grid the output keeps time
# on and nothing else.
worst = 0.0
at = 0
for n, g in enumerate(groups):
    seg = cut[at:at + len(g)]
    at += len(g)
    if not g:
        continue
    offs = [c[0] - w[0] for w, c in zip(g, seg)]
    spread = max(offs) - min(offs)
    worst = max(worst, spread)
    print(f"range.{n}={len(g)}@{min(offs):+.4f}s spread={spread * 1e6:.0f}us")
print(f"worst_spread_us={worst * 1e6:.0f}")
print("ok=1" if worst < 1e-3 else "ok=0")
