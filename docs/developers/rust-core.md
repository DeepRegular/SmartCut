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
| Writing VC-1 | Done — intra pictures, from [this program's own encoder](#vc-1-the-codec-with-no-encoder) |

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

## The splice

A cut copies whole GOPs and re-encodes the fringes, so a few frames at each
boundary are written by this program's encoder and the rest are the
recording's own bytes. **What those few frames are written at is what the
recording came in at, and a fifth again** — the pictures being replaced were
coded by whatever made the disc or the broadcast, with as long as it liked to
spend, and coded once and quickly at the same rate they come out visibly
softer than the copied pictures beside them.

The count comes from the index pass, which is holding every packet anyway:
`walk` adds up the video packets and divides by the span. Nothing else can
say. A transport stream declares no bit rate per stream, and the container's
overall figure counts the sound and the tables in with the pictures.

It used to be `width × height × fps × 0.08`, which knows nothing about the
recording at all. On this material that was wrong by a factor of five or six:

| | source | written at | after |
|---|---|---|---|
| Blu-ray 1080p | 27.2 Mbit/s | 4.5 Mbit/s | 32.6 Mbit/s |
| UHD 2160p | 73.5 Mbit/s | 15.9 Mbit/s | 88.2 Mbit/s |
| Blu-ray 1080i, at a seam | 75 kB/frame copied | 26 kB/frame | 96 kB/frame |

Measured against the source, the 1080p clip above went from **29.7 dB to
35.1 dB** PSNR. Where an index did not read the pictures — [a disc's own
map](disc.md#the-discs-own-index), a container's seek table — the file's own
rate less what the sound is worth stands in, and the frame size is the last
resort behind that.

### The seam, and reaching for an entry point a copy can be joined onto

A decoder hands its pictures back in picture-order-count order, and the counts on the
two sides of a seam were written by different encoders —
[pitfall 10](../technical/algorithm.md#10-the-picture-order-counts-either-side-of-a-splice-are-not-one-anothers)
has the whole of it. `--clean-joins` spends up to two seconds more re-encoding to
reach an IDR, which is the entry point that settles it, and is off unless asked for
because what it costs is exactness.

Two pieces of the core came out of that. `bitstream::starts_a_sequence` says whether
a key packet opens a coded video sequence — an IDR NAL for H.264, type 19 or 20 for
HEVC, and `true` for everything that reorders nothing, so MPEG-2 and VC-1 never move.
And planning gained an entry point that has the recording in hand, **`plan_on`**,
because the reading this needs is not in the index and on a disc the index was never
walked: it came off the disc's own table. `plan` stays the arithmetic. `plan_on` costs
a handful of seeks rather than a pass — the byte each entry point begins at is already
known — and `examples/idrdiag.rs` is the same reading turned into a report, printing
the wait for a clean entry point from each of a recording's own.

### The container the pictures go into

An MP4 puts a length in front of every NAL and keeps the parameter sets in
the `hvcC`; a transport stream separates NALs with start codes and expects to
meet the sets in the stream. Re-encoded pictures come out of the encoder
start-coded either way, so **only the copied ones have to be re-framed** —
`Reframe` on the way into an MP4, `Unframe` on the way out of one.

The second of those was missing. A cut of an MP4 written as `.ts` carried its
copied pictures across untouched, four bytes of length where the decoder was
looking for `00 00 00 01`: on a ten second cut of a 4K clip, **36 pictures
arrived out of 635**, and the 36 were the re-encoded fringes. `tests/
run_rust_tests.sh` now counts the pictures in an MP4 cut and in the same cut
written as a transport stream, and they have to agree.

### HDR is in the pictures too

HDR10 is two SEI messages — the mastering display's primaries and luminance
range, and the brightest content in the recording — and on the files measured
here they are in the bitstream and nowhere in the container. A copied picture
carries its own; a re-encoded one had nothing to say, so a player changed its
tone mapping partway through the cut. `signalling_of` decodes one picture to
read them and hands them to the encoder as `decoded_side_data`, which is where
libx265 looks. Done once per cut, and only when there is something to
re-encode. The transfer says whether to look at all: PQ and HLG carry this,
`bt2020-10` — Blu-ray's wide-gamut SDR — does not.

### And a broadcast does not say HLG where you would look for it

A 4K broadcast carries HLG the way it has to for a receiver that predates it.
The sequence header says `bt2020-10`, which every decoder ever built
understands, and an `alternative_transfer_characteristics` SEI beside it says
the pictures are really HLG. libavcodec reconciles the two and reports 18,
which is what the pictures are — so `signalling_of` finds the recording HDR
and reads its mastering after all, and the paragraph above holds.

What does not hold is writing 18 back. Handed the resolved answer, libx265
writes a sequence header saying 18 while the copied pictures either side keep
saying 14: on a 8.5 second cut of a satellite test stream, **the 99
re-encoded pictures described themselves differently from the 410 copied
ones**. A player that reads the SEI sees HLG throughout and notices nothing; a
player that reads only the sequence header — which is legal, the SEI is
optional — sees the transfer change at both seams. So
`bitstream::coded_transfer` reads the transfer out of the recording's own
sequence header, and where that differs from the resolved one the encoder is
given the recording's value and told `atc-sei` separately. Both halves of the
signalling then come out the way they went in. Only libx265 can be told this,
so it is the only codec it is done for.

### Dolby Vision is in the pictures, and cannot be written back

A Dolby Vision recording says nothing about colour in its sequence header at
all — an RPU on every picture carries it, in a NAL type nothing else uses.
Copied pictures keep theirs, and a range whose ends fall on entry points comes
through with every RPU intact, in `.ts`, `.mp4` and `.mkv` alike. A re-encoded
picture has none: libx265 will write RPUs, but only for the profiles
libavcodec can configure it for, and only when the pictures handed to it carry
Dolby Vision metadata of that profile.

So the attempt is made once, up front, by opening an encoder and seeing
whether it takes. Where it does not — the profile 4 recording measured here is
refused outright — the cut says so, and the Dolby Vision the recording
declares at stream level comes off the output with it. A stream that says
Dolby Vision and then hands a player no RPU to drive it is worse off than one
that never said so. The decision is made before the output declares its
streams, because by the first seam the header has been written.

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

## VC-1: the codec with no encoder

Every codec this program cuts has an encoder in libavcodec, bar one. There is no
VC-1 encoder anywhere — not in libavcodec, not on a graphics card, not in any
free implementation. `ff::encoder::find(AV_CODEC_ID_VC1)` comes back empty, and
until this was written a cut of a VC-1 recording stopped there.

That matters because of what is written in VC-1: most Blu-rays pressed before
about 2010. Reading them was already done ([discs](disc.md)); cutting them was
not.

So `rust/crates/vc1` is an encoder for the format's intra pictures, and nothing
else. What makes that a reasonable thing to write, rather than a research project,
is how little a smart cut actually asks for. Each end of a range needs one
fragment of a GOP — a few dozen pictures — spliced in front of a copy of the
recording's own bytes. Nothing outside the fragment may be referenced, so every
picture in it can be intra, and an intra-only encoder is only a fraction of the
format:

| Not needed | Why |
|---|---|
| Motion estimation and vectors | An intra picture predicts from nothing |
| The five bit-plane packings | Raw mode writes a bit per macroblock in the macroblock layer, and is legal everywhere |
| Variable block transforms | The discs in question declare `VSTRANSFORM=0`, so every block is 8x8 |
| Overlap smoothing, AC prediction | Both optional, and both declined in the picture header |
| A sequence header of its own | The recording's own is restated in front of every picture, which is what the discs themselves do — so the copied pictures across the splice are decoded against exactly the parameters they were coded with |

What is needed is the block layer: the DC differential codes, the AC coefficient
codes of two coding sets, the coded-block pattern and its prediction, the DC
prediction, the interlaced scan, and the 8x8 transform. The tables are the
format's, generated mechanically from the ones in FFmpeg's decoder rather than
transcribed — a single wrong entry in a few thousand produces a stream that
decodes to noise somewhere in the middle.

The transform is worth a note. The format defines only the inverse, and leaves
the forward one to whoever writes the encoder. `transform::inverse` is the
decoder's, ported so a test can hold the two against each other;
`transform::forward` is its algebraic inverse — the matrix is orthogonal with a
different norm on each row, so each coefficient is scaled by a factor of its own.

### How it is known to be right

There is no second VC-1 encoder to compare against, so the test is the round
trip: encode a picture, hand it to the decoder every player uses, and measure
what comes back (`tests/run_vc1_tests.sh`). Both halves matter. A subtly
malformed bitstream still decodes — to noise, or to one bad macroblock row — so
"it decoded" proves nothing on its own. And a transform scaled wrongly produces
a perfectly legal stream of the wrong brightness, which decoding without errors
would never reveal.

Two discs are held against it, and between them they cover the ways an
advanced-profile stream can differ. One is 1920x1080i at 29.97, coded as
interlaced frames, non-uniform quantizer, every GOP closed. The other is
1920x1080 progressive at 23.976, uniform quantizer, every GOP open, the
pulldown flags in their whole-frame form, skipped pictures in the run — and
heavy film grain, which is the hardest thing to put through a quantizer
without it turning into a smooth patch. Measured against the pictures each was
given:

| Quantizer | 1080i, animation | 1080p, film grain |
|---|---|---|
| 4 (the default) | 48.0 dB, 109 KB a picture | 44.8 dB, 205 KB a picture |
| 8 | 45.2 dB, 76 KB | 41.0 dB, 129 KB |
| 16 | 42.4 dB, 50 KB | 37.3 dB, 74 KB |

Grain costs twice the bits for three decibels less, which is what grain does to
any encoder. The context is what makes that figure meaningful: the grainy disc's
own I pictures average **327 KB**, over half as large again as the 205 KB written
here, and the animation disc's average 166 KB against 109 KB. An intra picture out
of this encoder is smaller than the ones the disc puts in the same places, so no
player is being asked for anything it was not already being asked for.

Real cuts of both discs, ten seconds mid-GOP to mid-GOP, came out with 90% of the
video byte-identical to the source and the rest written afresh. The grainy one,
examined at 1:1 against the picture it replaced, keeps its grain.

### What is refused

Pan-scan windows, and a quantiser that varies at the picture edges (`DQUANT=2`).
Both sit in the middle of the picture header and would have to be mirrored
exactly, and neither has turned up on a disc, so a recording that carries either
is refused by name rather than written wrongly.

Field pictures (`FCM=11`) are read but never written. Where a stream is
interlaced, this encoder writes interlaced frames, whichever of the two the
pictures around it used. VC-1 states that per picture, so the two may sit side by
side in one stream.

### Left for later

None of these stops a cut. They are written down because each was a decision
taken to get the encoder working, and every one of them is a place where the
next person to open this file will wonder what was intended.

| | |
|---|---|
| **The window offers no step** | `--vc1-quant` exists on the command line and nowhere else. The window has no video settings at all — every control on the output screen describes the sound — so this would be the first, and where it belongs is a question about that screen rather than about the encoder. Until then a cut from the window uses the default |
| **Field pictures are not written** | Where a stream is interlaced, this writes interlaced frames. That is legal beside field pairs, but it has never been put in front of a decoder that was reading field pairs on either side of it. Both discs to hand code frames; a stream that codes fields would want a run of pictures written both ways and compared |
| **The pictures cost more bits than they need** | Three things were left out for being optional: AC prediction, the two escape forms that write a coefficient as a difference from a table entry rather than in full, and any choice among the AC tables — one pair is picked from the quantizer and used throughout. Each would take a few percent off, and a fragment is short enough that none of it has mattered yet |
| **A fragment is all intra** | The largest saving on the bit rate is not in any of the above: it is P pictures, which would need motion estimation and the whole of the syntax that carries a motion vector. That is a different project, and what made this one feasible was being able to avoid it |

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
