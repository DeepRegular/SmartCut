# Joining, the master clip, and transitions

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](joining.ja.md)

Everything else in this program takes one recording and writes part of it out.
This page is about the three things that happen once several recordings are
written into the same file: laying them end to end, making one of them fit the
shape of another, and putting something between them.

They arrived together because they are the same question asked three times. A
file has one answer for what its pictures are, and a list of recordings has as
many answers as it has recordings.

## A cut of one recording was already a join of one reel

The machinery this needed mostly existed, and the reason is worth stating
before the new parts.

A cut of a single recording is a list of kept ranges laid end to end on one
output timeline. Each range is **anchored to the instant its own pictures
start**, so an error at one seam cannot accumulate into the next; the sound of
each range is mapped against that anchor rather than carried forward; and every
segment opens the file it reads for itself rather than sharing one demuxer.
None of that was written for joining — it was written because a recording cut
into five ranges has four seams in it, and each of them is the same problem a
join is.

So the change is not "write a concatenator". It is: let the list of ranges come
off more than one recording.

```rust
pub struct Reel<'a> {
    pub src: &'a Source,
    pub plans: &'a [RangePlan],
    pub after: Transition,
}

pub fn join(reels: &[Reel], master: usize, output: &str, opts: &CutOptions) -> Result<()>
```

`cut` is `join` of one reel, and goes down the same path. That is checked
rather than asserted: `tests/run_join_tests.sh` writes the same single-clip cut
twice and compares the bytes.

## Everything the output has only one of comes from the master

A stream is declared before the first packet is written and cannot be taken
back afterwards. So the file has exactly one answer for each of these, and one
reel — **the master** — supplies all of them:

| | |
|---|---|
| The pictures | Codec, size, pixel format, frame rate, scan, pixel aspect, colour |
| The timeline | What a tick is, worked out from the master's rate; see `Grid` |
| The tracks | How many sound tracks the file has, what each is declared as, and what the captions are |
| The tables | A broadcast's own SDT, EIT and TOT, and the PIDs everything is written on |
| The subtitles a disc draws | The master's only; see below |

The master is the caller's choice and not the first row, because the two are
different questions. A list of twelve episodes and one trailer wants the
episodes' shape, and the trailer may well be at the top of the list.

### What a reel that is not the master supplies

Its pictures and its sound, and its captions. **Which of its streams stands in
for which of the output's tracks is decided by position** — its first sound
track is written into the output's first, its second into the second. There is
nothing else to go on: a PID means one thing in one broadcast and another in
the next, a stream index is something libavformat makes up per file, and a
language is missing more often than not. Two recordings of the same broadcast
carry their tracks in the same order, which is the case this is for.

A reel with fewer tracks than the master leaves a gap in the ones it has not
got, and a reel with more has the spare ones left out. Both are said once,
before anything is written.

**The subtitles a disc draws are carried from the master only.** Not a limit
that saves work — a limit that keeps an answer honest. What a graphics track
carries is not packets but the state of a plane: a display set puts something
on screen and a later one takes it off, the cut has to replay whatever was
standing when a range opens and take it down when the range ends, and a DVD's
are converted against a palette read off the disc they came from. None of that
composes across files. Two reels would be two planes and two palettes written
onto one track.

## The master clip: what a reel has to match, and what happens when it does not

`crate::conform` decides it, and only it, so that the answer a list is drawn
from and the answer the cut acts on cannot differ.

A clip matches when it agrees with the master on the codec, the frame size, the
pixel format, the frame rate, the scan, the pixel aspect and the three colour
fields. A sound track matches when it agrees on codec, sample rate and channel
count. Anything else is a mismatch, reported by name:

```
note: other.ts is not the shape of one.ts, so its pictures and its sound are
written afresh to fit it -- frame size: 640x360 vs 320x240; frame rate: 29.97
vs 25; sample rate: 48000 Hz vs 44100 Hz; channels: 2 vs 1.
```

The window asks the same question. The `join_fit` command takes the list's
paths and which of them is the master, and answers, per clip, whether its
pictures and its sound are written afresh and what differs. The output screen's
re-encode note asks this before it asks for a plan: the other way round, a clip
being written afresh from end to end is announced as copied losslessly.

### A field a recording does not state is not a difference

Everything compared above is read out of the container, and a transport stream
is under no obligation to describe itself. Broadcast MPEG-2 states its colour
in a sequence display extension that a recording begun mid-programme can be
missing altogether. The field order is worked out by libavformat from the
pictures it probed, and comes back unknown where those did not settle it. A
stream that never stated a pixel aspect ratio has none to read.

Those are gaps in the description rather than differences in the pictures, and
one recording of a programme has them where the next week's has not. Read as
differences they were an hour of encoding apiece — for two recordings off one
channel, which are the same pictures in the same format.

So **a comparison where either side is silent is passed over.** The output
states what the master states, which is the only claim there was, and the
silent recording contradicts nothing. The colour is three fields read one at a
time, so a recording that states two of them and leaves the matrix unspecified
is the same colour as one that states all three.

What this costs is a real difference only where a recording states something it
is not — and a recording whose description disagrees with its content cannot be
told from one that says nothing, so there was never an answer there to lose.
The rule is `conform::stated` and `conform::same_colour`.

### What actually differs, measured

A month of broadcast: 1186 recordings off 32 channels, read with `conformdiag
--group`. Twelve channels hold recordings that are not all one shape **and
differ in nothing but the scan**; three more differ in something real (a
programme broadcast at 720x480 among a channel's 1440x1080, one file that
would not open). Across the whole corpus, 1088 recordings come back
interlaced top field first, 76 progressive and 22 bottom field first — and 40
of those are the odd one out on a channel whose other recordings are all the
same.

The scan is libavformat's reading of the pictures it probed, not a field the
stream states outright. A Japanese broadcast carries progressive-coded frames
inside a 1080i stream constantly — most animation is coded that way — so
whether the frames a probe happens to see are progressive depends on whether
the recording began in the programme or in the commercial before it. The same
programme two weeks running comes back progressive once and interlaced once.

**That is a whole clip re-encoded for a reading rather than for a difference**,
and it is what somebody joining two episodes of one series actually hits. The
`unknown` case is answered above; `progressive` against `interlaced` is a
stated disagreement and is not, so it stands as a mismatch and the screen now
says which field it was.

**29.97 and 30 are not the same rate.** The tolerance is a ten-thousandth,
relative; what it is for is the last figure of a rate that arrived as a
decimal, and it has to be well under the thousandth that separates 30000/1001
from 30. A timeline built on either of those shows the other drifting a frame
every thirty-three seconds.

### A clip that does not match has every picture written afresh

There is no partial answer available. A clip of another size has no picture in
it the file could carry, so the planner's whole business — find the stretch
that can be copied, re-encode the partial GOPs at its ends — has nothing to
divide. `plan::reencode_range` hands over one segment per range, and
`conform_segment` decodes it and writes it into the master's shape.

Two things there are worth naming.

**The pictures are placed on the master's grid, not their own.** A seam places
each decoded picture at its own instant, and that is right where the pictures
around it came at the same rate. Here they did not: a 25 fps clip placed by its
own instants into a 29.97 file lands between the frames of every second one,
and what comes out is a stream whose pictures are neither one rate nor the
other. So the output's own frame instants are counted off from the range's
start and each is given whichever source picture was on screen at that moment.
Slower in than out and a picture is written twice; faster and one is passed
over. Measured: a 25 fps clip of 8.02 s joined into a 29.97 file came out 241
output frames, which is 8.02 s of the file's own rate.

That is the rate conversion, and it is the one a smart renderer can honestly
offer. Nothing here invents a picture that was never taken.

**The scaler is rebuilt whenever the decoder starts handing over a different
shape.** Not paranoia: a broadcast recording can change frame size mid-file
where the station switched feeds, and a scaler built for the first shape
silently refuses the second.

### A sound track any reel cannot match is written afresh for the whole file

One answer per track for the whole output, not one per reel. A track is
declared once, as one thing, and what a stream says about itself has to
describe every frame on it — so a track written from the master's own packets
for one stretch and from an encoder for the next would be a track whose two
halves are described by one header that fits one of them.

What crosses a reel boundary is **the encoder**; what changes is the decoder in
front of it. `audio::Reencoder::retune` swaps the decoder, the rate its samples
arrive at, the resampler between that rate and the output's, and the last
timestamp seen — and keeps the encoder, the count of samples fed, and the
samples already decoded and not yet framed. A second encoder started at the
join would number its frames from nought and write the rest of the file on top
of what is already there.

## Transitions

**A transition is the one thing in this program that cannot be smart
rendered.** Everything else here exists to leave the recording's own pictures
alone. A transition asks for pictures that are in neither recording — a frame
that is half of one and half of the other, a frame a quarter of the way to
black — so every frame it covers is made here, whatever the two clips have in
common. Asked for two seconds, it costs two seconds of encoding at each join
and nothing anywhere else.

It belongs to the clip **before** it, which is where the reference tool puts it
and the only place that reads in the order the file is written. On the last
clip there is nothing to give way to, so anything but a fade becomes a fade to
black at the end of the file.

### The two timings, and why there are two

| | |
|---|---|
| **Fade** through black or white | The clip before goes down to the colour over half the time, the clip after comes up out of it over the other half. Each gives the part of itself the transition covers, and the output is as long as it would have been without one |
| **Dissolve, wipe, slide** | The two clips are on screen together for the whole of it. Both give that many seconds, and the output is that much shorter |

Not a choice: it is what the two kinds *are*. A fade through black never has
both clips up at once, so asking it to consume both at once would be asking it
to throw one of them away; a dissolve has nothing to show but both at once.

### A transition is a segment of a range, not a range of its own

The sound is cut against the range — one boundary decision per end, one anchor
per range — so a range split in two to make room for a fade would put a seam in
the sound where the picture has none. Instead the range keeps its bounds and
gains a segment, and `Segment::retouch` says what that segment is.

The two exceptions are the two ends of an overlapping crossing, where the range
really does lose those seconds:

- **The clip after** a dissolve gives up its first `seconds` — they are on
  screen during the crossing, written by the crossing — so its range starts
  that much later, sound and all.
- **The clip before** gives up nothing. Its pictures are consumed by the
  crossing but its sound plays through it, which is what keeps the sound in
  step with the pictures on either side.

**So the sound is not mixed.** The clip before plays through the crossing and
the clip after starts where it ends. That is a decision and not an omission:
mixing two tracks would mean decoding both and encoding the result, which makes
every sound track in the file one this program writes rather than one it
copies — for a second of crossfade in an hour of programme. What softens the
change instead is a fade, which costs nothing that is not already being spent.

### A join's fade is asked for at each end

The fade at a cut inside one recording is `CutOptions::audio_fade`, one number
for the run. A join between two recordings is not that. How the programme that
is ending should end and how the one that is starting should start are two
questions, and one number cannot answer them: a programme that ends on its own
theme wants a long way down and nothing at all on the way up.

So `Transition` carries two lengths. `fade_out` is how long the sound of the
clip before takes to leave, `fade_in` how long the clip after takes to come
back. They are independent of the picture: a join whose `kind` is `None` can
carry a fade. A crossing takes material off both clips to happen in
(`Transition::takes`) and a fade is written over material that is staying, so
it changes no length.

A fade out lands on **the last sample the track still has** rather than on the
end of the range. A broadcast recording's audio often stops half a second
before its pictures do, and a ramp aimed at the range's end is still a quarter
of the way up when the sound runs out — a fade out that stops dead at quarter
level. `boundary_patches` takes the smaller of the range's end and the end of
the frames in hand. (That half second is filled from the next clip's own
sound — see `min_start` — which happens with or without a fade.)

`cut::fade_lengths` builds the pair — head and tail — for every range of the
job in the order they are written: `audio_fade` at a seam inside one recording,
and the join's own two numbers where one recording gives way to the next. The
two ends of the output are not seams and get nothing, which `audio::fades_for`
settles from the range's place in the job rather than from that table.

### The kinds, and the arithmetic they are made of

Every one of them is the same operation: for each sample of the output, choose
between two numbers or mix them, by a function of where the sample is and how
far through the crossing it is. `crate::blend` is the only place any of it is
written — four times over would be four places to get the chroma subsampling
wrong.

| | |
|---|---|
| Fade | Each sample moved towards the colour. **Studio black and studio white**, 16 and 235, not 0 and full scale: the pictures around it were coded in the same range, and a frame that goes past where they can reach is a frame that flashes |
| Dissolve | Each sample of one mixed with the same sample of the other |
| Wipe | An edge travelling across the frame, both pictures standing still |
| Slide | The same edge with both pictures travelling: the one before is pushed off the side the next one comes in from |

The edge is rounded rather than truncated into each plane's own samples, so the
luma edge and the chroma edge are at the same place on screen at every instant.
A half-sample apart is a coloured fringe travelling across the picture.

Everything is integer arithmetic — a per-sample float multiply over a 4K frame
is the difference between a transition that encodes as fast as it writes and
one that does not — and the formats accepted are planar YUV, eight or ten or
twelve bits, which is everything this program decodes. A packed or RGB frame is
refused rather than guessed at.

### Easing

The names are the reference tool's, which are the ones a person who has used
one of these before will look for: None, Back, Bounce, Circle, Elastic,
Exponential, Power, Sine, Quadratic, Cubic, Quartic and Quintic, each in In,
Out, In-out or Out-in. What each of them is, is the standard curve of that
name; there is nothing invented here.

Two of them leave the interval by design — Back overshoots and Elastic rings
past both ends — and that is left alone rather than clamped at the source. A
dissolve's mix clamps where it uses it; a wipe's edge does not, because an edge
a little off the frame is simply an edge off the frame. Every curve in every
mode starts at nought and arrives at one, which is checked: a transition that
did not would show a step at one of its ends, which is the thing it exists to
remove.

### An image over the crossing

A still — a title, a card — drawn over the transition and nowhere else, coming
up over the first quarter of it and going down over the last so that it is
never cut on or off.

**Nowhere else, and that is the whole reason it is offered at all.** Drawing a
picture over a programme means re-encoding every frame it covers, and a smart
renderer that quietly re-encoded an hour because somebody put a caption on it
would not be one. The frames a transition covers are being written afresh
already.

The image is read through libavcodec like everything else — PNG, JPEG, BMP,
anything it has a decoder for — and scaled twice: once into the output's own
pixel format for the colours, and once into RGBA for the alpha channel. An
image with no alpha of its own is solid. It is stretched to the frame rather
than placed in a corner, because where to put a smaller one is a question with
no answer that suits everybody and an image with an alpha channel answers it
itself.

### The preview on the settings screen (`crossview.rs`)

A join's transition is set in a window of its own, with the seconds either
side of the seam composited and playing. `crossview.rs` is the path that
produces them.

**None of the arithmetic is written here.** Every sample of the preview and
every sample of the output go through the same `blend.rs`, and how far
through the crossing each instant is comes from the same `transition.rs`.
The way to keep a preview and an output from disagreeing is for there to be
one of them.

What is written twice is the **schedule** — which instant shows what. The
cutter builds it as plan segments around a whole edit (`ranges_with_transitions`);
the preview has no edit, only two recordings and a setting, so it builds the
same thing around one seam. The tests at the foot of `crossview.rs` are what
hold the two answers together.

| | Preview | Output |
|---|---|---|
| Composited into | the preview's own size, `yuv420p` | the master clip's shape |
| Scaler | `FAST_BILINEAR` — a picture is looked at once and dropped | `BICUBIC` — it is paid for the length of the reel |
| Sound | the two clips in turn to one output device (`play_audio_across`) | the same order, written |

The composite is done in studio-range `yuv420p` and the conversion to JPEG
comes after it. The other way round, the studio black `Shade::yuv` gives
would land at a different brightness in the full-range `yuvj420p` a JPEG
wants.

## What was measured

`tests/run_join_tests.sh`, seventeen checks, fixtures generated so it needs
nothing but ffmpeg.

| | |
|---|---|
| Two clips of a shape | The file is the two lengths, holds every picture of both, its sound runs to the end, and it decodes with nothing to say |
| Three clips | In the order given |
| One that does not match | Every picture the master's width, the file as long as the two clips, the mismatch named before a byte was written |
| A fade | The file keeps its length, and the middle of the fade is black |
| A dissolve | The file comes out its own seconds shorter |
| A wipe | Writes and decodes |
| A crossing against a short range | Held to whichever of its two ends allows less, so the clip after it loses nothing the crossing did not show |
| A cut of one recording | Byte for byte what it was, and the run says nothing about joining |

On real material: two twenty-second stretches of a broadcast recording came out
39.91 s and 1,194 pictures against 598 + 596 written separately, with the sound
ending within a frame of the pictures and no complaint from the decoder. A
720x480 clip joined to a 1440x1080 master came out 1440x1080 throughout, 22.02 s
against a wanted 22.04, and the burnt-in timecode of the second clip read 6.039
at six seconds into it.
