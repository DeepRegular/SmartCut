"""What came out of a downmix: how many channels, and what is in each of them.

A downmix is a claim about where sound went, so this checks the sound. Each
channel of the fixture carries a tone of its own, which makes the folding
audible in the spectrum: 5.1 into stereo puts the centre in both channels, the
left surround into the left and nothing of the LFE anywhere, and a channel
that received nothing it should have -- or something it should not -- says so
as a missing or an extra peak.

    python3 tests/downmix.py FILE CHANNELS "TONES;TONES;..." [--config N]

One `TONES` per channel, in order, each a comma-separated list of the
frequencies that channel should carry and nothing else; an empty one is a
channel that should be silent. `--config` additionally requires the ADTS
headers to announce that many channels, which is what a transport stream's
decoder actually reads.
"""
import subprocess, sys

import numpy as np

# How much of the strongest peak a bin has to reach to count as a tone, and
# how far from a named frequency a peak may sit and still be it.
FLOOR = 0.05
SLACK = 8.0


def track(path):
    """What the file says its audio track is: (channels, sample rate).

    Read by name: ffprobe's csv output follows the order the fields sit in a
    stream, not the order they were asked for, so the two numbers arrive the
    other way round as often as not.
    """
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0",
         "-show_entries", "stream=channels,sample_rate", "-of", "default=nw=1", path],
        capture_output=True, text=True).stdout
    fields = dict(
        line.split("=", 1) for line in out.strip().split("\n") if "=" in line
    )
    return int(fields.get("channels", 0)), int(fields.get("sample_rate", 48000))


def channels(path, n):
    """Every channel of the file, decoded, one row each."""
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-map", "0:a:0", "-f", "f32le", "-"],
        capture_output=True,
    ).stdout
    x = np.frombuffer(out, dtype="<f4").astype(np.float64)
    return x[: len(x) // n * n].reshape(-1, n).T


# Below this a channel is empty, whatever its own noise floor looks like once
# a spectrum is normalised against it.
SILENT = 1e-3


def tones(x, sr):
    """The frequencies actually present in a channel, strongest first."""
    if len(x) < 4096:
        return []
    spectrum = np.abs(np.fft.rfft(x * np.hanning(len(x))))
    freqs = np.fft.rfftfreq(len(x), 1 / sr)
    peak = spectrum.max()
    if peak <= 0:
        return []
    # One peak per tone: a bin either side of a maximum is the same tone.
    found = []
    for k in np.argsort(spectrum)[::-1]:
        if spectrum[k] < peak * FLOOR:
            break
        if all(abs(freqs[k] - f) > SLACK for f in found):
            found.append(float(freqs[k]))
    return found


def adts_config(path):
    """What the frames themselves say their channel count is."""
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-map", "0:a:0", "-c", "copy", "-f", "adts", "-"],
        capture_output=True).stdout
    for i in range(min(len(raw) - 7, 1 << 16)):
        if raw[i] == 0xFF and raw[i + 1] & 0xF0 == 0xF0:
            return ((raw[i + 2] & 0x01) << 2) | (raw[i + 3] >> 6)
    return None


path, want_channels, spec = sys.argv[1], int(sys.argv[2]), sys.argv[3]
want_config = None
if "--config" in sys.argv:
    want_config = int(sys.argv[sys.argv.index("--config") + 1])
wanted = [
    [float(f) for f in part.split(",") if f] for part in spec.split(";")
]

ch_count, sr = track(path)
bad = []
if ch_count != want_channels:
    bad.append(f"the track has {ch_count} channel(s), want {want_channels}")
got = channels(path, ch_count or 1)
if got.shape[1] < sr:
    bad.append(f"only {got.shape[1]} sample(s) came out")
print(f"  {'channel':>8} {'rms':>8}  tones")
for i, (ch, want) in enumerate(zip(got, wanted)):
    # A window well inside the file, clear of the seams and of the fade the
    # encoder's priming leaves at either end.
    seg = ch[sr : sr * 3]
    rms = float(np.sqrt((seg ** 2).mean())) if len(seg) else 0.0
    # A spectrum is normalised against its own strongest bin, so an empty
    # channel's noise would come back looking like tones. It is empty.
    found = sorted(tones(seg, sr)) if rms >= SILENT else []
    print(f"  {i:>8} {rms:8.4f}  {[round(f) for f in found]}")
    missing = [f for f in want if not any(abs(f - g) <= SLACK for g in found)]
    extra = [g for g in found if not any(abs(f - g) <= SLACK for f in want)]
    if missing:
        bad.append(f"channel {i} is missing {[round(f) for f in missing]} Hz")
    if extra:
        bad.append(f"channel {i} carries {[round(g) for g in extra]} Hz it should not")

if want_config is not None:
    got_config = adts_config(path)
    print(f"  ADTS channel_config: {got_config}")
    if got_config != want_config:
        bad.append(f"the frames announce channel_config {got_config}, want {want_config}")

if bad:
    print("  BAD  " + "; ".join(bad))
    sys.exit(1)
sys.exit(0)
