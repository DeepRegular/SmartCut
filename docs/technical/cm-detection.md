# Commercial detection internals

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](cm-detection.ja.md)

**How to use the detector is in the user guide: [Commercial detection](../user-guide/cm-detection.md).**
This page is the implementation and the measurements behind it (`cm.rs`,
`logo.rs`, `caption.rs`).

## What came out of trying to improve accuracy

The work started as "reduce the misses", but building a ground truth by eye showed
that the problem was the other way round. The **five minutes of "commercial block"**
being emitted on the BS Fuji recording were **programme from end to end**.

The ground truth came from laying out thumbnails every 30 seconds across the whole
recording and checking them by eye. A detector's own output cannot be used to judge
the detector.

| Material | Actually | Before | After |
|---|---|---|---|
| Terrestrial Nihonkai TV | 4 s of commercial at the head plus 4 commercial blocks | 4 (the head missed) | 5 (correct) |
| AT-X | 5 minutes of commercial at the end | 1 (correct) | 1 (correct) |
| **BS Fuji** | **No commercials** | **1 (a 5-minute false positive)** | **0** |
| NHK E | No commercials | 0 | 0 |

### The two changes that worked

**Take the grid tolerance from the silence itself.** A seam's time is represented by
the centre of the silence, but the actual cut may be anywhere inside it: a long
silence is ambiguous about where the seam is. A fixed ±0.4 s was too narrow for
terrestrial recordings with 1.4 s silences and too wide for BS with 0.5 s ones. The
tolerance became "the mean of the two silence lengths, capped at 0.6 s, plus
0.15 s".

**Require a block's 15-second boundaries to be filled.** A run of commercials has a
silence every 15 seconds, because that is where one commercial ends. Measured,
correct blocks have 81–100% of their boundaries filled while false positives have
only 23–43%. A threshold of 0.6 separates them cleanly.

The second change works because a talkative programme already has roughly one
silence every 15 seconds. The BS Fuji recording has 91 silences in 1440 seconds, one
every 16 s, so landing on the grid carries almost no information by itself. What had
to be checked was not "it lands on the grid" but "**it lands on the grid with no
gaps**".

### Then the run was being counted wrong, and the score leaned on it

Both of those depend on how long a chain of grid-aligned neighbours a silence belongs
to. The walk that counts the chain searched the list from index nought in **both**
directions, so the backward half stepped straight to the earliest neighbour on the
grid and skipped every junction between. On a break of three junctions a minute
apart, the last one chained sixty seconds back to the first, found nothing before
that, and counted a run of two where its neighbours counted three: it scored 0.51
against their 0.68, fell under the threshold, and the break was left with two
junctions and thrown away for having too few. On a half-hour broadcast with no logo
and no subtitle resets, that was the whole of the detection — **nothing found where
there is a sixty-second break**. Each direction now takes the *next* junction its own
way, rather than the furthest one that happens to sit on the grid.

**And counted properly, the chain then decided too much.** A pause every fifteen
seconds is what conversation sounds like, so in a talkative programme the chain runs
long everywhere and saturates. At the weights it had — 0.35 for the length of the
silence, 0.65 for the chain — a 0.41 s pause, the shortest length this even looks at,
chained four deep and scored 0.63, over the 0.6 a block is built from, and bridged
the pauses either side of it into twenty-nine seconds of "commercial" in a recording
that has none.

The two halves are **evenly weighted** now, 0.5 and 0.5. That pause scores 0.58 and
the run it would have bridged never forms, while a real junction — a second of
silence — clears the line on the strength of its own length.

### Ideas tried and dropped

Both were measured before the hunch was implemented, and neither worked:

- **Snapping to scene changes.** The centre of a silence is a coarse position for a
  cut, so pulling it towards a scene change inside the silence ought to sharpen the
  15-second grid. The median improved but the worst case got worse: on Nihonkai TV,
  neighbours landing within ±0.4 s of the grid went from 27/29 to 22/29. Commercials
  contain scene changes too, and it snaps to the wrong one.
- **Scene change density.** Commercials should cut faster than the programme. The
  measured ratios were 3.13× on terrestrial, 1.58× on AT-X, and **1.08× on BS
  Fuji** — anime cuts plenty fast.

### The commercial at the head of a recording

This came from a report that a leading commercial was never removed. Logo absences
shorter than 20 seconds are ignored, because no commercial block is shorter than
that. But the head of a recording is a different case: those few seconds are the
recorder starting before the programme did, not a block.

The 3.5 s from `00:00:00.665` to `00:00:04.169` was exactly that, and it fell below
the threshold and was thrown away. An absence touching either end of the recording
is now treated as an **edge, not a break**, and one second of it is enough to pick
it up. Where the logo first appears is the head of the programme.

### Placing boundaries to the frame

A block's times are only estimates: the centre of a silence, or the moment a moving
average of logo strength crossed a threshold. Neither is a picture, and in practice
they are off by 0.2–0.33 s. The real boundary is the change between two consecutive
pictures, so that window is decoded and searched (`thumbs::cut_near()`).

This was got wrong twice before it was got right:

1. The first attempt snapped to the nearest mark in the scene index. That index is
   built from key pictures every 0.5 s, which is coarse, and commercials contain
   cuts too.
2. The second attempt took the largest difference within the window. Since a cut
   inside a commercial is more dramatic than the cut at its edge, that dragged the
   boundary a full second into the commercial.
3. The right answer is **the nearest cut**. The incoming estimate is close to begin
   with, so all that was left to decide was which picture the change happens on. The
   window is ±0.5 s.

The fix proved itself. The four Nihonkai TV blocks went from 119.8 / 120.2 / 59.9 s
to **120.0 / 120.0 / 60.0 s** — and since commercials are sold in 15-second units,
lengths only become exact multiples when the boundaries land on real cuts.
`tests/run_cm_tests.sh` checks the multiple-of-15 property.

The leading commercial's boundary was checked to the frame as well: 3.80 s is still
commercial (a picture of a can), and 3.835 s is the programme's first picture. The
mark is in the right place.

## How it is scored

The ground truth for five recordings is pinned in `tests/run_cm_tests.sh`. The two
with no commercials are the more valuable ones, because they guard the side that
must not emit anything. On a machine without the material they **fail rather than
SKIP**: a run that checked nothing must not read as "passed".

Scoring is `tests/cm_score.py`. It does not count blocks against a ±2 s tolerance;
it reports two numbers, in seconds:

| | |
|---|---|
| Programme wrongly cut | Programme swallowed by a block. Gone the moment you cut |
| Commercial left in | Commercial left outside the blocks. Annoying, but visible |

Each gets its own budget, for the reason given at the top of this page.

**Some seconds are neither.** Slot idents, programme promos and sponsor credits —
material the broadcaster places around the seams — are a matter of viewer taste, not
a fact about the recording. Forcing a decision would make the score lean whichever
way it was forced, so they are marked grey (`~START-END`) and excluded from both
counts.

The seconds budget catches large mistakes; boundary precision is measured with a
different ruler, the multiple of 15. The detector does not use that property, so it
is independent evidence, and it works down to sub-second scale. If it is ever used
by the detector, an independent replacement check has to exist first.

The BS Fuji ground truth (with slot idents) was checked frame by frame: 3.930 is the
slot ident's first picture, 189.916 the commercial's first, and 324.818 the first of
the programme (the EPISODE 2 eyecatch). The block lengths come out at 134.90 s and
105.005 s, landing on 9×15 and 7×15 within 0.1 s and 0.005 s.

## Logo detection (`logo.rs`)

Silence alone is not enough. A run of commercials ends at the **last seam**, but one
more commercial follows that seam before the programme returns, and that last one
has no seam after it, so it gets missed.

The station logo is on during the programme and gone during commercials, so it tells
you the **range**. Silence supplies the **precise boundary** and the logo the **real
end**; the two complement each other.

**The logo template is learned from the recording itself**, so no per-station logo
images are needed. The logo is the one thing in its corner that never moves, so
averaging a few thousand frames leaves the logo and blurs the background away.
High-pass that and you have the template; the test is a correlation.

Four things mattered in the implementation:

- **Do not pick the "strongest" corner.** Programme captions are usually denser. The
  logo is on throughout the programme, so pick the corner with **the fewest state
  changes**. With that criterion, Nihonkai TV correctly selected the top right
  (strength 26.9) and rejected the programme logo at the bottom right (1110.7).
- **Keep only the largest connected component of the mask.** The logo is one mark;
  scattered survivors are noise, such as the edges of a subtitle box. Adding this
  stopped the correlation wobbling during commercials and fragmenting the regions —
  13 regions down to 4. Pixels count as connected when they are within **three**
  pixels of each other, not only when they touch; see
  [a pale logo is not one blob](#a-pale-logo-is-not-one-blob).
- **Take the threshold from the recording.** Logo density differs by station, but the
  programme occupies most of the running time, so the median score works as the
  representative "logo present" value.
- **Make the hysteresis asymmetric in time, not just in level.** Going absent is
  believed at once, but a return only ends the absence once it has been held for the
  length of the smoothing window. Inside a long break, the correlation occasionally
  grazes the threshold for a frame or two on a commercial that happens to resemble
  the template. Splitting a break there is worse than it sounds: each fragment
  becomes its own block, each edge is snapped to its own nearest junction, and the
  earlier block's end can then pass the later block's start, producing
  **overlapping blocks**. On BS Animax one 302-second break was coming out as three,
  two of which overlapped by 1.6 s.

**A recording with no logo gets the answer "none".** Some stations do not show a
logo continuously. Commercial blocks are always long, so if the absences found are
short and fragmented, the thing being tracked is not a logo, and detection falls back
to silence alone.

| Material | Logo | Result |
|---|---|---|
| Nihonkai TV (logo present, 4 commercial breaks) | Detected top right | **4 blocks, every one an exact multiple of 15 s** (150.0 / 119.8 / 120.2 / 59.9 s) |
| AT-X (no logo) | **Judged not found → silence only** | 1 block (correct) |
| NHK E-Tele (no commercials) | Detected top right, 0 absences | 0 blocks (correct) |

With silence alone, the second and third blocks come out at the odd lengths 104.8 s
and 105.2 s. Adding the logo makes them 119.8 s and 120.2 s, back on the multiple of
15 — evidence that the missing last commercial got filled in.

### A pale logo is not one blob

The detector found nothing at all on a BS recording whose logo is a pale grey
watermark — every corner flipped state around a hundred times and none of them was
believed. The recording has three commercial blocks and a shopping programme at the
end, and silence alone found one of them.

Three things were wrong, and all three were about which pixels the template is drawn
from rather than about any threshold.

**A mark made of thin strokes is not a connected blob.** High-passing the averaged
corner leaves a pale logo as a scatter of one- and two-pixel fragments. The top 500
pixels of that corner fell into **290 clusters, the largest of them 15 pixels** of a
single stroke — and a correlation over 15 pixels is noise, which is why the corner
never read as carrying a logo. Letting a pixel reach **three** pixels instead of one
before the run is traced assembles the strokes into one mark of **156 pixels**
covering the whole logo, while the commercial's own graphics, ten pixels below it,
stay a cluster of their own.

Reach is the whole of the setting, so it was measured on both sides: at four and
five pixels one recording's mask stopped being the logo and grew across the corner
(78 pixels to 245 and then 399), and at one pixel nothing assembles. Three is the
largest value that kept every measured mask on its mark.

**The picture's own edge stands as still as a logo does.** On a recording with
pillar-box bars the mask came out as a **428-pixel line two pixels wide running the
height of the corner** — the step between the bar and the picture. It is the
steadiest thing in the frame and it never goes away, so it read as a logo that is
never absent, and NHK E-Tele got its correct answer for the wrong reason. The mask
is now drawn from four pixels inside the region, and that recording picks its real
logo, top right, instead.

**Two whole marks can be equally steady.** With the strokes assembled, the programme
branding at the bottom left of the terrestrial recording flipped state exactly as
often as the station logo top right — eight times each — and iteration order decided
between them. It chose the branding and put a four-minute "absence" over the
programme. A tie now goes to the corner that is on for more of the recording: the
branding is up for 56% of it against the logo's 76%.

| | Before | After |
|---|---|---|
| BS recording, pale logo | **no logo found**, 1 block from silence | top right, 24 state changes, **5 blocks** |
| Terrestrial, logo + branding | top right (correct) | top right (correct) |
| NHK E-Tele | bottom left — the pillar-box edge | **top right — the logo**, still 0 absences |
| BS Animax × 2 | 3 and 4 absences | the same, on a stronger template (30.1 → 37.6) |
| AT-X (no logo) | none | none |

The five recordings pinned in `tests/run_cm_tests.sh` come out **unchanged, to the
tenth of a second**. What moved is the recording that was getting nothing: its blocks
now sit on the head commercial, a break at 2:05, the break silence already had, and
the shopping programme at the end — all four checked against the pictures.

The cost is about 30 seconds on a 30-minute recording: two passes over the video,
decoding only keyframes, so about an eighth of a full decode. Silence alone is 3
seconds. The GUI offers it as "use the logo too".

## Subtitle resets (`caption.rs`)

Silence and logo are both inferences. Silence only says "somewhere in here", and a
logo's edges lag by the moving-average window. It would be better if the broadcast
itself stamped the seam — and it does.

Japanese broadcasts carry subtitles in an ARIB STD-B24 stream, and that stream
carries a statement that clears the screen and re-declares the display format **every
time the subtitle service restarts**. A seam is exactly that, because a commercial is
not the programme and does not carry subtitles across.

```
CS(0x0C)  →  CSI…SWF  CSI…SDP  CSI…SDF  CSI…SSM  CSI…SHS  CSI…SVS
```

The tell is that **nothing is written afterwards**. A normal line of subtitles looks
the same up to a point, and then positions the cursor and writes characters. That is
the only difference, so testing "does the run after CS end in a sequence of CSIs" is
enough. Testing "starts with CS" alone is not: that fired 395 times on AT-X and 90
times on NHK E, because ordinary subtitle lines clear the screen before writing too.

| Material | Resets | In commercials | In programme |
|---|---|---|---|
| Nihonkai TV | 35 | **35** | **0** |
| BS Fuji (with slot idents) | 14 | 12 | 2 (a slot ident and a promo — the grey area) |
| AT-X | 0 | — | — |
| NHK E (no commercials) | 0 | — | — |
| BS Nittele (anime) | 1 | 0 | 1 (the end of the subtitles themselves; see below) |

All 35 on Nihonkai TV land exactly on the 15-second grid. The largest difference from
the boundaries checked by eye is 0.16 s, and it is always slightly early: the screen
is cleared *for* the cut, not *by* it.

**A recording has to be carrying the stream at all.** A recorder that started before
the programme did has a first program map naming no caption stream, and libavformat
stops probing five megabytes in, so such a recording used to arrive here with no
captions to read resets out of — 7 of 279 captioned recordings measured. Those are
opened a second time with a deeper probe now, and the marks are there to find; see
[the captions the head of the file does not mention](broadcast-ts.md#the-captions-the-head-of-the-file-does-not-mention).

**More stations may omit these marks than emit them.** Three of the five recordings
emit none. So this takes the same shape as the logo detector: if nothing is found it
returns "none" and the caller falls back to silence and logo. Only when resets are
found are they stronger than the other two.

### One reset does not make an emitting station

A BS Nittele anime recording (30 minutes) produced **no commercial blocks at all**,
because one subtitle reset had been found — and finding any at all means neither
silence nor logo gets read. What looked like choosing the stronger method was
choosing to look at nothing.

That one reset is not a seam. This recording's subtitles cover only the first 20
seconds, because they came with a JBA public-service spot, and there is not one line
in the following 30 minutes. The reset at 19.986 marks the end of the subtitles
themselves.

Stations that emit resets emit them at every seam, so the count follows the length of
the recording. The two measured emitting stations gave 13 and 35 in half an hour, the
three non-emitting recordings gave 0, and this recording gave 1. **Fewer than three
now answers "none"** (`caption::MIN_MARKS`). Three is far from either side.

The threshold was added not because the count means something in itself, but because
getting one or two resets only happens when the subtitle service started or ended
somewhere in the recording — which is a fact about the subtitles, not about the
seams.

### When the fallback is not good enough

That fix goes as far as "nothing at all comes out"; whether what does come out is
correct is a separate matter. Checked with thumbnails every 30 seconds, this
recording contains four commercial blocks:

| | By eye | What logo plus silence produced |
|---|---|---|
| Head (JBA spot) | 0 – about 19 | **Nothing** |
| After the OP | about 263 – 327 | **Nothing** |
| Between parts A and B | about 827 – 886 | 825.2 – 858.2 (**28 s left at the end**) |
| End (after the ED) | about 1570 – 1805.8 | 1586.8–1640.2 / 1665.1–1760.1 / 1761.9–1805.2 |

**The logo is weak.** The corner correlation strength is 9.4, a third of Nihonkai
TV's 26.9, because BS Nittele's watermark is a pale grey. The state flips 23 times
and more absences are found, but they fall short of `min_absent`'s 20 seconds and get
thrown away: the 64 seconds that should be 263–327 fragmented, and all of it was
dropped.

**Silence alone does not produce it either.** The ending has eight score-1.00 seams
sitting on the 15-second grid, but the fill ratio does not reach 0.6. The run of
promos at 1640–1685 is chopped into 10 s, 25 s and 20 s, so the grid itself has broken
down. This is not a threshold problem.

Everything beyond here is separate work. Until it is decided how to count an ending
that contains the grey area of promos, there is no target to move `fill` or
`min_absent` towards.

### Why resets work, and what they cost

Silence and logo *guess* at "programme or commercial"; a reset reads the mark the
broadcaster's own equipment stamped on the seam. And it needs no decoding: select a
PID and read packets, 3 seconds for a 3.7 GB recording, against 30 for the logo.

There are two costs.

- **It discards cases where the logo would be right.** On BS Fuji the logo swings to
  "present" during commercials, because white-on-dark phone numbers in the corner
  raise the correlation, so discarding the logo was a win for that recording. It
  could go the other way on another station.
- **The end of a recording cannot be closed up.** BS Fuji's final block ends at the
  last reset, 1683.9, but the recording continues for another 3.4 s and that is still
  commercial. Resets alone cannot tell the difference: Nihonkai TV has the same
  shape, with the last reset 2 s before the end, and what follows there is programme.
  Extending the block would shave 1.7 s off that one. Telling the two cases apart
  means looking at the logo, which costs 30 seconds of decoding for 3.4 seconds of
  commercial, so it is not worth paying. A miss is the cheap error.

## Tried and dropped: dividing the recording in one decision

Everything above decides locally and in a fixed order of preference — resets if the
recording has any, otherwise the logo, otherwise the silences, each with thresholds
of its own. Whatever comes second is never read, and a threshold cannot be argued
with by evidence sitting either side of it. So the readings were put into one
objective instead: every reading offers boundaries (the middle of a silence, a reset,
an edge of a logo absence), every division of the recording into alternating
stretches of programme and commercial is scored by how well it explains all of them
at once, and the best division is found exactly, with a segmental dynamic program
over those boundaries.

It was implemented, measured, and taken back out. What follows is why, because the
idea is sound enough that someone will have it again.

**It won where a logo or a reset exists.** All five pinned recordings passed. BS
Fuji's leftover commercial went from 3.4 s to none. On the recording with the pale
logo the middle break came out at exactly 60.0 s against 64.9 s. Two recordings
outside the pinned set were checked by eye where the two answers differed, and the
one decision was right both times: a BS Animax break ended at 765 s, where the
programme resumes, rather than six seconds into it, and a Kids Station recording
that carries only four resets — so the preference order read those and stopped,
keeping three minutes of trailers and an infomercial — got the rest of its blocks.

**A survey of thirty-five recordings across twenty stations said no.** Both ways were
run off one reading of each file. The one decision found more: 120 blocks against
110, 11930 s of commercial against 10792, with 1653 s that only it called commercial
against 515 s the other way. But on the independent ruler — a block's length being a
whole number of fifteen-second units — it was **worse**: 52.0% of 75 blocks against
61.1% of 72.

**What the ruler was pointing at.** On an AT-X recording with no logo and no caption
resets, thumbnails every thirty seconds across the whole recording showed that four
of the five blocks it emitted — 173 seconds — were **programme**. Only the last one,
the trailers at the end, was real. The preference order emitted nothing there: it
missed some 330 s of trailers, and cut nothing. Six of the thirty-five recordings
have the silences as their only reading, and that is where the risk lives. The
thresholds the preference order applies to a silence-only recording — a score, a
chain, at least three junctions, a filled grid — exist for exactly this case, and
the one objective dissolved them into something a talkative programme can outvote.

Three things about the model were right, and are worth keeping for whoever tries
again:

- **A stretch of commercial has to cost something before the evidence starts.** The
  junction term can only add, so without a standing cost "all commercial" wins on a
  recording that has none.
- **Where the logo is up, the junctions do not get a vote.** The logo is an
  observation of which of the two a stretch is; the grid is circumstantial.
- **Count the grid by boundary, not by junction.** A talkative programme pauses
  several times inside one fifteen-second unit, which counts as more than a full
  grid and swallowed 41 seconds of programme before it was fixed.

To revisit it, the thing to build is not a better weight but a rule for the case that
broke it: what a break has to look like when the silences are all there is. That can
be measured on those six recordings and the pinned five without rebuilding any of
this.

The survey paid for itself on the way: one recording's silence walk **panicked** on a
frame claiming a ninth audio plane, which an `AVFrame` has no pointer for. That is
fixed, and is not part of what was dropped.
