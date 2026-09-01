# Commercial boundary detection (`cm.rs` / `logo.rs` / `caption.rs`)

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](cm-detection.ja.md)

Japanese broadcasters lay commercials out in 15-second units and put a short
silence at every seam. Two things distinguish that from a pause inside the
programme:

- **The silence is long** — about 1 second at a seam, 0.1–0.4 s for a pause in
  the programme
- **It lands on a 15-second grid** — the neighbouring seam is exactly 15/30/60
  seconds away

Neither is decisive on its own, so what comes out is a *candidate*, not a
verdict. Silence length and the length of the run give a score, and the run is
grouped into a commercial block.

There are three ways to read it: **silence** (`cm.rs`), **the station logo**
(`logo.rs`), and **resets of the subtitle service** (`caption.rs`). Only the last
of these is a real event rather than an inference, so it is used on recordings
that carry it, and the other two are the fallback for recordings that do not.
Stations divide sharply into those that emit it and those that do not, so all
three are needed.

## Results on real material

| Material | Method used | Programme wrongly cut | Commercial left in |
|---|---|---|---|
| Nihonkai TV variety show (30 min) | Subtitle resets | 0.2 s | 0.1 s |
| AT-X anime (30 min, no logo) | Silence only | 0.0 s | 0.1 s |
| BS Fuji anime (28 min, with slot idents) | Subtitle resets | 0.1 s | 3.4 s |
| BS Fuji anime (24 min, no commercials) | Logo plus silence | 0.0 s | 0.0 s |
| NHK E-Tele (6 min, no commercials) | Logo plus silence | 0.0 s | 0.0 s |

**The two errors are measured separately.** With a single number like "98 %
accurate", the expensive error hides inside an improvement to the cheap one.
Emitting an interval deletes what is inside it, so seconds of programme swallowed
and seconds of commercial left behind do not cost the same.

All three recordings with commercials produce block lengths that are **exact
multiples of 15 seconds** (150.0 / 120.0 / 120.0 / 60.0, 135.0 / 105.0, 300.0).
The detector does not use that property at all, which makes it independent
evidence that the boundaries landed on real cuts.

## What came out of trying to improve accuracy

It started as "reduce the misses", but building a ground truth by eye showed the
problem was **the other way round**. The **five minutes of "commercial block"**
being emitted on the BS Fuji recording **were programme from end to end**.

The ground truth came from laying out thumbnails every 30 seconds across the
whole recording and checking by eye. A detector's output is not evidence about
the detector.

| Material | Actually | Before | After |
|---|---|---|---|
| Terrestrial Nihonkai TV | 4 s of commercial at the head plus 4 commercial blocks | 4 (the head missed) | 5 (correct) |
| AT-X | 5 minutes of commercial at the end | 1 (correct) | 1 (correct) |
| **BS Fuji** | **No commercials** | **1 (a 5-minute false positive)** | **0** |
| NHK E | No commercials | 0 | 0 |

**A false positive costs more than a miss.** An emitted interval gets cut as it
stands, so getting it wrong deletes five minutes of programme. A miss is
something a person notices on sight.

### Ideas tried and dropped

Measured before implementing the hunch. Neither worked:

- **Snapping to scene changes.** The centre of a silence is a coarse position for
  a cut (it wanders by the length of the silence), so pulling it towards a scene
  change inside the silence ought to sharpen the 15-second grid. The median
  improved but **the worst case got worse** (on Nihonkai TV, neighbours landing
  within ±0.4 s of the grid went from 27/29 to 22/29). Commercials contain scene
  changes too, and it snaps to the wrong one.
- **Scene change density.** Commercials should cut faster than the programme. The
  measured ratios: 3.13× on terrestrial, 1.58× on AT-X, **1.08× on BS Fuji** —
  anime cuts plenty fast.

### The commercial at the head of a recording

This came from a report that the leading commercial was never removed. Logo
absences **shorter than 20 seconds are ignored**, because no commercial block is
shorter than that. But the head of a recording is different: those few seconds
are just **the recorder starting before the programme did**, not a block.

The 3.5 s from `00:00:00.665` to `00:00:04.169` was exactly that, and it fell
below the threshold and was thrown away. An absence touching either end is now
treated as an **edge, not a break**, and one second is enough to pick it up.
Where the logo first appears is the head of the programme — that is all it was.

### Placing boundaries to the frame

A block's times are only estimates — the centre of a silence, or the moment a
moving average of logo strength crossed a threshold. Neither is a picture, and in
practice they are off by 0.2–0.33 s. The real boundary is **the change between
two consecutive pictures**, so that window is decoded and searched
(`thumbs::cut_near()`).

This was got wrong once. The first attempt snapped to **the nearest mark in the
scene index**, but that index is built from key pictures every 0.5 s, which is
coarse, and commercials contain cuts too. The second attempt took **the largest
difference within the window**, and since a cut inside a commercial is more
dramatic than the cut at its edge, that dragged the boundary a full second into
the commercial. The right answer is **the nearest cut** — the incoming estimate
is close to begin with, so all that was left to decide was which picture the
change happens on. The window is ±0.5 s.

The fix proved itself. The four Nihonkai TV blocks went from **119.8 / 120.2 /
59.9 s to 120.0 / 120.0 / 60.0 s** — commercials are sold in 15-second units, so
only when the boundaries land on real cuts do the lengths become **exact
multiples**. The detector makes no use of that property, so it is independent
evidence. `tests/run_cm_tests.sh` checks the multiple-of-15 property too.

The leading commercial's boundary was checked to the frame as well: 3.80 s is
still commercial (a picture of a can), and **3.835 s is the programme's first
picture**. The mark is there.

### The two things that worked

- **Take the grid tolerance from the silence itself.** A seam's time is
  represented by the centre of the silence, but the actual cut may be anywhere
  inside it. In other words **a long silence is ambiguous about where the seam
  is**, and a fixed ±0.4 s was too narrow for terrestrial recordings with 1.4 s
  silences and too wide for BS with 0.5 s ones. The tolerance became "the mean of
  the two silence lengths (capped at 0.6 s) plus 0.15 s".
- **Require a block's 15-second boundaries to be filled.** A run of commercials
  has a silence every 15 seconds, because that is where one commercial ends.
  Measured, correct blocks have **81–100 %** of their boundaries filled while
  false positives have only **23–43 %**. A threshold of 0.6 separates them
  cleanly.

The latter works because **a talkative programme already has roughly one silence
every 15 seconds**. The BS Fuji recording has 91 silences in 1440 seconds, one
every 16 s, so landing on the grid carries almost no information by itself. What
had to be checked was not "it lands on the grid" but "**it lands on the grid with
no gaps**".

## The ground truth, and how it is scored

The ground truth for five recordings is pinned in `tests/run_cm_tests.sh`. The
two with no commercials are the more valuable ones, because they are the tests
guarding the side that must not emit anything. On a machine without the material
they **fail rather than SKIP**: a run that checked nothing must not read as
"passed".

Scoring is `tests/cm_score.py`. Not block counts and ±2 s, but **two numbers, in
seconds**:

| | |
|---|---|
| Programme wrongly cut | Programme swallowed by a block. Gone the moment you cut |
| Commercial left in | Commercial left outside the blocks. Annoying, but visible |

Each gets its own budget. Combine them and the expensive error hides inside an
improvement to the cheap one.

**Some seconds are neither.** Slot idents, programme promos, sponsor credits —
material the broadcaster places around the seams — are a matter of viewer taste,
not a fact about the recording. Forcing a decision makes the score lie in the
direction you forced, so they are marked **grey** (`~START-END`) and excluded from
both counts.

The seconds budget catches large mistakes; boundary precision is measured by a
different ruler — **the multiple of 15**. The detector does not use it, so it is
independent, and it works down to sub-second scale. If it ever starts being used,
an independent replacement check has to exist first.

The ground truth for BS Fuji (with slot idents) was checked frame by frame:
3.930 is the slot ident's first picture, 189.916 the commercial's first, 324.818
the first of the programme (the EPISODE 2 eyecatch). The block lengths come out
at 134.90 s and 105.005 s, landing on 9×15 and 7×15 within 0.1 s and 0.005 s.

## Adding logo detection (`logo.rs`)

Silence alone is not enough. A run ends at the **last seam**, but one more
commercial follows it before the programme returns. That last one has no seam
after it, so it is missed.

The station logo is on during the programme and gone during commercials, so it
tells you the **range**. Silence supplies the **precise boundary** and the logo
the **real end**; they complement each other.

**The logo template is learned from the recording itself.** No per-station logo
images are needed. The logo is the one thing in its corner that never moves, so
averaging a few thousand frames leaves the logo and blurs the background away.
High-pass that and you have the template; the test is just a correlation.

Four things mattered in the implementation:

- **Do not pick the "strongest" corner.** Programme captions are usually denser.
  The logo is on throughout the programme, so pick the corner with **the fewest
  state changes**. With that criterion, Nihonkai TV correctly selected the top
  right (strength 26.9) and rejected the programme logo at the bottom right
  (strength 1110.7).
- **Keep only the largest connected component of the mask.** The logo is one
  blob; scattered survivors are noise such as the edges of a subtitle box. Adding
  this stopped the correlation wobbling during commercials and fragmenting the
  regions (13 regions down to 4).
- **Take the threshold from the recording.** Logo density differs by station, but
  the programme occupies most of the running time, so the median score works as
  the representative "logo present" value.
- **Make the hysteresis asymmetric in time, not just in level.** Going absent is
  believed at once, but a return only ends the absence once it has been held for
  the length of the smoothing window. Inside a long break the correlation
  occasionally grazes the threshold for a frame or two on a commercial that
  happens to resemble the template. Splitting a break there is worse than it
  sounds: each fragment becomes its own block, each edge is snapped to its own
  nearest junction, and the earlier block's end can then pass the later block's
  start — **overlapping blocks**. On BS Animax one 302-second break was coming
  out as three, two of which overlapped by 1.6 s.

**A recording with no logo gets the answer "none".** Some stations do not show a
logo continuously. Commercial blocks are always long, so if the absences found
are short and fragmented, the thing being tracked is not a logo, and it falls
back to silence only.

| Material | Logo | Result |
|---|---|---|
| Nihonkai TV (logo present, 4 commercial breaks) | Detected top right | **4 blocks, every one an exact multiple of 15 s** (150.0 / 119.8 / 120.2 / 59.9 s) |
| AT-X (no logo) | **Judged not found → silence only** | 1 block (correct) |
| NHK E-Tele (no commercials) | Detected bottom left, 0 absences | 0 blocks (correct) |

With silence alone the second and third blocks come out at the odd lengths
104.8 s / 105.2 s. Adding the logo makes them 119.8 s / 120.2 s, back on the
multiple of 15 — evidence that the missing last commercial got filled in.

The cost is about 30 seconds on a 30-minute recording (two passes over the video;
only keyframes are decoded, so an eighth of a full decode). Silence alone is 3
seconds. The GUI offers it as "use the logo too".

**On recordings where subtitle resets are found, the logo is not read.** That
method is both stronger and ten times faster, so it is skipped even with "use the
logo too" ticked.

## Adding subtitle resets (`caption.rs`)

Silence and logo are both **inferences**. Silence only says "somewhere in here",
and a logo's edges lag by the moving-average window. If the broadcast itself
stamps the seam, that is better. It does.

Japanese broadcasts carry subtitles in an ARIB STD-B24 stream, and that stream
carries a statement that "clears the screen and re-declares the display format"
**every time the subtitle service restarts**. A seam is exactly that, because a
commercial is not the programme and does not carry subtitles across.

```
CS(0x0C)  →  CSI…SWF  CSI…SDP  CSI…SDF  CSI…SSM  CSI…SHS  CSI…SVS
```

The tell is that **nothing is written afterwards**. A normal line of subtitles
looks the same up to a point, then positions the cursor and writes characters.
That is the only difference, so testing "does the run after CS end in a sequence
of CSIs" is enough. "Starts with CS" alone is not: that fired 395 times on AT-X
and 90 times on NHK E, because ordinary subtitle lines clear the screen before
writing too.

| Material | Resets | In commercials | In programme |
|---|---|---|---|
| Nihonkai TV | 35 | **35** | **0** |
| BS Fuji (with slot idents) | 14 | 12 | 2 (both a slot ident and a promo, i.e. the grey area) |
| AT-X | 0 | — | — |
| NHK E (no commercials) | 0 | — | — |
| BS Nittele (anime) | 1 | 0 | 1 (the end of the subtitles themselves; see below) |

All 35 on Nihonkai TV land **exactly on the 15-second grid**. The largest
difference from the boundaries checked by eye is 0.16 s, and it is **always
slightly early** — the screen is cleared *for* the cut, not *by* it.

**More stations may omit it than emit it.** Three of the five recordings emit
none. So it takes the same shape as `NoLogo`: if nothing is found it returns
`NoResets` and the caller falls back to silence and logo. **Only when it is
found is it stronger than the other two.**

### One reset does not make an emitting station

A BS Nittele anime recording (30 minutes) produced **no commercial blocks at
all**, because **one** subtitle reset had been found and finding any means
neither silence nor logo gets read. What looked like choosing the stronger
method was choosing to look at nothing.

That one reset is not a seam. This recording's subtitles cover **only the first
20 seconds** — they came with a JBA public-service spot — and there is not one
line in the following 30 minutes. The reset at 19.986 marks **the end of the
subtitles themselves**, not a commercial boundary.

Stations that emit resets **emit them at every seam**, so the count follows the
length of the recording. The two measured stations gave 13 and 35 in half an
hour, the three non-emitting recordings gave 0, and this recording gave 1.
**Fewer than three now answers "none"** (`caption::MIN_MARKS`). Three is far from
either side.

The threshold was added not because counting means something, but because
**getting one or two resets only happens when the subtitle service started or
ended somewhere in the recording**, which is a fact about subtitles, not about
seams.

#### The fallback is not good enough yet

The fix goes as far as "nothing at all comes out"; **whether what comes out is
correct is a separate matter**. Checked with thumbnails every 30 seconds, this
recording contains four commercial blocks:

| | By eye | What logo plus silence produced |
|---|---|---|
| Head (JBA spot) | 0 – about 19 | **Nothing** |
| After the OP | about 263 – 327 | **Nothing** |
| Between parts A and B | about 827 – 886 | 825.2 – 858.2 (**28 s left at the end**) |
| End (after the ED) | about 1570 – 1805.8 | 1586.8–1640.2 / 1665.1–1760.1 / 1761.9–1805.2 |

**The logo is weak.** The corner correlation strength is 9.4, a third of Nihonkai
TV's 26.9 (BS Nittele's watermark is a pale grey). The state flips 23 times and
more absences are being found, but they fall short of `min_absent`'s 20 seconds
and get thrown away — the 64 seconds that should be 263–327 fragmented and all of
it was dropped.

**Silence alone does not produce it either.** The ending has eight score-1.00
seams sitting on the 15-second grid, but `fill` does not reach 0.6. The run of
promos at 1640–1685 is chopped into 10 s, 25 s and 20 s, so **the grid itself has
broken down**; this is not a threshold problem.

Which makes everything beyond here separate work. Until it is decided how to
count an ending that contains the grey area (promos), there is no target to move
`fill` or `min_absent` towards.

### Why it works, and what it costs

Silence and logo *guess* at "programme or commercial"; this reads the mark the
broadcaster's own equipment stamped on the seam. And **it needs no decoding** —
select a PID and read packets, 3 seconds for a 3.7 GB recording. The logo takes
30. On recordings where resets are found neither logo nor audio is read, which
took Nihonkai TV's analysis **from 50 seconds to 7**.

There are two costs.

- **It discards cases where the logo would be right.** On BS Fuji the logo swings
  to "present" during commercials (white-on-dark phone numbers in the corner raise
  the correlation), so for that recording discarding the logo was a win. It could
  go the other way on another station.
- **The end of a recording cannot be closed up.** BS Fuji's final block ends at
  the last reset, 1683.9, but the recording continues for another 3.4 s and that
  is still commercial. Resets alone cannot tell — Nihonkai TV has the same shape
  (last reset 2 s before the end) and what follows there is **programme**.
  Extending would shave 1.7 s off that one. Separating the two means looking at
  the logo, which costs 30 seconds of decoding for 3.4 seconds of commercial, so
  it is not paid. A miss is the cheap error.

## In the GUI it takes two clicks

"Detect commercials" → "Keep everything but the commercials". The boundaries are
**snapped to an access point within ±0.5 s** before becoming intervals. On a
30-minute commercial-broadcast recording, 22.6 minutes of programme remain as
five intervals, **100.0 % lossless copy**. For a single-interval example:

```
output 1506.005s — lossless copy 1505.971s (100.0%) / re-encoded 0.033s (0.0%)
```

The seam sits in the middle of about a second of silence, so moving it a few
hundred milliseconds is inaudible. That small concession makes **the whole
commercial cut lossless**.
