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
has the whole of it. Where a copy opens on an I that is not an IDR and its count
would derive below pictures already shown, `cut::poc_seam::carry_on` moves the
head's `pic_order_cnt_lsb` along (widening the field where it is too narrow), and
`Writer::hold`, `seal` and `release` keep every write to the muxer back until the
copy's first picture has said whether that is needed, so the mux order is the one it
would have been. A cut that needs no renumbering is byte-identical to one made
without it.

`--clean-joins` remains as an option: it spends up to two seconds more re-encoding to
reach an IDR, which restarts the sequence, and is off unless asked for because what
it costs is exactness. Two pieces of the core came out of that. `bitstream::starts_a_sequence` says whether
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

In a Dolby Vision recording the colour is decided by an RPU on every picture,
in a NAL type nothing else uses; the sequence header describes the base layer
at most, and in some recordings says nothing about colour at all. Copied
pictures keep their RPUs, and a range whose ends fall on entry points comes
through with every one intact, in `.ts`, `.mp4` and `.mkv` alike. A re-encoded
picture has none: libx265 will write RPUs, but only for the profiles libavcodec
can configure it for, and only when the pictures handed to it carry Dolby
Vision metadata of that profile. What then becomes of the Dolby Vision is
below.

So the attempt is made once, up front, by opening an encoder and seeing
whether it takes. Where it does not — the profile 4 recording measured here is
refused outright — the cut says so, and what happens next depends on what is
underneath the Dolby Vision. The record's `dv_bl_signal_compatibility_id`
says what the base layer is on its own: HDR10, SDR or HLG, or, at 0, nothing a
player can show.

- **A base layer that stands on its own:** the output becomes that base layer
  throughout. The stream-level record comes off, so does the Dolby Vision
  video stream descriptor (0xB0) in a transport stream's map, and so do the
  RPUs (NAL type 62) and the enhancement layer (63) of every copied picture. A
  stream that says Dolby Vision and then hands a player no RPU to drive it is
  worse off than one that never said so, and a `.ts` has no record of the
  kind an `.mp4` or `.mkv` carries: a player finding RPUs there takes it for
  Dolby Vision with or without the descriptor.
- **Nothing underneath:** the Dolby Vision stays — record, descriptor and the
  copied pictures' RPUs — and only the pictures a seam rewrites go without.
  The profile 4 recording is this case. It carries an enhancement layer and
  states no colour anywhere but in the RPUs; taking the Dolby Vision off would
  leave every picture of the output in no stated colour, to spare the few a
  seam rewrites.

A compatibility id of 0 needs care. A transport stream's 0xB0 descriptor may
stop before the compatibility byte, and the demuxer then fills in 0 — the
profile 4 recording is written that way. For profiles 7 and 8, 0 cannot be the
answer: a profile 7 base layer is a Blu-ray's HDR10 by definition, and profile
8 exists only as 8.1, 8.2 and 8.4, each a base layer that stands on its own.
Those two are taken as standing on their own whatever the byte says.

And only HEVC pictures can have their RPUs and enhancement layer taken off.
On any other codec (AVC's profile 9, AV1's profile 10), dropping the record
would leave the RPUs in place under a stream that no longer declares them —
the half-way state all this is meant to avoid — so the Dolby Vision is kept,
as it is for a compatibility id of 0.

The decision is made before the output declares its streams, because by the
first seam the header has been written.

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

The flag has since stopped being the last word for H.264 whose parameter sets
travel in band. [`leadrefs`](../../rust/crates/core/src/leadrefs.rs) follows the
reference buffer from each entry point during the same scan and builds the
reference lists of the pictures after it, so a leading reference picture that only
other leading pictures predict from no longer bars a copy by itself. The conditions,
including the one the `frame_num` gap adds, are in
[pitfall 3](algorithm.md#3-leading-pictures--the-heart-of-the-open-gop-problem).

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

**A Matroska file states its rate only where it is told one.** Its clock is in
milliseconds, and where a file states no rate a reader works one out from those
rounded stamps. The `.mkv` and `.webm` SmartCut wrote stated none, so the next
open of one -- a second cut, or the editor -- read a 23.976 title at 24.417 with
pictures that vary, a 59.94 one at 62.5, a 30 fps one at 30.303 and a recorder's
1080i at 31. The rate is now written (`rate_to_state` in
[`cut.rs`](../../rust/crates/core/src/cut.rs)) where every recording in the cut
runs at the master's rate and is not variable, and where the container's own
figures agree on that rate: the average at the declared rate, or at half of it
for an interlaced one, or an MPEG-1 or MPEG-2 stream, which states its own. A
phone recording that holds pictures in the dark, a slice the walk calls
variable and a join of recordings at two rates are left unstated, as they were.
The packets, their times and what they decode to are unchanged; only the decode
times libavformat derives for the first two packets move, by a millisecond.
Read back, the outputs count the pictures that were written, and cut again
fresh, from a saved seek index and again, they come out the same each time.

**A `.mkv` or `.webm` that states no rate at all is measured.** Where there is
neither an average nor a declared rate, libavformat declares the clock's tick
turned over -- a thousand a second, on a Matroska -- and that is what a phone's
variable-rate recording cut to `.mkv` here comes back with, having had nothing
true to state. Up to 0.8.27 it was taken at its word: the cut was planned at a
thousand frames a second and the encoder told the same, so each re-encoded
picture had a thousandth of a second's bits, came out near 28.5 dB, and
`--verify` failed it (24.3 dB at worst). A constant 90 fps `.mkv` with no
default duration went the same way, and a two-range cut of it wrote 467 of its
612 pictures. Now a declared rate that is a millisecond tick turned over, with
no average beside it, is taken as no rate (`outline_of` in
[`lib.rs`](../../rust/crates/core/src/lib.rs)); and where nothing is stated,
the first 240 pictures are read on a demuxer of their own and counted over the
time they span (`measured_rate`), less the last sixteen, which arrive in
decode order and may still have holes among them. A whole rate, or a whole
rate over 1.001, that the millisecond rounding cannot tell from the measured
one is taken instead; thirty stands only where nothing can be measured. The
phone's `.mkv` now reads at 56 a second, with pictures that vary, and the same
range re-encodes at 34.0 dB, against 33.7 cut from the original `.mp4`; the
90 and 144 fps files read as constant at those rates. A file with an average,
and every transport stream and MP4, never reaches the measuring.

**A range from the very start is read without a seek.** A demuxer just opened
is already there, and the probe's packets come first, as they were read. A seek
there makes libavformat forget what it knew of the timestamps, and an
`ffmpeg -c copy` `.mkv` of a broadcast recording -- which opens on two leading
pictures at negative times in front of its first key picture -- came back from
it with those two at positive times among the key picture's own: the cut kept
the two broken pictures and lost the real one (599 of 600). Read without
the seek, the first sound packet can arrive with no length, and is given its
frame's own (`take_audio` in `cut.rs`).

One kind of file can lose its head to the open instead. A WebVTT block
libavformat cannot parse makes it drop the rest of the cluster the block is in,
pictures and sound alike, and read from the open such a file lost about its
first two seconds (527 pictures of 584 on the material of
`tests/run_demux_tests.sh`, 826 frames of sound of 915). WebVTT is the one
subtitle format the Matroska demuxer parses rather than hands over; SubRip,
ASS, PGS and VobSub are passed through as they are, so a Matroska file with any
of those is read from the open like any other. But the seek cannot be made on
every WebVTT file either: a remuxed recording with a WebVTT track, every block
of which reads, lost a picture to it from its head (143 of 144, 10 dB). So a
Matroska file with WebVTT in it is sought only where the open really did lose
something (`sought_at_head` and `open_lost` in
[`sound.rs`](../../rust/crates/core/src/sound.rs)). The file is opened twice
more: once read on from the open, once sought to the head with the subtitles
switched off. Every picture and sound packet of the sought read from
before where the open stopped reading must be among the open's, by stream and
byte position; a packet missing, or one that does not say where it was read,
means the seek. A cover picture is left out of the check: Matroska shows an
attached image as a picture stream, but it is never read from a block and says
nowhere where it was read, and a video site's recording saved with its
thumbnail and its subtitles was sought for that alone and came out wrong at the
head (145 packets for 144, failing its check). That costs two opens and a read
of the head, and only on a Matroska file with WebVTT read from 0. The same rule decides the head of a
`--sound-only` file, which never sought at the head before and so lost the
same two seconds of sound. A file that has both an unparsable block in the
head and leading pictures at negative times still loses the leading picture:
neither read has all of it right.

**The sound ahead of the first key picture needs a seek of its own.** A
broadcast sends half a second to a second of sound before its first picture, and
copied into a Matroska file (`ffmpeg -c copy` of a recording) that sound sits
in a cluster of its own, ahead of the first key picture's. A seek to the head
does not land there: libavformat's index of a Matroska file is its cues, which
name key pictures only, so the sought read starts on the key picture's cluster.
Where a WebVTT block the demuxer cannot parse sits in that first cluster, the
open loses its sound too, and the sought read then has nothing the open lacks:
`open_lost` said no, and the head was read from the open. A `--sound-only`
file of the first ten seconds of such a recording came out 9.15 seconds long,
the 0.85 s at its head gone and everything after it that much early, where the
same file with a readable WebVTT track kept it. libavformat does know where the
sound starts: it indexes every cluster it reads a sound frame in, the open's
look-ahead among them. So where the sound track's first entry lies in a cluster
before every picture stream's, the read is sought on the sound track itself, to
that cluster, and read whole once the subtitles are switched off
(`sound_from_head` and `seek_to_sound_head` in
[`sound.rs`](../../rust/crates/core/src/sound.rs)). Where the open was not sought,
the same check as `open_lost`, made on the sound track alone, decides whether
it lost any. Only a Matroska file with WebVTT ever reaches this. The cut with
pictures starts on the first picture, so neither its pictures nor its sound
are changed by it (its head seek goes the same way where the open lost part
of the head, for the subtitles: see below). The readers
that take the sound from the head of the whole recording use it as well: the
waveform, the commercial detector's sound and the quiet detection, the fade-in
of a join (which reads the head again from a fresh open), and `--verify`, which
reads the head of each track by the same steps the sound-only writer takes for a
range that opens at the head. The saved waveform's number (`WAVE_VERSION` in
[`lib.rs`](../../gui/src-tauri/src/lib.rs)) went up to 3 with this, so an
outline saved by an earlier version is drawn afresh, once, the next time it
is shown.

**Or ahead of it in the same cluster.** Where the sound leads by less -- 0.38 s
in the case that showed it -- its first frames share the first cluster with the
key picture and come before it there. The sound's first entry was then the
pictures' own cluster, nothing lay ahead of it by the check above, and the head
was read as before: a seek skips every block before the key picture's instant,
and a WebVTT block the open could not parse, sitting between the sound's start
and the key picture, took the rest of them from the open too. A `--sound-only`
file of such a recording wrote 709 frames where the same file with a readable
WebVTT track wrote 721, its first 0.26 s gone and the rest that much early;
`--verify`, reading the head the same way, called it OK. So where the sound's
first entry is in the pictures' first cluster but its first instant comes before
the first picture's, the read is sought on the sound track as well, and the
cluster is read from the sound's first block. Every reader named above takes the
same rule; the cut with pictures, which leaves out the sound ahead of its
first picture, writes the same either way. The saved
waveform's number went up to 4 with this, as it had to 3.

Every other read that runs on from the head of the whole recording now asks
the same rule before it switches streams off: the film strip and the waveform,
the walk that builds the seek index (and the rate measured for a Matroska file
that states none), the black, white and quiet detections, the commercial
detector's sound, the proxy, and a logo search over the whole recording. Read
on from the open, each of them lost the two seconds the cut keeps. The proxy
began two seconds in (527 pictures of 584), so the editor showed nothing
there; the walk took the clusters it never saw for pictures held on screen,
called an eight-second recording one whose pictures vary, and the cut wrote
its `.mkv` with no rate; a quiet stretch that began inside them was marked
from where the read picked up again. The commercial detector also leaves
WebVTT out of the subtitle streams it reads for resets: WebVTT never carries
one, and with the stream on, a block it could not parse cost the sound of its
whole cluster at every cue.

**A cue ahead of everything in the first cluster moves the recording's zero.**
Where the first cluster opens with a cue, ahead of every picture and sound
block in it, and that cue is one libavformat cannot parse, the open sees none
of the cluster: libavformat's start is the next cluster's, 2.4 s on a file
whose twin with readable cues starts at 0. Everything before it lay before the
recording's zero. A cut of the whole file lost its first GOP (182 pictures of
240), and `--keep 0-2` cut another stretch than the twin's. So where the open
has lost something (`open_lost`, above), the file is opened once more with the
WebVTT switched off before libavformat looks ahead, an open that loses
nothing, and each WebVTT track's start is taken from its first entry in the
file's Cues, which name every cue without its block being parsed. The
recording's start is worked out from those streams' starts as libavformat
works it out (a text track starting less than a second ahead of the pictures
and sound is the start), which is where the readable twin starts, and is
written into the open, every stream's start with it (`true_start` in
[`input.rs`](../../rust/crates/core/src/input.rs)). Every reader of the open
counts from that zero: the outline and everything rebased by it (the index
walk, the plan, the cut, the sound-only file, the film strip and the
detections), the preview, the media information and the sound's priming rule.
A file whose open lost blocks after the first of each kind keeps its zero, and
is cut byte for byte as before. The answer is kept per file (by its size and
time) for as long as the program runs, since the check costs two opens and the
answer a third, and a cut opens a recording many times. A seek index saved
before holds every time on the old zero and no entry point for the first GOP,
so `seek_index::VERSION` went up to 16 with this, and every saved index is
made again once.

**The sound in that lost cluster is found by the open that lost nothing.**
The sound's own first index entry, which the seek to the sound's head goes by
(above), comes from what the open read -- and on such a file the open never
read the first cluster, so the track's index starts in the cluster after it,
past the pictures' own. The seek to the head then skipped every sound block
ahead of the key picture: a test file whose sound leads its first key picture by a
third of a second wrote a `--sound-only` file of 709 frames where its readable
twin wrote 721, and a VP9 and Opus `.webm` whose first cluster also opens with a cue 400 where its
twin wrote 402. The open that discards the WebVTT before it looks ahead reads
the first cluster whole, so it also notes the cluster each sound track's
first block is in, and that block's instant (`head_unlost` in
[`input.rs`](../../rust/crates/core/src/input.rs)). Where that comes before
the track's own first entry, it is put into the track's index and sought as
the track's first entry would be. Every reader that takes the sound from the
head goes through the same step, so the waveform, the detections, a join's
fade-in and `--verify` are read as the sound-only file is.

**And a first cluster that holds no key picture.** Where the sound leads by
more -- from about 0.6 s with one AAC track in the test files tried, already at
0.4 s with two -- the muxer starts a cluster at the key picture, and the first one
holds the cue and the sound alone. The seek to the head then lands on the key
picture's cluster, past the first, just where the open picked up: both reads
started in the same place, and `open_lost` had nothing to tell them apart by.
The file kept libavformat's zero (1.231 s on a file whose readable twin starts
at 0), so every `--keep` was counted from it, and a `--sound-only` file of the
whole recording wrote 510 frames where the twin wrote 564, its first 1.15 s
gone; the waveform and the commercial detector's sound lost the same stretch,
and `--verify` called the short file OK. So where the Cues the seek read put
the first cue in a cluster ahead of the key picture's, and the open handed
over nothing from ahead of the key picture's cluster, `open_lost` reads the
file a third time, through the open that switches the WebVTT off before it
looks ahead (`demux_unlost` in
[`input.rs`](../../rust/crates/core/src/input.rs)): a packet from before
everything the open handed over is one the open lost. `true_start` and the
seek to the sound's head then do the rest: the zero, the cuts, the
sound-only file, `--verify` and the commercial detection of such a file now
come out as its twin's do. Only a file meeting both conditions is opened
more often than before. An index saved before holds every time on the zero of
the cluster after, so `seek_index::VERSION` went up to 17, and every saved
index is made again once; the saved waveform's number went up to 5 for this
and the case before it.

The look backs for the subtitles up at a range's opening -- a broadcast's
captions, a DVD's subpictures, PGS -- left a range within the
first few seconds to read on from the open, since the open is already at the
top. On such a file the open is not: a look back read from it started after
the rest of the first cluster, and a subtitle up at the opening was left out
of the cut while the preview showed it. They now leave the head to the seek
every other reader uses, which reads on from the open except where the open
lost part of the head.

**And that seek starts where the sound does.** Sought to the head, the read
lands on the first key picture's cluster, since a Matroska file's Cues name key
pictures only. On a remux whose first cluster has no picture in it -- only the
sound a broadcast sends ahead of its pictures, and whatever else falls there --
the seek passed over that cluster and everything in it: a PGS display set up at 0, beside a WebVTT track the open could
not read, was in the open's packets, and was carried neither by the look back
nor by a cut from 0 -- 13 display sets where the readable twin had 14, and
where the cut of 0.8.32, read from the open, had them all. Where the open lost
part of the head, the cut's head seek now goes through `sound_from_head` on the
first sound track, as the sound-only file's does, so the read starts on the
cluster the sound starts in; a file with no sound is sought to the head as
before. The preview's subtitles open their first window the same way, and show
what the cut writes. The cut's pictures and sound come out as they did: it
starts on the first picture either way.

Only an answer that is known is kept for the rest of the run. A check that
could not be made, or a loss whose start could not be read, is asked again the
next time the file is opened, rather than leaving the file on libavformat's
zero until the program is closed.

A Matroska file with WebVTT and no Cues at all -- written to a pipe -- whose
open lost part of its head cannot be read again from the head: there is
nothing to seek by. It is refused rather than cut from the open, which would
lose the first cluster without a word (38 pictures of 240 and 57 frames of
sound of 376 on the file that showed it). The refusal now says why: `seek to
the head failed: … (the head of this Matroska file with WebVTT has to be read
again, as its open can lose part of it to a WebVTT block it cannot read, and
the seek there failed -- as it does in a file with no index (Cues), one
written to a pipe)`. It says no more than is known: the same seek is made
where the check of the open could not be made, and a seek can fail for other
reasons than a missing index.

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
