# The Rust core (`rust/`)

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](rust-core.ja.md)

The Rust port is what ships. The Python implementation is kept as the test oracle.
The "first frame is 13 ms early" limitation is gone in this implementation — see
below.

Everything about audio has its own page: [audio](audio.md).

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
[pitfall 10](algorithm.md#10-the-picture-order-counts-either-side-of-a-splice-are-not-one-anothers)
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

**Matroska keeps lengths, as an MP4 does**, and was on the wrong side of that
line. Its `CodecPrivate` is the same `avcC`/`hvcC` record, and its muxer turns
start codes into lengths only where that record is itself written in start
codes — true of a cut from a transport stream, false of one from an MP4 or a
Matroska file. Those went through `Unframe`, so every copied picture reached
the file start-coded under a record that says lengths: `sps_id 32 out of range`
and `Invalid NAL unit size`, once per picture, from a cut of H.264 out of one
`.mkv` into another. Matroska now goes through `Reframe` like an MP4; only the
`avc3`/`hev1` tag stays the MP4's own. The same suite writes each fixture into
`.mkv` as well, and counts the decoder's complaints as well as the pictures.

**A `.m4v` is written as an MP4.** By its name libavformat hands it to the `ipod`
muxer, which is not one the test above recognised as keeping lengths, takes no
`avc3`, and takes no HEVC at all.

**In a join, each recording brings its own framing.** `Reframe` and `Unframe` were
worked out once, from the master, and every reel was copied through them: a
transport stream joined onto an MP4 went in start-coded, an MP4 joined onto a
transport stream went in with its lengths, and an MP4 joined onto another MP4 was
decoded against the first one's parameter sets. `framing_for` now works out a reel's
own: into lengths, whatever it arrived in and with its own sets in front of each key
picture; into start codes, unframed with its own sets. The sound had the same fault
one layer down. An MP4's raw AAC among a broadcast's ADTS frames stopped the MP4 and
Matroska writers at the first raw frame, since their muxers run the whole track
through `aac_adtstoasc`, and garbled a transport stream's. A copied frame is now put
into the master's framing on its way out (`AudioTrack::framed`), a header added or
taken off.

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
picture's display index. The output time base counts *fields*: one tick per
1/(2 × the frame rate's numerator) of a second — 1/60000 for 29.97 — so a field is
exactly the denominator in ticks and an ordinary frame twice that. Nothing is rounded
anywhere on the timeline, and a three-field picture is expressible, which is what
[2:3 pulldown](validation.md#23-pulldown-support-a-field-based-timeline) needs:

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

The background to this is [pitfall 1](algorithm.md#1-the-parameter-sets-spspps-do-not-match).

## Where the reference test became more accurate than Python

Python needed to launch ffmpeg a second time to read the bitstream, so it **sampled one
place in the file and applied the result to the whole thing**.

In Rust, `nal_ref_idc` can be read while the packets are already being scanned, so the
test is **evaluated exactly, per access point**. No extra pass, and no extra cost.

## MPEG-2: the encoder that decodes nothing

`rust/crates/mpeg2` is unlike anything else here. It is neither a decoder nor
an encoder: it **reads a coded picture apart and writes it back with less in
it**. No picture is ever reconstructed -- there is no inverse transform and no
motion compensation.

It is used in one place: fitting a disc. Where a night's recordings will not go
onto one, the pictures are written back at a share of their own size, and the
GOPs, the motion vectors and the timestamps stay the bits that arrived. See
[Fitting a disc](transrate.md).

How it is known to be right is the VC-1 encoder's idea pointing the other way.
That one writes something and puts it through a decoder; this one writes a
picture back **with nothing changed and compares the bytes**. A table with one
row wrong, a length counted one bit out, a run misplaced: all of them come out
as bytes that differ. Over eleven recordings and 709,534 pictures, none do.

## A picture is on screen until the next one

Two facts about a picture, and for most of this program's life they were the
same number. **When it arrives** is its timestamp. **How long it stays** is
whatever the stream says it is worth -- two fields, or three where MPEG-2
asks for a repeat. Where pictures come at a constant rate the next one is
always one frame away, so the second fact is the first one restated and
nothing had to tell them apart.

A variable-rate recording holds a picture for as long as nothing changed. A
screen capture holds one for minutes; a phone drops to half rate in the dark;
anything that has been through `mpdecimate` holds one wherever the repeats
came out. There the two facts are different numbers, and three things follow
from having counted only the second.

**Every seam lost the difference.** Each segment reported the span it
occupied as its last picture's position plus that picture's coded length, and
the next segment began there. Where the last picture was held, the difference
went missing and everything after the seam moved early by the sum of it.
Measured on a WebM whose pictures sit between 33 ms and 2.4 s apart, 46 of a
range's 505 pictures were a whole frame out; on one whose gaps run from 16 ms
to 117 ms, 598 of 628, by up to three frames. The copied stretches were exact
throughout -- the copy path places each picture by its own timestamp, and
that was never the problem.

**A range ended early.** The same arithmetic decides how long the last
segment is, so a thirty-second range whose last picture arrived two seconds
before the end came out twenty-eight seconds long, with the audio running on
past the end of the video.

**A range inside a hold had no picture at all.** `reencode_segment` takes the
pictures whose timestamps fall inside its window, and a window inside one
picture's hold contains none -- so the run stopped with `no pictures
decoded`, which on a screen capture is an ordinary thing to ask for.

Both are answered by asking the recording rather than the picture header.
[`held_to_the_end`] takes a segment's span up to where its display coverage
ends, which is a real instant: the entry point the next segment starts on, or
the end of the range, bounded by the end of the file. And the last picture
before a window is kept, and put in at the window's own start where nothing
of the recording's own arrives within a frame of it.

### The other way round, which is the way recordings actually vary

Everything above is about a picture that stays up too long. The recordings
people have vary the other way as well: a 23.976 programme with a second or
two of 59.94 in it, which is how a great deal of what is downloaded is
authored. Two things then go wrong, and both are the timeline itself rather
than anything counted on it.

**A field was 20 ms and the fast pictures are 16.7 ms apart.** The output
timeline counts in whole fields, so two of them landed on the same place, the
second had nowhere to go, and it was dropped -- with a note blaming a damaged
recording, which it was not. Measured on a three-range cut of a
twenty-four-minute programme: **11 pictures gone**.

**And the timeline was built on the average of the two rates.** 23.976 with a
burst of 59.94 averages 24.02, or 24.93 over a minute that contains one --
and a grid at that rate is a grid nothing in the recording was ever coded to,
so *every* picture in the cut is quantised to somewhere it never was. On the
same three ranges, 227 gaps came out wrong, the worst by 45 ms.

[`Grid`] answers both at once. Where the recording is variable the timeline is
built on the rate the container says the material was authored at --
`r_frame_rate`, the commonest duration in the sample table -- and each field
of it is divided into 32. Both numbers are then round: the base rate is what
the pictures come at nearly all the time, and a thirty-second of a field is
two thirds of a millisecond, which is finer than the containers involved
state their times in. The same three ranges now come out **6294 pictures of
6294, no gap wrong by more than 0.6 ms**.

**The two sides of the question are not asked the same way.** A gap too long
is what a hold looks like and also what a dropout looks like, so it takes a
share of them -- one in a hundred -- to tell a recording that varies from one
that is damaged. A gap too *short* has no such twin: nothing goes wrong with
a recording in a way that puts two of its pictures closer together than its
own rate allows, and every one of those is a picture the timeline has nowhere
to put. So four of them settle it, which is enough that one malformed
timestamp cannot and few enough that the ten fast pictures in a
twenty-five-minute programme can. Across seven broadcast and disc recordings
here, 47,971 pictures between them, there is not one gap shorter than half a
frame.

**The container can see half of this for itself**, which matters because the
decision is made before a frame is written and an MP4's index gives access
points and nothing else. A recording whose pictures average out faster than
the rate it declares has some of them closer together than that rate allows;
there is no other way to arrive at the average. The margin is a hundredth of
a percent, because a twenty-five-minute programme with ten fast pictures in
it averages two parts in ten thousand above its declared rate, and ten
pictures with nowhere to go are ten pictures dropped. Calling a constant-rate
recording variable by mistake costs nothing: the timeline it then gets is the
same rate divided more finely, which puts every picture in exactly the same
place. The one answer that would be wrong is taking an interlaced recording's field
rate for its frame rate -- a Blu-ray averaging 29.97 declares 59.94, and a
timeline counting fields of that makes every picture half as long as it is.
The container's own answer cannot produce it, since it only calls a recording
variable when the average runs *above* the declared rate; the walk has an
answer of its own, so [`Grid`] declines a declared rate above the average
rather than let the two of them meet in the one place it would matter.

**Neither touches constant-rate material**, and both are written so that they
cannot. The span is a maximum over the two answers, and they agree whenever
the pictures are a frame apart; the carried picture is only reached when the
gap to the first picture of the range is a whole frame or more, which at a
constant rate cannot happen. The thirteen cases in `tests/run_rust_tests.sh`
report the same counts, the same first timestamps and the same jitter of zero
as before, and `tests/run_vfr_tests.sh` has a constant-rate control in it for
the same reason.

### A rate with nothing round in it, and the ticks a container can state

The output timeline is counted in `2 * num * sub` ticks a second, where `num`
is the numerator of the rate it is built on. A round rate leaves that nowhere
near any limit: 60000 ticks for 29.97, and 1,536,000 for the finest grid
[`Grid`] builds. A rate that is not round is another matter. The average of a
recording that holds a picture whenever the light drops is a fraction with
nothing round in it -- one phone recording here averages 2033620773/34906711
-- and twice that numerator is over four billion, which is past the 32-bit
number an MP4 states its timescale in.

**What that failure looks like is a broken recording.** `avformat_write_header`
answers AVERROR(ERANGE), and the line the caller gets is `Numerical result out
of range`, naming neither the rate nor the container, with no output written at
all. The same recording writes to a `.ts` and to a `.mkv`, each of which counts
its own ticks, so it reads as something wrong with MP4 rather than something
wrong with the rate. `SMARTCUT_FFMPEG_LOG=2` is what says
`video_track_timescale` was out of range, and it is off by default.

So [`Grid`] approximates the rate until the timescale fits, and only then:
where it already fits the terms are untouched and the output is what it was,
byte for byte. For the recording above the nearest rational under the bound is
158226409/2715926, which differs from the average in the thirteenth figure, and
nothing moves as a result. A picture is placed by its own timestamp rather than
by counting this rate off, so what the approximation changes is the size of a
tick.

**Five phone recordings say what this is worth**, three H.264 and two HEVC, all
declaring 60 and averaging 56.59 to 59.98 by holding a picture in the dark, one
of them for four frames at a time. One is the recording that could not be
written; the four that already fitted come out hash for hash as before. Across
the five, every range cut keeps the pictures the recording had in it, holds
included -- 3,438 of 3,438 on the one that used to fail, its 151 held pictures
still held -- with no picture more than 11 ms from where the recording had it,
and sound and pictures ending within a frame of each other over a
fifteen-minute range.

[`held_to_the_end`]: ../../rust/crates/core/src/cut.rs
[`Grid`]: ../../rust/crates/core/src/cut.rs

## VP9 and AV1: the codecs that turned out to need nothing

These two were written down for a long time as having no elementary-stream
concatenation form, and were left out on that ground. The claim was wrong, and
what it cost was two codecs.

Neither carries a parameter set the way H.264 and HEVC do, and that is the whole
of why they splice. A **VP9** key frame writes its own frame size, bit depth and
colour config into the uncompressed header, and refreshes all eight reference
slots, so a key frame is a decoder reset that describes itself. An **AV1** key
frame has a sequence header OBU in front of it -- SVT-AV1, libaom and rav1e all
write one, ten headers for ten key frames in the fixture counted here -- and the
format allows the sequence header to change at exactly that point. So a partial
GOP written afresh, and the recording's own pictures after it, need nothing
patched between them. The generic path splices both, and did before anything was
written for them: what a measurement of a 554-second VP9 recording found was 900
pictures out of a wanted 900, decoded clean by libvpx and by libavcodec's own
VP9, with 82.9% of the packets the recording's own bytes -- the share the plan
said it would copy, to a tenth.

Three things did have to be said outright.

| | |
|---|---|
| **Which AV1 encoder** | `avcodec_find_encoder(AV_CODEC_ID_AV1)` answers libaom-av1, whose default `cpu-used` is 0: 348 seconds for two seconds of 1080p24. That is not a seam being written, it is an export that looks hung. `cut::encoders_for` names SVT-AV1, then rav1e, then libaom, and each is opened in turn -- an encoder being present is not the same as its being able to write this recording, and SVT refuses 4:2:2 when it is opened rather than when it is looked up. At preset 8 it writes the same two seconds in 3.97 s, 0.002 dB from libaom's |
| **The level is not one number** | A decoder puts an AV1 stream's `seq_level_idx` in `AVCodecContext.level` -- 0 for level 2.0, counting up -- and SVT-AV1 wants 20 for the same level. Handed the recording's own figure it said `Level must be in the range of [2.0-7.3]` at every seam. For AV1 and VP9 the level is left for the encoder to work out; a level describes what a decoder must keep up with, and the pictures written here are the size and rate of the ones they sit among |
| **The speeds** | Their defaults are set for encoding a film, and this writes a second of one. libvpx spends 6.52 s on two seconds of 1080p24 at `cpu-used 0` and 2.54 s at 2, for a thousandth of a decibel. The figures chosen put a seam at roughly what libx264 costs for the same seconds |

### And every encoder was on one core

Found while measuring the above. `avcodec_alloc_context3` leaves `thread_count`
at 1, not 0, and an encoder reads it literally -- so seams were written on one
core while the decode feeding them ran on four, which is the one thing
`video_decoder_with` was written to avoid at the other end. It never showed
on the codecs this started with: a seam of H.264 is a second of work either way,
and the measured difference there is nil. libvpx is 2.7x, and a 1080p VP9 cut of
two ranges went from 32.6 s to 13.1 s.

### The container

A `.webm` is Matroska with a short list of what may go in it: VP8, VP9 or AV1
pictures, Opus or Vorbis sound, and nothing else. SmartCut writes neither Opus
nor Vorbis, so a `.webm` comes out only where its sound is the recording's own,
carried through -- which is the ordinary case, since that is how the recording
arrived.

Taking these three as input turned the question round. For as long as WebM was
the only container that refused anything, there was one entry to grey out and
every other container held whatever reached it. A transport stream has no stream
type for VP8, VP9 or AV1: libavformat declares such a stream as private data of
no stated kind and writes the file without a word, and every player reads the
pictures back as `bin_data` -- carried, declared, unplayable. VP8 came out worse
again, declared as MPEG-4 video, which is a description that is not true.
QuickTime refuses the three outright, and TrueHD, FLAC and Opus with them.

So `carry` holds the table and every caller asks it. The transport stream is
answered from the list of what it *has* stream types for; the others from what
they are known to refuse, which is all a container that says no out loud needs.
A cut into a container that cannot hold what is going into it stops before the
output file is made, and the window greys the entry out with the same answer;
see [the design notes](design.md).

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
| [The three modes](audio.md#the-three-modes) | `copy`, `smart` and `reencode`, and what each costs |
| [Cutting per interval](audio.md#cutting-per-interval-and-the-drift-that-nearly-happened) | Why an MP4 `stts` track accumulates drift, and how it is bounded |
| [Smart rendering applied to audio](audio.md#smart-rendering-applied-to-audio) | Re-encoding only the straddling frames, the guard frame, and the silence test |
| [Writing MPEG-2 AAC](audio.md#writing-mpeg-2-aac---aac) | Why the ADTS headers are built here rather than by a muxer |
| [The opening is not the recording](audio.md#the-opening-is-not-the-recording) | Why a broadcast recording's own frames settle what its sound is, and the probe does not |
| [Downmixing](audio.md#downmixing---audio-channels) | Folding 5.1 to stereo, and why it forces a whole-track re-encode |
| [Multi-audio broadcasts](audio.md#multi-audio-broadcasts) | Cutting every sound track independently |
