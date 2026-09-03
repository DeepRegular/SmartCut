# State of the Rust implementation (`rust/`)

[← Documentation](README.md) ・ [← SmartCut](../README.md) ・ [日本語](rust-core.ja.md)

The port is what ships; the Python implementation is kept as the test oracle.
**The "first frame is 13 ms early" limitation is gone** — see below.

| Part | State |
|---|---|
| Access point index and leading-picture analysis | Done (output identical to Python) |
| Leading-picture reference test | Done, and **more accurate than Python** |
| Planner | Done (agrees with Python on 11 cases) |
| Cutting (copy path) | Done |
| Cutting (re-encode path) | Done |
| Resolving mixed SPS/PPS | Done (`avc3` plus parameter set re-insertion) |
| Audio (copy) | Done (sync verified by measurement) |
| Audio (smart rendering) | Done (only the boundary frames re-encoded) |
| Audio (re-encode) | Done (sample-accurate, and MPEG-2 AAC on the way out) |

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

| Mode | Boundary error | Left over at a seam | Frames re-encoded |
|---|---|---|---|
| `copy` | ±half a frame (10.7 ms for AAC at 48 kHz), no accumulation | up to 21.3 ms of the audio that was cut away | none |
| `smart` (default) | as above | nothing (it is silence) | at most 2 per boundary, none when the seam is in silence |
| `reencode` | **-0.02 ms** (one sample is 0.021 ms, so effectively zero) | nothing | all of them |

Run `tests/run_audio_tests.sh` with `SMARTCUT_AUDIO=reencode` and all 5 cases
land at -0.02 ms. Video stays lossless in every mode (1798/1798, 99.1 % on real
material).

**The default is `smart`.** On an ordinary cut -- a commercial break taken out
in the silence around it -- its output is byte-identical to `copy`, so there is
nothing to lose by making it the default; where they differ, the cut is in the
middle of sound and the frame is worth touching. `copy` remains for work that
must be exact to the byte. The ±10.7 ms boundary error is the same in both, and
far below the perceptual threshold for lip sync.

A trap hit while implementing this: audio frames straddling a segment boundary
were **submitted twice, once from each of the two adjacent segments**, and the
error accumulated one AAC frame (21.3 ms) at a time. Fixed by adopting the same
exclusive ownership rule as the copy path.

`tests/run_audio_tests.sh` — measures A/V sync sample by sample on material with
a 2 ms impulse every 0.5 s. Steady tones cannot reveal a sync error.

## Smart rendering, applied to audio (`--audio-mode smart`)

The boundary error is not the only thing copying leaves behind. **The frame a
boundary falls inside is copied whole**, and what is inside it includes the
audio from the side that was cut away. That is the last syllable of a
commercial arriving over the opening of the programme: up to 21.3 ms of it,
once per seam, every seam.

Smart mode re-encodes **only the frames the boundaries fall inside**. Each is
built from the recording's own samples, with everything outside the kept range
faded to silence over a millisecond. No other frame is touched. On real
material (Nippon TV, two intervals, four boundaries), **5602 of 5606 frames
are the recording's own bytes; 4 were rewritten**.

**The boundary itself does not move.** Two frames cannot occupy one instant --
MP4 lays its samples end to end, and MPEG-TS rejects a timestamp that goes
backwards -- so an interval is always a whole number of frames long. Opening
an interval on the frame the boundary falls inside would lose nothing, but
that frame reaches back into the interval before it, where the previous
interval's own overrunning last frame already is. Every mode therefore keeps
whole frames and centres the error; what smart mode changes is **what fills
the rest of the straddling frame**.

One neighbour is re-encoded with it, as a **guard**. AAC frames overlap by
half a window, so the decoder rebuilds each frame from that frame and its
neighbour, and a re-encoded frame sitting directly against a copied one is the
one place the two halves can disagree. Silencing part of a frame is a
transient, and a transient is what makes an encoder switch to short windows --
which is exactly that disagreement. The guard carries the recording's own
samples, unmasked, and keeps the switch one frame away from the copied
material. When the straddling frame is not the one the interval opens on --
the nearest-frame rule may pick the next -- the guard has nothing to guard and
is not re-encoded either.

**When the far side is already silent, nothing is re-encoded.** A commercial
break is cut in the silence around it, so the far half of the straddling frame
is usually silent already and replacing it would remove nothing. The test is
whether the samples outside the range peak above -60 dBFS. On real material, a
cut that takes out one commercial block -- all four boundaries inside the
silence -- re-encodes **no frames at all, and the output is byte-identical to
`copy`**. The re-encode only engages where a cut lands in the middle of sound.

That design came from measuring TMPGEnc MPEG Smart Renderer 6.1's output
against its source. It re-encodes **not one audio frame**: of the 67499 frames
in a 24-minute cut, 67492 (99.99%) carry the recording's own coded audio, and
the seven that do not sit nowhere near a join. All three of its joins are
inside digital silence at -91.0 dBFS, where cutting on a frame boundary loses
nothing. (It does re-pad every frame to a strict 256 kbps CBR, so only 24.7%
of its frames are byte-identical -- the audio data inside them is untouched.)

Whether a frame can be replaced at all comes down to **the encoder's delay
being a whole number of frames**: the replacement has to cover the same
samples the recording's frame did, and a fractional delay puts every packet
the encoder makes off the recording's frame grid. AAC's delay is 1024, exactly
one frame, and lines up. AC-3's is 256, and does not. An encoder is opened
once before the run to check, and when it does not line up the tool says so
and copies instead.

## Writing MPEG-2 AAC (`--aac`)

A Japanese broadcast carries **MPEG-2 AAC**: the ADTS `ID` bit is 1, profile
LC, 48 kHz, with a CRC. FFmpeg's AAC encoder produces MPEG-4 AAC, and every
muxer that frames raw AAC for us writes `ID = 0`. The ADTS muxer has a
`write_mpeg2` option, but **MPEG-TS has no way to pass it down to the ADTS
muxer it uses internally**. Left alone, a cut comes out as a stream that is
MPEG-2 nearly everywhere and MPEG-4 for one frame per seam, which the tools
downstream of a recording read as malformed rather than as the recording they
were handed.

So the ADTS headers are written here instead (`adts.rs`). The profile,
sampling frequency, channel configuration and `ID` are read off the
recording's own frames and put in front of the encoder's output. **Every muxer
involved leaves a packet that already begins with a sync word alone**, which
is what makes this work: MPEG-TS passes it through untouched, and MP4 runs it
through `aac_adtstoasc` exactly as it does the source's own frames.

- The default follows the recording (`--aac auto`), so broadcast material
  stays MPEG-2.
- Only **the frames this tool writes** are framed here; copied frames keep
  their own headers. So a version that disagrees with the recording cannot be
  delivered while anything is being copied -- it would make a stream that is
  two kinds of AAC at once -- and the request is refused with a note instead.
  Under `--audio-mode reencode` nothing is copied and it is honoured. The
  tests check both.
- The payload is kept inside MPEG-2 AAC LC as well: perceptual noise
  substitution, an MPEG-4 tool, is switched off.
- The frames written here carry no CRC (`protection_absent = 1`).
  It is a per-frame field, so such a frame sits legally among frames that have
  one, and writing a CRC that was subtly wrong would be worse: a decoder that
  checks it would throw the frame away.
- `--audio-mode reencode` uses the same framing, so **a whole track
  re-encoded into a transport stream also comes out MPEG-2 AAC** -- a
  combination FFmpeg on its own cannot be asked for.

`tests/run_aac_tests.sh` — walks the output's ADTS frame by frame, checks they
are all MPEG-2 LC, and counts how many are the recording's own bytes. Neither
question can be put to a decoder; both are about the bytes.

## Downmixing (`--audio-channels`)

Some recordings carry 5.1, and a good many of the places they end up do not
want it: a television that folds it badly, a player that puts the dialogue in
the centre channel and never plays it, a phone. Asking for stereo is asking for
the track to be rebuilt, and this is the one audio setting that **decides the
mode rather than living under it**. No frame of a 5.1 recording can be spliced
into a stereo track, so `smart` and `copy` have nothing to offer here: a
channel count that is not the recording's makes the cut a whole-track
re-encode, and says so on the way past.

The fold itself is swresample's, which means the rematrixing coefficients are
libav's own -- centre at -3 dB into both, the surrounds into their own side,
the LFE dropped. What comes out is what a player downmixing the recording would
have produced, which is the point: the recording is being fixed, not
reinterpreted.

- **In and out at the same rate**, which is what keeps it a per-frame
  operation. swresample hands a frame's samples back one for one and holds
  nothing over, so the sample window each keep-range is trimmed against still
  means what it meant on the source's own clock, and the boundaries stay
  sample-accurate.
- **The ADTS header has to be rewritten too.** A transport stream says how many
  channels a frame has in the frame's own header, and the header these frames
  inherit is the recording's. Left alone it announces 5.1 over a stereo
  payload, and a decoder that believes the header ahead of the payload gets
  neither. `AdtsFormat::with_channels` puts the `channel_config` right;
  a count with no configuration of its own (7 channels, say) is left alone,
  because such a stream describes itself in a program config element inside the
  frame instead.
- **The output stream's parameters come from the encoder** once the channels
  have changed, framed output or not -- the recording's parameters describe a
  track the file no longer contains. This costs the framing nothing: a packet
  that already begins with a sync word is passed through whatever the extradata
  says.
- **A derived bitrate comes down with the channel count.** 384 kbit/s is what
  the 5.1 cost, not what the stereo it was folded into is worth, so the rate
  taken from the recording is scaled by the fold and floored at 128 kbit/s.
  `--audio-bitrate` is taken as given.

`tests/run_downmix_tests.sh` — a 5.1 fixture with a different tone in every
channel, so the fold is read out of the spectrum rather than off the metadata:
the centre has to arrive in both output channels, the left surround in the left
only, and the LFE nowhere. It checks the ADTS `channel_config` as well, and
finishes by measuring A/V sync through the fold on a 5.1 impulse fixture --
0.83 ms at worst, against the 1 ms bar a whole-track re-encode is held to.

## Multi-audio broadcasts -- all of them, not one

A bilingual broadcast sends its sound on **two separate PIDs**. Only one used
to be picked up, by `best(Type::Audio)`, so the second language was never
even read.

`Source.audios` now lists every sound track the recording carries.
`Source.audio` stays, as the *main* one: commercial detection, preview
playback and the `.aac` sidecar each read a single track, and which one that
is remains a real question.

On the way out, **each track is cut on its own**. That is the whole point,
and the reason the writer's state was split from one set into one set per
track: two tracks have their frames at different instants, so

- a boundary falls inside a different frame on each (a different number of
  frames get re-encoded),
- each accumulates a different drift from range to range,
- and they need not even be framed the same way -- one MPEG-2 AAC, the other
  MPEG-4.

Use one track's answer for the other and the other slips a frame per range.
So an `AudioTrack` holds everything true of one track alone -- its output
stream index, how far it has been written, the previous frame's `pts`, its
table of re-encoded boundary frames, its re-encoder -- and the `Writer` holds
a list of them.

Options like `--audio-channels` apply to every track, but the *decision* is
per track: only a track that actually has to be folded becomes a whole-track
re-encode, and a track that was already stereo is still copied.

A track that is not wanted is dropped with `--drop-stream <stream index>`
(in the GUI, the cut editor's track menu). The default is to **write them
all**: a track nobody asked about is a track that was in the recording, and
dropping it silently would be this program deciding what the recording is
for.

## The encoder knows its own delay

The re-encoding path was a frame out, and had been all along. It counted the
encoder's delay itself, from `initial_padding`, dropped that many opening
packets, *and* subtracted the same amount from their timestamps -- taking it
off twice.

A libav encoder declares its delay by stamping its output: the first packet
comes out at `pts = -1024`, covering the window that reaches back before
anything was fed, and the next at `pts = 0`, covering the first frame that
was. The packet says where it belongs, so the fix is to believe it -- drop
what lands before zero and take the timestamp as the position. The A/V length
skew in `run_audio_tests.sh` under `reencode`, which peaked at 21.3 ms
(exactly one frame), now peaks at 16.0 ms and averages 6.4 ms.

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
