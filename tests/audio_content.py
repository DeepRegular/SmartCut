"""Is the audio in a cut actually the source's audio, in the right place?

Decodes a window from the output and a wider one from the source around where
the cut should have put it, finds the alignment by cross-correlation, and
reports both how well they match and how far off the alignment is. A copy
that dropped, muted, duplicated or misplaced the audio cannot pass this.
"""
import os, subprocess, sys
import numpy as np

SR = 48000
SEARCH = float(os.environ.get("SEARCH", "5.0"))  # seconds either side


def pcm(path, start, dur):
    """Decode a window, seeking in two stages.

    A one-shot `-ss` on an MPEG-TS lands by byte position and can miss by
    hundreds of milliseconds -- the same approximation the cutter itself has
    to work around. Jumping short of the target and then discarding decoded
    audio up to it is exact.
    """
    pre = max(0.0, start - 5.0)
    rest = start - pre
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-ss", f"{pre:.3f}", "-i", path,
         "-ss", f"{rest:.3f}", "-t", f"{dur:.3f}",
         "-map", "0:a:0", "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"],
        capture_output=True).stdout
    return np.frombuffer(out, dtype="<f4").astype(np.float64)


def rms_db(x):
    return 20 * np.log10(max(np.sqrt(np.mean(x ** 2)), 1e-12))


def align(a, b):
    """Where in `b` does `a` sit, and how well? Returns (correlation, offset)."""
    n, m = len(a), len(b)
    a = a - a.mean()
    N = 1 << (n + m - 1).bit_length()
    cc = np.fft.irfft(np.conj(np.fft.rfft(a, N)) * np.fft.rfft(b - b.mean(), N), N)
    k = int(np.argmax(cc[: m - n + 1]))
    seg = b[k : k + n]
    seg = seg - seg.mean()
    den = np.linalg.norm(a) * np.linalg.norm(seg)
    return (float(np.dot(a, seg) / den) if den else 0.0), k


src, out = sys.argv[1], sys.argv[2]
pairs = [tuple(float(x) for x in p.split(":")) for p in sys.argv[3:]]
dur = 6.0
print(f"  {'出力位置':>10} {'素材位置':>10}  {'出力RMS':>9} {'素材RMS':>9}  {'相関':>6} {'ずれ':>9}")
worst, lags = 1.0, []
for s_t, o_t in pairs:
    a = pcm(out, o_t, dur)
    b = pcm(src, max(0.0, s_t - SEARCH), dur + 2 * SEARCH)
    if len(a) < SR or len(b) < len(a) + SR:
        print(f"  {o_t:10.2f} {s_t:10.2f}   （音声が取れません len={len(a)}/{len(b)}）")
        worst = -1
        continue
    c, k = align(a, b)
    lag = k / SR - min(s_t, SEARCH)
    print(f"  {o_t:10.2f} {s_t:10.2f}  {rms_db(a):8.1f}dB {rms_db(b):8.1f}dB"
          f"  {c:6.3f} {1000*lag:+8.1f}ms")
    worst = min(worst, c)
    lags.append(lag)
# A constant offset is the measuring apparatus -- decoder priming, and the
# container's own start time. What matters is whether it *varies*: a step at
# a seam is the cutter putting the audio in the wrong place.
spread = max(lags) - min(lags) if lags else 0.0
min_c = float(os.environ.get("MIN_CORR", "0.99"))
max_s = float(os.environ.get("MAX_SPREAD", "0.04"))
ok = worst >= min_c and spread <= max_s
print(f"  最小相関 {worst:.3f} (>= {min_c})   継ぎ目でのずれ幅 {1000*spread:.1f}ms"
      f" (<= {1000*max_s:.0f}ms)  ->  {'OK' if ok else 'CHECK'}")
sys.exit(0 if ok else 1)
