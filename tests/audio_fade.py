"""Measure the fade a cut puts either side of a seam.

The fixture is a tone that never changes, so what comes out of the output is
the shape of the fade itself: level, down to nothing at the join, and back to
level. Three things are asked of it.

**It reaches nothing.** The quietest point has to be far below the tone. A
fade that arrives at a tenth of the level is not a fade, it is a dip.

**Nothing survives at the join.** The frame the seam falls on is the one to
watch: a patch that is prepared and then refused leaves the recording's own
frame standing there -- full level, in the one place the fade was asked to
take the level away. It lasts one frame, so nothing measured over a tenth of
a second would notice it, and it is exactly the click the fade was asked for.

**It comes back.** A fade that takes the level away and leaves it away has
cut the programme rather than smoothed the join.

Run as: audio_fade.py <cut.ts> <seconds> [<seam seconds>]
"""
import math, subprocess, sys

SR = 8000  # the shape is an envelope; it does not need the recording's rate


def mono(path):
    """The output as mono floats, whatever it was written as."""
    raw = subprocess.run(
        ["ffmpeg", "-v", "quiet", "-i", path, "-vn", "-ac", "1",
         "-ar", str(SR), "-f", "f32le", "-"],
        capture_output=True).stdout
    import array
    a = array.array("f")
    a.frombytes(raw[: len(raw) // 4 * 4])
    return a


def rms(a, t0, t1):
    lo, hi = max(0, int(t0 * SR)), min(len(a), int(t1 * SR))
    if hi <= lo:
        return 0.0
    return math.sqrt(sum(v * v for v in a[lo:hi]) / (hi - lo))


def db(v, ref):
    return 20 * math.log10(v / ref) if v > 0 and ref > 0 else -99.0


def main():
    path, fade = sys.argv[1], float(sys.argv[2])
    seam = float(sys.argv[3]) if len(sys.argv) > 3 else None
    a = mono(path)
    level = rms(a, 1.0, 3.0)
    if level <= 0:
        return f"BAD|the fixture is silent where it should be a tone"
    # Where the join is. Given, or wherever the output is quietest -- which on
    # a tone is the bottom of the fade and nothing else.
    if seam is None:
        step = 0.02
        n = int(len(a) / SR / step)
        seam = min(range(n), key=lambda i: rms(a, i * step, (i + 1) * step)) * step
    bottom = db(rms(a, seam - 0.02, seam + 0.02), level)
    # A window wide enough to hold a frame of any codec here, either side.
    burst = db(max(rms(a, seam + k * 0.01, seam + (k + 1) * 0.01)
                   for k in range(-6, 6)), level)
    back = db(rms(a, seam + fade + 0.2, seam + fade + 0.4), level)
    away = db(rms(a, seam - fade - 0.4, seam - fade - 0.2), level)
    if bottom > -40.0:
        return f"BAD|the fade only reached {bottom:.1f} dB at the join"
    if burst > -12.0:
        return f"BAD|{burst:.1f} dB survives at the join, which is a frame the fade missed"
    if back < -1.0 or away < -1.0:
        return f"BAD|the level did not come back: {away:.1f} dB in, {back:.1f} dB out"
    return f"OK|bottom {bottom:.0f} dB, loudest at the join {burst:.0f} dB"


print(main())
