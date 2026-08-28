# State of the Rust implementation (`rust/`)

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](rust-core.ja.md)

The port is in progress. **The "first frame is 13 ms early" limitation is
gone** — see below.

| Part | State |
|---|---|
| Access point index and leading-picture analysis | Done (output identical to Python) |
| Leading-picture reference test | Done, and **more accurate than Python** |
| Planner | Done (agrees with Python on 11 cases) |
| Cutting (copy path) | Done |
| Cutting (re-encode path) | Done |
| Resolving mixed SPS/PPS | Done (`avc3` plus parameter set re-insertion) |
| Audio (copy) | Done (sync verified by measurement) |
| Audio (re-encode) | Not started |

On video, the 13 cases in `tests/run_rust_tests.sh` reach the **same lossless
ratio** as `tests/run_tests.sh` (Python):

```
h264 single range      lossless 180/222   first=0.00000 step=0.033333 jitter=0
h264 cut middle        lossless 540/540   first=0.00000 step=0.033333 jitter=0
hevc                   lossless 300/342   first=0.00000 step=0.033333 jitter=0
ntsc 29.97fps          lossless 300/342   first=0.00000 step=0.033367 jitter=0
mpeg2 ts open-GOP      lossless 328/342   first=0.00000 step=0.033367 jitter=0
```

And on top of that **the timestamps are perfect in every case** (the Python
version starts 13 ms early).

## Fixing the timestamp problem

The Python version had no option but to hand a raw ES to ffmpeg, which puts the
first frame 13 ms early (`irregular=[(0, 0.046667)]`). The Rust version
**assigns PTS/DTS directly, in integer ticks**, from each picture's display
index. The output time base is `1/fps_numerator`, so one frame is exactly
`fps_denominator` ticks and no rounding happens at all:

```
h264 keyframe-exact   lossless 180/180   first=0.00000 step=0.033333 jitter=0
mpeg2 ts open-GOP     lossless 283/283   first=0.00000 step=0.033367 jitter=0
```

Verified by `tests/run_rust_tests.sh`. Every case starts at exactly 0.000 s with
zero jitter.

## Resolving mixed SPS/PPS

An MP4 `avcC` can hold only one set of parameter sets, but the SPS of the
re-encoded part is always different from the original stream. On top of that MP4
stores NALs length-prefixed while the encoder emits Annex-B. Leave either alone
and the video collapses with `sps_id 32 out of range` / `Invalid NAL unit size`.

- Set the sample entry to **`avc3` / `hev1`** so in-band parameter sets are
  allowed
- Reframe the encoder output from Annex-B to length-prefixed
- **Re-insert the original SPS/PPS ahead of every keyframe in the copied part** —
  if the re-encoded part's SPS were left active, the copied part would be decoded
  with the wrong SPS, so the original parameter sets are restored at each splice

## Audio boundaries (`--audio-mode`)

Audio has no GOP structure, so it is handled **per kept interval** rather than
per video segment. Each interval's audio is **anchored to the output time where
that interval's video starts**, so drift across intervals cannot happen by
construction… or so it seemed.

There was a trap. **An MP4 audio track expresses time through sample durations
(stts); it does not keep a timestamp per packet.** All it keeps is the track's
start offset, and the samples are laid out back to back after that. So dropping
a single audio frame at an interval boundary **shifts every following interval
permanently, and the error accumulates interval by interval**. Measured on
material with a click every 0.5 s, the second interval came out a uniform 11 ms
early.

The fix is to **track the actual end position of the audio written so far and
carry that error into the choice of the next interval's first frame**. Since the
interval opens on the frame nearest the boundary, the error is **bounded by half
a frame (about 10.7 ms for AAC at 48 kHz)** and does not accumulate. A test with
three intervals and every boundary deliberately off the frame grid showed a worst
case of +7.67 ms.

Two routes to trimming in the container were tried, and both are dead ends:

- `AV_PKT_DATA_SKIP_SAMPLES` (the side data used for gapless playback) — **the
  MP4 muxer ignores it**. The samples marked for skipping stayed in and the audio
  ran late by exactly the amount requested.
- `initial_padding` on the output stream — **the muxer does not write it to the
  file** (probe the output and `initial_padding=0`). Trying it made the
  single-interval error worse, from 0.00 ms to +9.33 ms.

That leaves re-encoding, which is implemented as `--audio-mode reencode`: each
interval's audio is cut sample-exactly and fed continuously into a single
encoder.

| Mode | Boundary error | Audio |
|---|---|---|
| `copy` (default) | ±half a frame (10.7 ms for AAC at 48 kHz), no accumulation | Lossless |
| `reencode` | **-0.02 ms** (one sample is 0.021 ms, so effectively zero) | Re-encoded |

Run `tests/run_audio_tests.sh` with `SMARTCUT_AUDIO=reencode` and all 5 cases
land at -0.02 ms. Video stays lossless in both modes (1798/1798, 99.1 % on real
material).

**The default is `copy`.** Smart rendering is a tool for avoiding re-encoding,
and 10.7 ms is far below the perceptual threshold for lip sync. The GUI exposes
the switch as a "sample-accurate audio" checkbox.

A trap hit while implementing this: audio frames straddling a segment boundary
were **submitted twice, once from each of the two adjacent segments**, and the
error accumulated one AAC frame (21.3 ms) at a time. Fixed by adopting the same
exclusive ownership rule as the copy path.

`tests/run_audio_tests.sh` — measures A/V sync sample by sample on material with
a 2 ms impulse every 0.5 s. Steady tones cannot reveal a sync error.

## Whether the audio of real material is genuinely correct (`tests/run_audio_content_tests.sh`)

Impulse material can only answer "how does it behave on material we made". "When
a real broadcast recording is cut, is the sound that comes out really the sound
from that place?" is a different question. So: **decode 6 seconds of the output
and find, by cross-correlation, where in the recording it matches**. Dropped,
muted or misplaced audio all fail this.

What matters is **not the absolute offset but whether the offset changes from
interval to interval**. Decoder priming and the container start time apply
equally everywhere, so a constant offset belongs to the measurement.
**If it changes, that is a bad seam.**

Measured on two intervals of real material (Nihonkai TV, 30 minutes):

| Audio | Container | Correlation | Offset spread across seams |
|---|---|---|---|
| copy | MP4 | **1.000** | 31.0 ms |
| copy | TS | **1.000** | 31.0 ms |
| sample-accurate | MP4 | **1.000** | **0.0 ms** |
| sample-accurate | TS | **1.000** | **0.0 ms** |

**The sound is unquestionably there** — correlation 1.000 across every interval,
with RMS within 1 dB of the source. On top of that, **leaving audio on copy
shifts the sound by up to about 30 ms per seam, and it stacks up with the number
of seams**. AAC can only be cut on a 21.3 ms frame grid, so the boundary rounding
shows through directly. The same 31.0 ms appears in MP4 and in TS, so this is a
property of the cutting side, not of the muxing.

Re-rendering sample-accurately brings it to 0.0 ms (`--audio-mode reencode`).
It is nonetheless **kept out of the window**, for two reasons, both from
measurement:

- **On actual commercial cuts, copy showed no drift.** The 31.0 ms above comes
  from two intervals deliberately placed on boundaries that are not lossless
  points. Commercial detection snaps its boundaries to access points, and across
  a five-interval export of real material all six seams were **a constant
  +5.7 ms** — unchanged from interval to interval. The offset is per-seam
  rounding, not something that stacks, and placing the boundaries well removes
  almost all of it.
- **Re-rendering changes the shape of the ADTS and breaks downstream tools.** As
  described below, Japanese broadcast AAC is `ff f8` (MPEG-2, with CRC) while
  ffmpeg can only write `ff f1` (MPEG-4, no CRC). Indexing software written for
  ARIB fails to read it.

The judgement is that what is lost (downstream tools cannot read the file)
outweighs what is gained (an improvement of roughly 0 ms). It is still in the
engine and the CLI, so `--audio-mode reencode` is there if it is needed.

## Where the reference test became more accurate than Python

Python needed to launch ffmpeg a second time to read the bitstream, so it
**sampled one place in the file and applied the result to the whole thing**. In
Rust `nal_ref_idc` can be read while the packets are already being scanned, so
the test is **evaluated exactly, per access point**. No extra pass, no extra cost.
