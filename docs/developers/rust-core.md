# The Rust core (`rust/`)

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](rust-core.ja.md)

The Rust port is what ships. The Python implementation is kept as the test oracle.
The "first frame is 13 ms early" limitation is gone in this implementation — see
below.

Everything about audio has its own page: [audio](../technical/audio.md).

## State of the port

| Part | State |
|---|---|
| Access point index and leading-picture analysis | Done, output identical to Python |
| Leading-picture reference test | Done, and **more accurate than Python** |
| Planner | Done, agrees with Python on 11 cases |
| Cutting (copy path) | Done |
| Cutting (re-encode path) | Done |
| Resolving mixed SPS/PPS | Done — `avc3` plus parameter set re-insertion |
| Audio (copy) | Done, sync verified by measurement |
| Audio (smart rendering) | Done, only the boundary frames re-encoded |
| Audio (re-encode) | Done, sample-accurate, and MPEG-2 AAC on the way out |

On video, the 13 cases in `tests/run_rust_tests.sh` reach the same lossless ratio as
`tests/run_tests.sh` (Python):

```
h264 single range      lossless 180/222   first=0.00000 step=0.033333 jitter=0
h264 cut middle        lossless 540/540   first=0.00000 step=0.033333 jitter=0
hevc                   lossless 300/342   first=0.00000 step=0.033333 jitter=0
ntsc 29.97fps          lossless 300/342   first=0.00000 step=0.033367 jitter=0
mpeg2 ts open-GOP      lossless 328/342   first=0.00000 step=0.033367 jitter=0
```

And on top of that the timestamps are exact in every case, where the Python version
starts 13 ms early.

## Fixing the timestamp problem

The Python version had no option but to hand a raw elementary stream to ffmpeg, which
puts the first frame 13 ms early (`irregular=[(0, 0.046667)]`).

The Rust version **assigns PTS and DTS directly, in integer ticks**, from each
picture's display index. The output time base is `1/fps_numerator`, so one frame is
exactly `fps_denominator` ticks and no rounding happens at all:

```
h264 keyframe-exact   lossless 180/180   first=0.00000 step=0.033333 jitter=0
mpeg2 ts open-GOP     lossless 283/283   first=0.00000 step=0.033367 jitter=0
```

Verified by `tests/run_rust_tests.sh`. Every case starts at exactly 0.000 s with zero
jitter.

## Resolving mixed SPS/PPS

An MP4 `avcC` can hold only one set of parameter sets, but the SPS of the re-encoded
part is always different from the original stream's. On top of that, MP4 stores NALs
length-prefixed while the encoder emits Annex-B. Leave either alone and the video
collapses with `sps_id 32 out of range` or `Invalid NAL unit size`.

Three things are needed:

- Set the sample entry to **`avc3` / `hev1`**, so in-band parameter sets are allowed.
- Reframe the encoder output from Annex-B to length-prefixed.
- **Re-insert the original SPS/PPS ahead of every keyframe in the copied part.** If the
  re-encoded part's SPS were left active, the copied part would be decoded with the
  wrong SPS, so the original parameter sets are restored at each splice.

The background to this is [pitfall 1](../technical/algorithm.md#1-the-parameter-sets-spspps-do-not-match).

## Where the reference test became more accurate than Python

Python needed to launch ffmpeg a second time to read the bitstream, so it **sampled one
place in the file and applied the result to the whole thing**.

In Rust, `nal_ref_idc` can be read while the packets are already being scanned, so the
test is **evaluated exactly, per access point**. No extra pass, and no extra cost.

## Audio

The audio side of the engine is large enough to have its own page. It covers:

| | |
|---|---|
| [The three modes](../technical/audio.md#the-three-modes) | `copy`, `smart` and `reencode`, and what each costs |
| [Cutting per interval](../technical/audio.md#cutting-per-interval-and-the-drift-that-nearly-happened) | Why an MP4 `stts` track accumulates drift, and how it is bounded |
| [Smart rendering applied to audio](../technical/audio.md#smart-rendering-applied-to-audio) | Re-encoding only the straddling frames, the guard frame, and the silence test |
| [Writing MPEG-2 AAC](../technical/audio.md#writing-mpeg-2-aac---aac) | Why the ADTS headers are built here rather than by a muxer |
| [Downmixing](../technical/audio.md#downmixing---audio-channels) | Folding 5.1 to stereo, and why it forces a whole-track re-encode |
| [Multi-audio broadcasts](../technical/audio.md#multi-audio-broadcasts) | Cutting every sound track independently |
