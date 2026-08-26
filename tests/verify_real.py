"""Validate a cut of real-world material.

Checks the four things that actually matter on broadcast recordings: the
right frames came out, the copied ones came out untouched, the timeline is
uniform, and interlacing survived the splice.
"""
import os, subprocess, sys
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from smartcut import probe
from smartcut.verify import verify


def probe_csv(path, args):
    return subprocess.run(["ffprobe", "-v", "error"] + args + ["-of", "csv=p=0", path],
                          capture_output=True, text=True).stdout


def main():
    src, out, ranges = sys.argv[1], sys.argv[2], sys.argv[3]
    pairs = [tuple(float(x) for x in r.split("-")) for r in ranges.split(",")]
    info = probe(src)
    fps = float(info.video.avg_frame_rate)

    r = verify(src, out, pairs)
    print(f"  frames        : {r.produced} produced / {r.expected} expected"
          f"  {'OK' if r.frame_count_ok else 'MISMATCH'}")
    print(f"  alignment     : offset {r.offset:+d}  {'OK' if r.aligned else 'MISMATCH'}")
    pct = 100.0 * r.identical / r.produced if r.produced else 0.0
    print(f"  bit-identical : {r.identical}/{r.produced} ({pct:.1f}%)")

    # The output's timing must match the source's, not some ideal of
    # uniformity: 2:3 pulldown legitimately alternates two-field and
    # three-field pictures, and demanding a constant step would call correct
    # output broken.
    # ffprobe's -read_intervals takes absolute timestamps, which on a
    # broadcast TS start in the tens of thousands of seconds. Cut the sample
    # out with a stream copy instead so the comparison looks at the right
    # stretch of the source.
    chunk = "/tmp/verify_real_chunk.ts"
    t0, t1 = pairs[0]
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-ss", f"{t0}", "-t", f"{t1 - t0}",
                    "-i", src, "-map", "0:v:0", "-c", "copy", "-f", "mpegts", chunk],
                   capture_output=True)

    def steps(path, extra=None):
        raw = probe_csv(path, (extra or []) + [
            "-select_streams", "v:0", "-show_entries", "frame=pts_time"])
        v = sorted(float(x.strip().rstrip(",")) for x in raw.splitlines() if x.strip())
        return v, Counter(round(b - a, 4) for a, b in zip(v, v[1:]))

    out_pts, out_steps = steps(out)
    _, src_steps = steps(chunk)
    shared = set(out_steps) | set(src_steps)
    same_shape = all(
        abs(out_steps.get(k, 0) / max(sum(out_steps.values()), 1)
            - src_steps.get(k, 0) / max(sum(src_steps.values()), 1)) < 0.1
        for k in shared)
    print(f"  timeline      : first={out_pts[0]:.5f} steps={dict(out_steps.most_common(3))}"
          f"  {'OK' if same_shape and abs(out_pts[0]) < 1e-9 else 'CHECK'}")

    # Likewise for interlacing: what matters is that the output codes its
    # pictures the same way the source did, not that they are all interlaced.
    def interlace_mix(path, extra=None):
        raw = probe_csv(path, (extra or []) + [
            "-select_streams", "v:0", "-show_entries", "frame=interlaced_frame"])
        c = Counter(l.strip().rstrip(",") for l in raw.splitlines() if l.strip())
        total = sum(c.values()) or 1
        return c["1"] / total

    out_mix = interlace_mix(out)
    src_mix = interlace_mix(chunk)
    print(f"  interlacing   : output {100*out_mix:.0f}% interlaced, source {100*src_mix:.0f}%"
          f"  {'OK' if abs(out_mix - src_mix) < 0.1 else 'CHECK'}")

    if info.audio:
        vd = probe_csv(out, ["-select_streams", "v:0", "-show_entries", "stream=duration"])
        ad = probe_csv(out, ["-select_streams", "a:0", "-show_entries", "stream=duration"])
        try:
            skew = abs(float(vd.strip().rstrip(",")) - float(ad.strip().rstrip(","))) * 1000
            print(f"  A/V skew      : {skew:.1f} ms  {'OK' if skew < 100 else 'CHECK'}")
        except ValueError:
            print("  A/V skew      : could not read durations")


main()
