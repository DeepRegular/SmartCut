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

### How often it lands on one, and what a wider window would cost

The report behind this was marks sitting mid-shot: "not an entry picture, not a
scene change". `examples/cutdiag.rs` measures it — run the detection over a
recording and print, for every boundary it emits, what the window around that
boundary actually holds. Thirty BS Nittele and BS Asahi recordings, every one of
them read by logo and silence:

| | |
|---|---|
| Put on the frame a cut happens on | **87%**, median shift 0.088 s, worst 0.49 s |
| No cut within half a second of them | 13% |

A cut stands within 2.5 s of most of the second group, so a wider window would
move them. Measured over the blocks that sit wholly inside a recording, it buys
nothing and then costs:

| Window | A whole number of 15-second units | ...of 5-second units |
|---|---|---|
| ±0.5 s | 47/66 | 52/66 |
| ±1.0 s | 47/66 | 52/66 |
| ±1.5 s | 47/66 | 52/66 |
| ±2.5 s | 45/66 | 50/66 |

The boundaries that stay put are on the grid already. Their estimate is right and
the junction there is simply not a change of picture — a fade, or two commercials
that look alike — and dragging them onto the nearest cut takes them off it.

**And a BS anime slot is not sold only in fifteens.** Of those same blocks, 47 are a
whole number of 15-second units and 52 a whole number of 5: the ones that are not
run 65.97, 70.10, 85.39 and 115.01 seconds, which are exact multiples of five and
read as a sixty- or ninety-second break with a five-second sponsor card on the end.
Measured against the fifteen alone a block like that looks wrong when it is not, and
the ruler is worth the more careful reading.

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
is independent evidence, and it works down to sub-second scale. Across the three
recordings that contain commercials, every block comes out exact: 150.0 / 120.0 /
120.0 / 60.0 s, 135.0 / 105.0 s, and 300.0 s. If the property is ever used by the
detector, an independent replacement check has to exist first.

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

### An absence begins before the average says it has

The report was a keyframe sitting in the middle of a mid-programme commercial
block, on BS Nittele and BS Asahi. It was eleven seconds into a sixty-second
break, and the cause is in the two thresholds.

A moment is scored on an average of the seconds behind it, so a step down in the
corner reaches the *lower* of the two thresholds only once most of that window
is inside the break. On a recording with a pale grey watermark that was seven
seconds late. A break's start is then pulled onto the nearest silence within six
seconds of the logo's edge, which from seven seconds in does not reach the
junction at the head of the break — and finds instead the junction between the
first commercial and the second.

The end of an absence was already read the other way: it ends where the logo
returned, not where the return was confirmed, the wait being only to establish
that the return was one. The start now mirrors it — the first sample under the
*present* threshold, confirmed by the absent one.

| | Before | After | By eye |
|---|---|---|---|
| Mid-programme break | 924.1 – 973.2 (48.8 s) | **913.1 – 973.2 (60.0 s)** | 912.9 – 973 |
| The break after it | 1588.2 – 1640.2 (52.0 s) | **1580.1 – 1640.2 (60.0 s)** | |

**Reading the edge earlier is not enough on its own.** Two silences frequently
stand within reach of it — the one at the head of the break and the one between
its first commercial and its second — and which of them is nearer is a coin toss
the lag decides. Over thirty recordings the correction put two blocks right and
two wrong, and the ruler moved by one.

A break begins where the picture is replaced, so a silence that no cut stands on
is a pause in a programme and is passed over. That settled both of the two it
had got wrong and kept both of the two it had put right. The junctions are asked
about only where the recording has been walked, and only those a start could be
snapped to: each question is a seek and a second of decoding, and a half-hour
recording carries forty that no edge is near.

| Thirty BS Nittele and BS Asahi recordings | Before | After |
|---|---|---|
| Boundaries on the frame a cut happens on | 180 (87%) | 182 (88%) |
| Blocks a whole number of 15-second units | 47/66 | **50/66** |
| ...of 5-second units | 52/66 | **55/66** |

Four recordings changed and none for the worse. The five pinned recordings are
unchanged.

What this costs is whatever the recording can be read at. The two passes decode
only entry pictures and spread them over the machine, so a 30-minute recording
held in memory is 1.5 seconds of them; the same recording off the disc is 46,
and 42 of that is reading 3.7 GB twice. Silence alone is 3 seconds, and comes out
of the same read as the caption stream.

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

### A station that marks only where the programme stops and starts

The preference order reads the resets *instead of* the logo and the silences rather
than beside them, which is right where the marks are a reading of the whole
recording. On one BS station they are not. It marks the two ends of each break and
nothing between, so half an hour of anime carries six or seven marks where a marking
station carries twenty-six. Read instead of the other two, six marks produced one
break on a recording that has four, and the logo -- strong, and right about all four
-- was never looked at.

**What tells the two kinds of station apart is a mark inside a break**: two resets one
15-second unit apart, which is one commercial ending and the next beginning. Both
kinds mark the ends, so the ends say nothing.

| | Adjacent pairs, per half-hour recording |
|---|---|
| Three stations that mark throughout (24 recordings) | 5 – 30 |
| The station that marks only the ends (26 recordings) | 0 – 2 |

`marks_every_junction` asks for three of them (`RESET_INTERIOR`), the middle of that
gap. Below it the marks are dropped and the caller falls back exactly as it does for
a recording that carries none. Nothing else measured was as clean: the count of marks
separates as well (5–10 against 18–35) but has to be read against the recording's
length, and the longest run of marks separates with a margin of one.

Two recordings were then checked against the pictures. Both gained the two blocks the
marks are silent about -- the spot at the head, and the promos and shopping at the end
-- which is 221 s and 231 s of commercial that had been left in, and neither lost
anything.

### A break longer than two minutes, marked only at its ends

The grid that chains silences allows neighbours up to eight units -- two minutes --
apart, because a chain forming by accident is the whole of what that reading defends
against. The resets were held to the same eight. On a station that marks only the
ends, the whole break is one gap.

Measured over a season of one channel's anime slots -- 68 recordings, 26 carrying
three marks or more -- raising the limit added **twenty blocks, every one of them
135 s (nine units) or 150 s (ten)**. Nothing appeared at eleven units or beyond, at
any limit tried up to twenty: those two lengths are what that channel's mid-programme
break is, and the programme between two breaks runs to twenty-five units and more.
`RESET_UNITS` is twelve, three minutes.

A station that marks every junction is untouched. The 134.9-second break pinned in
the suite is nine units too and was never at risk: its marks are one unit apart, so
the break is a chain of ones.

### A mark from before the clock started again

A transport stream carries its times on a 33-bit 90 kHz clock, which starts again
every twenty-six hours. One recording of the fifty crossed that point, and the mark
after the crossing came back as -93848.7 s. Sorted, it went to the front of the list,
where a lone mark is read as a break running from the start of the recording to it --
a block that ends twenty-six hours before it begins.

It was a real junction. Brought back by one whole wrap it lands at 1595.0 in a
recording of 1807.1, which is where that programme ends and the promos begin; the
logo, read separately, puts the same boundary at 1599.3. `resets_with` now adds whole
wraps until a mark is no longer in the past, and drops what still does not fit inside
the recording. Only that direction is mended: the other way round would mean the
recording's own start time had been read past a wrap its packets had not, which was
not measured, and a mark with no reading is better dropped than guessed at.

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
PID and read packets, 3 seconds for a 3.7 GB recording, against two more reads of
the whole of it for the logo.
Where the resets are used the logo pass is skipped outright: on the Nihonkai TV recording that took the whole analysis from 50
seconds to 7.

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
  means looking at the logo, which costs two more reads of the recording for 3.4 seconds of
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

## Tried and dropped: trusting the silences where the logo stays up

A report showed marks landing in the middle of commercial breaks on a BS Nittele
anime slot, five episodes of one half-hour series. Thumbnails every ten
seconds gave the truth: two breaks inside the programme, 150 s each, plus the
usual ones at the head and tail. The detection found the head and the tail, but
none of the ten inner breaks whole: five came out cut short or split in two, and
five not at all.

**The cause is the logo.** The channel fills those breaks with its own trailers
and a teleshopping minute, and keeps its logo up over them because they are its
own programming. The logo went only for the sponsors' spots. A recording with a
logo takes its blocks from the logo's absences alone, so a break that is mostly
trailers is a fragment or nothing, and a mark on a fragment's edge sits in the
middle of the break.

The silences said where the breaks were every time: a silence at each junction,
on the 15-second grid, 1.0 to 1.9 s long. They were not used
for two reasons. In logo mode silence chains are not consulted at all, and on
their own these chains fail the fill (`min_fill`, 0.6): minute-long trailers put
five junctions on eleven boundaries, 0.36 to 0.45.

So three rules were added and measured:

- a chain of four or more junctions whose silences are all 0.9 s or longer
  stands as a break whatever its fill (a pause in dialogue is 0.1 to 0.4 s);
- a chain that fails whole is retried with its ends trimmed, where the trimmed
  stretch is all such silences (one episode's break had a programme pause
  chained on 90 s ahead of it, which halved the fill and lost the break);
- in logo mode, a chain that stands widens the logo block it meets, or is a
  block where it meets none, except at either end of the recording.

On the five episodes it worked: nine of the ten inner breaks came out right to
the second, and the tenth lacked only its last spot.

**On the wider set it did more harm than good.** 44 recordings of the 67 in
the survey had been compared when it was stopped. Twenty changed, every change
an addition, and every added stretch was checked against thumbnails. Nine were
real commercials (trailer blocks on TBS Channel 2 and BS Nittele, a
teleshopping tail). Sixteen were programme:

- **closing titles, 90 s**, taken into the break after them, four times;
- **opening titles**, taken into the break before or after, three times;
- **a minute or more of the story itself**, next to a correct block, six times.
- **shorter pieces**, a next-episode trailer and the like, three times.

Anime openings and endings are timed by the station as exactly as a spot is,
90 s to the frame, and the story's own scene changes put a second of silence on
the grid more often than expected. Structurally the good widenings and the bad
ones are the same thing: a correct block reaching one or two junctions outward
across 60, 90 or 120 seconds. One episode's break correctly reached back across
a two-minute teleshopping spot; another recording's reached back across a
90-second opening. Silence length, spacing and the logo do not separate them.

The rules were taken back out, and the behaviour is as it was. To solve this the
detection needs a reading that tells programme from trailer rather than a better
threshold. The promising one is still matching a recording against other
episodes of the same series: the opening and ending recur every week in the
same place, and the breaks' contents do not.

## Showing the silences themselves (`blank.rs` and `cm.rs`)

The cut editor's `≡` menu has two detections beside this one: **Detect black**
and **Detect silence**. The sound pass is this one -- `find_silences` is what
both call -- and the difference is what is done with the answer: nothing is
ranked, nothing is fitted to a 15-second grid, and the stretches are reported
as they were found.

Which shades the pictures pass looks for is a preference, black alone out of
the box, and the line is named after that answer: *Detect black*, *Detect
white*, *Detect blank*. A shade that was not looked for was not written down
either, so asking for white afterwards reads the recording again.

The flat pictures are `blank.rs`, which decodes every picture rather than the
entry ones. Broadcast black runs two to four pictures, and a sample every half
second misses it. Moving the black threshold across luma 12 to 40 (of 255) does
not change how many stretches are found; it changes only how much of a fade is
called black. So both ends of a stretch are reported and neither is named the
junction.

The two levels are fractions of the way from studio black up to studio white:
16 to 235 at 8 bits and the same values shifted up at higher depths, 64 to 940
at 10. So 0% is black itself, and the defaults, 0.04 and 0.99, are luma 24 and
232. Two scales were tried first and both went wrong. Taken as a fraction of
0..255, the bottom six percent of the scale was under broadcast black and found
nothing. Taken against the largest sample the depth can hold, which goes 255 to
1023 rather than being shifted, a 10-bit flash to white was never found: 940 out
of 1023 is 0.919, under the 0.922 that 235 out of 255 clears. A recorder's 4K is
10-bit HEVC, which is where this was missing them. The span is the studio one
whatever range a picture says it is in, so a full-range picture's black and
white fall outside it and are still found.
