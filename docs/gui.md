# GUI (`gui/`)

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](gui.ja.md)

Tauri v2 plus vanilla JS. It embeds the core as a crate, and the engine barely
needed to change (the first two additions were extracting a single frame and
reporting progress; the thumbnail track and the proxy came later).

```
[Cut editor] full_ntv.ts
Lossless points: 3607  1440x1080  29.97 fps  interlaced (TFF)  audio  [x logo] [Detect commercials]
┌──────────────┬─────────────────────────────────────────────┐
│ Keyframes: 9 │                                             │
│ ┌──┐#01      │                  Preview                    │
│ │▤ │00:00    │                                             │
│ └──┘00.00    │           20   00:00:00.66                  │ <- big frame number and TC
│ ┌──┐#02 ✕    │      I frame — lossless point  sel 0-54111   │
│ │▤ │09:49    │                                             │
│ └──┘49.75    ├─────────────────────────────────────────────┤
│ ┌──┐#03 ✕    │ [GOP][GOP][GOP]▼[GOP][GOP][GO]▒▒  view GOP·6s│ <- one cell = one GOP,
│ │▤ │12:19    │  34.55 35.05 35.55 ┊ 36.06 36.56 37.06      │    the mark stays centred
│ └──┘19.90    ├─────────────────────────────────────────────┤
│    …         │   ▼      ▼        ▼   <- keyframes           │
│              │ ┌─────────────────────────────────────────┐ │
│              │ │███████ green = the output itself ██│███ ●│ │ <- playhead
│              │ └────────────────────────────────────┴────┘ │
│              │                      ↑ red line = a cut seam │
│              │  ◤[                             ]◥  <- IN/OUT│
│              │  ▏▏▏ ▏ ▏▏▏▏▏▏  <- scene changes              │
│              ├─────────────────────────────────────────────┤
│              │ ⚑Keyframe ⇤Scene Scene⇥ │ |◀ ◀| ◀ ⇤| [IN ✂Cut OUT] |⇥ ▶ |▶ ▶| │ Cut outside  Snap to lossless  ↺Undo  Clear all │
│              │ 20 / 54111  00:00:00.66   selection 0 - 54111 : 00:30:05.50 │
│              ├─────────────────────────────────────────────┤
│              │ █████████████████ blue = copy / orange = re-encode │
│              │ 20 / 44766 …  <- the counter counts the output too │
│              │ output 00:24:53.02 (2 intervals, 1 cut) — lossless copy 100.0% │
│              │ [Export] □sample-accurate audio [video fully lossless] ──── │
└──────────────┴─────────────────────────────────────────────┘
```

**The screen layout follows TMPGEnc MPEG Smart Renderer 6's cut editor.** What
was borrowed is not the look but **the movements of the hand**, and above all not
mixing two ideas together:

- A **keyframe** is a *mark*, not an edit. They line up on the left with a
  picture, and pressing one jumps there. `⚑ Keyframe` (or `K`) registers the
  current frame.
- A **cut** is the edit. Select with IN and OUT, press `✂ Cut` in the middle, and
  that range disappears from the output. **What comes out is "the whole recording
  minus the cuts"**, so immediately after opening a file nothing is cut — the
  whole thing is the output. That is why the selection covers the whole file at
  load time too.

**What is on screen is the edited timeline, not the original recording.** A cut
region does not go grey, it **disappears** — the seek bar shrinks, the filmstrip
closes over the hole, and the frame counter counts **the length that will be
written** (cut 9345 frames out of a 54111-frame recording and it becomes 44766 on
the spot). All that remains is a red vertical line marking the seam.

So there are two kinds of time: **output time** and **source time**. Every number
and coordinate on screen is output time; `cuts`, keyframes, scenes and the
arguments to the engine are all source time; and exactly two functions,
`outToSrc()` and `srcToOut()`, stand on the border. **A time that has fallen
inside a cut gets "no such time" from `srcToOut()`** — the keyframe leaves the
list (undo the cut and it comes back) and the playhead moves to the seam. The
arrow keys move in output time, which is the same mechanism that makes stepping
across a cut point jump automatically.

Once that split was made, the button layout became obvious too. **Cut is in the
middle, `[ IN` to its left and `OUT ]` to its right, and outward from there,
symmetrically: "go to that point", "one frame", "lossless point", "start/end".**
Which side an action belongs to is readable from where it is.

- `Cut outside` — drop everything outside the selection. One press for extracting
  a single region.
- `Snap to lossless` — pull both ends of the selection to the nearest access
  point. Press it and re-encoding goes to zero.
- `↺ Undo` — undo the last cut (50 levels). `Clear all` clears both cuts and
  keyframes.

**IN to OUT includes the OUT frame.** "I put OUT where I wanted the deletion to
end and one frame survived" is not a thing that should be sayable. Select five
frames and five frames go.

**Setting one end does not move the other.** IN and OUT are placed one end at a
time as you close in, so re-placing IN is no reason to lose OUT. Only when the
two cross does the one just placed win, and the other retreats to the end of the
timeline — "from here on" and "up to here" is the honest reading until the range
is narrowed.

**A seam left by a cut becomes a keyframe.** That is exactly what you will want
to check afterwards, and it works the same for commercial removal and for manual
cuts. **The same applies when the head of the recording is cut.** Removing the
head leaves only one interval, so no "between two intervals" is created, but
**the new start is a seam in every sense** and it needs a mark too.

**Seam cells decode the actual frame at that point.** What is retained are key
pictures, so the nearest retained image to a seam is **usually the frame the cut
took away** — "the frame you supposedly cut is standing in as the mark" is not
something you can check anything against. Seam cells set `exact` and really
decode.

This was missed in the filmstrip. **"Is this a seam" was written twice, and the
copy the filmstrip consulted only counted intervals from the second onwards** —
so **only when the head was cut**, the one thing that mattered most, the new
start, was not treated as a seam and showed a key picture instead. The test was
unified onto the `joinTimes()` list.

**The cards decode every mark, seam or not.** A card carries its own time in its
caption, and marks do not sit on key pictures: `⚑` takes the playhead where it
is, and commercial detection reports **the frame** a break falls on. Answering
with the nearest retained picture therefore put some other frame next to that
caption — off by **up to half a GOP, seven frames on broadcast material**. Twenty
marks placed across a 30-minute recording measured **eighteen showing another
frame**. The cards now go out as one `thumbs_at` call asking for the frame at
each mark's own time, and a retained picture only fills a card for as long as
that decode takes — at a seam not even that, the nearest one there being material
the cut removed. The frame at an instant does not change with the edit around it,
so the cache is keyed by time alone and holds across cutting and undoing.

**A mark that was just placed comes up selected.** A keyframe registered with `K`
and a seam left behind by a cut both turn the list card selection-coloured
immediately — the one you want to check right after is almost always that one,
and the more marks there are, the more the eye pays to find "which is the new
one". The selection is remembered by **time**, not by position in the list. Cut
something earlier and shift the numbering, and the same mark stays selected. Only
when several go up at once, as with commercial detection, is the selection left
alone, since there is no reason to prefer any one of them.

**Keyframes can be written out alongside the output.** Tick "keyframes to
.keyframe" next to the export button and a `.keyframe` file appears beside the
video with the same name. The contents are **line numbers only, CRLF, no
header** — the shape the tools that read this kind of file expect. **The numbers
are in the exported timeline, not the original recording:**

```
1532<CR><LF>
2827<CR><LF>
```

**A `.keyframe` of the same name beside the video is read back when the video
is opened.** Opening what you exported and starting from an empty list means
doing the last session's work again. The numbers are in the exported timeline,
so reading them back treats them the same way: `0` lands on the first picture
even when the recording's clock starts at 00:00:00.94. Numbers past the end are
dropped — a list written for a different cut of the same recording is the likely
reason, and a mark with no material behind it can neither be shown nor exported.
Header lines, and lines like `fps 29.970`, are skipped: files that arrive from
elsewhere carry them, and failing to open the recording over one unreadable line
would be the worse trade. The selection is left alone, as with commercial
detection — there is no reason to prefer any one of them. The number read shows
in the status line.

`Ctrl+D` starts it too. **Detection progress shows on the button.** A 30-minute
recording with the logo takes a minute and a half — you can tell you are waiting,
but not knowing how far along it is, is the problem. The `Detect commercials`
button itself becomes `Detecting 34%`, with the stage beside it: "examining the
audio", "looking for the logo". **Audio takes seconds and the logo ten times
that, being two passes over the video**, so progress is weighted 1:9 —
otherwise it jumps to 10 % instantly and then appears stuck.

**Detecting commercials lines up the start of each programme part and each
commercial as keyframes.** On real material (Nihonkai TV, 30 minutes) that is 9 —
the head of the recording, four commercial starts, four returns to the programme.
The thumbnails make it obvious at a glance that #05 is a commercial's title card.

Detection goes **as far as placing the marks**; a human does the cutting. A
"drop all the detected regions" button was added once and then removed — as long
as you want to check the boundaries by eye first, the one-press route goes unused.

**The point is that the copy ratio is visible under your finger.** Every added cut
re-runs the planner and updates the bar and the segment list.

**"Fully lossless" is claimed only when not a single frame was re-encoded.** Cut
away from a lossless point and a few frames are always rebuilt, but 2 frames in
40000 rounds to 100.0 % — **the one number a smart renderer must never print**.
The badge gives the actual frame count instead of a percentage (`re-encoded 14
frames`). The percentage too refuses to write 100 % when it has not reached 100 %
(99.96 % becomes `99.9%`).

**The filmstrip is the tool for deciding where to cut.** The playhead sits at the
centre with pictures laid out around it, as many as fit the window. The spacing
is chosen from GOP · 3 s / 6 s / 30 s / 3 min / frame. In the caption, `▲` is a
lossless point (make it a boundary and it costs no re-encoding), `✂` is a scene
change, and `⚑` is a registered keyframe.

**The unit of division is the GOP, not seconds.** Count the timecodes in a TMSR6
screenshot and the thumbnails go 00:03:42.36 / 42.86 / 43.36 … — steps of
**0.50 seconds**, which is exactly one GOP for broadcast MPEG-2 at 29.97 fps.
And the cell at the end of the clip is **narrower** (00:24:09.25 is followed by
00:24:09.95, and everything after is grey). So that is not "frames at equal
spacing" but **a fixed time window divided at GOP boundaries**, with each cell's
width being that GOP's length.

**The widths, however, do not follow GOP length here. Every cell is one picture
wide**, and the caption says how many seconds that cell represents. What the menu
selects is not "cell width" but "seconds per cell".

> This was rebuilt twice. The first version **apportioned the window width by GOP
> length**. Step one frame and the leftmost thumbnail is shaved while the
> rightmost grows — the pictures are still, but their sizes creep. That was the
> real content of "the thumbnail size is variable and unsettling". The second
> version made **each cell a whole GOP and clipped at the window's edge**. The
> creeping stopped, but as long as width follows GOP length, **wide and narrow
> cells still mix within one reel**. And shortening the window (which widens each
> GOP) opens up more than a picture's worth of empty backing beside each picture —
> "GOP · 3 s" was exactly that, too wide to be usable. Fixing the width makes this
> whole family of complaints go away at once.

**There is one scale factor for the whole reel.** Back when cell width followed
GOP length, fitting the picture to the cell with `object-fit: cover` made **the
scale factor move with the cell width too**. On material carrying 2:3 pulldown,
GOP lengths scatter in half-frame steps (0.63 s next to 1.07 s on real AT-X
material), so a face is 1.7× larger in one cell than in its neighbour — the other
half of "the thumbnail size is not constant".

Now **the height is fixed at 62 px and the width is whatever the picture's shape
asks for at that height**. The cell takes the same value (`cellPx()`), so picture
and cell are exactly the same size. The shape is **read from the picture on
screen**, not from the resolution in the code, because broadcast material is
anamorphic and the pictures the engine hands over have already been corrected.

> There was a period in between where every picture was drawn at **the width of
> the longest cell in the reel**. That does give a single scale factor, but
> `object-fit: cover` fills the box it is given, so **when the box is wider than
> 16:9 it enlarges the picture and crops the top and bottom**. One long GOP in the
> mix, or simply a shorter window (wider cells), turns the entire reel into that,
> and every cell looks like a horizontally stretched band. On real material this
> produced 250 px-wide cells at 62 px tall — a 4:1 box holding a 16:9 picture.
> Decide the height and let the picture decide the width, and this failure mode
> cannot occur in the first place.

The cell's backing is the same colour as an empty cell, so that a 1px rounding
error does not leave a bright stripe; normally the picture covers it. Positions
and widths are laid out in pixels, so the reel is rebuilt when the window width
changes (the number of cells that fit changes).

This unit is right in two senses. **The head of a GOP is the only place a cut
costs nothing but a copy**, and **its picture is already in memory from the scan**
(`thumbs.rs` keeps one per key picture), so filling a cell needs no decoding.

**Every cell, including the one you are standing on, shows the head of its GOP.**
Replacing just the playhead's cell with the preview's picture was tried, and it is
worse — the moment you cross a scene change, **one cell jumps to a different
picture while both neighbours stay put**. The eye can only read that as a glitch.
The strip is a ruler; saying where you are is the mark's job.

**The window is centred on the playhead, so the mark automatically sits in the
middle.** Step one frame and the whole row of cells flows one frame to the left —
"the mark is fixed at the centre and the pictures move" and "it moves one frame at
a time" are the same mechanism. Outside the recording stays grey (closing the gap
would take the mark off centre).

The view width is chosen from GOP · 3 s / 6 s / 30 s / 3 min. Since the width is
constant, the number of cells that fit (13 at 1400 px) is known in advance.
**The number of GOPs grouped per cell is chosen so that each cell covers the
chosen duration divided by that count** — for 3 minutes, 28 half-second GOPs per
cell, 14 seconds per cell. Even grouped, **a cell's head is a real GOP head**, so
picture and timecode always agree.

A cell is never shorter than one GOP (there is only one picture). So "3 s" really
shows about 6 seconds — it means "draw every boundary", the finest view available.
The grouping count comes from **the average GOP length across the whole
recording**. Counting around the playhead instead halves the answer at the ends of
the recording, where only one side exists, and "3 min" showed only 1 minute 15.

There is also a **frame view**. That is the one for closing in on a cut point
frame by frame, and reusing a 41-frame window gets it to a measured 27 ms per
frame. One wheel notch is one frame; Shift plus wheel jumps by GOP. Cell width is
one picture, same as the GOP view, so **changing the menu changes only how much
time is on screen**.

**Frame-by-frame fetches 41 frames or more at a time.** Asking naively for just
the 13 frames on screen pays a seek and a GOP decode per step, and 12 of those 13
are the same as last time. Fetching a wider window once and reusing it until you
approach its edge keeps a measured single-frame step at **about 27 ms**.

**The request is "this list of times", not "a centre and a spacing".** Walking the
edited timeline, adjacent cells can be minutes apart across a cut
(`preview::shots_at()`). Contiguous times are decoded in one seek, and the run is
broken where there is a jump. Rather than hard-coding the jump threshold, it is
**2.5× the smallest requested spacing plus 0.5 s** — one-frame spacing and
two-second spacing are both regular, and neither should be broken. On top of that
sits a **2-second cap**: even regular 14-second spacing ("GOP · 3 min") would,
with the run kept whole, decode three whole minutes to keep 13 pictures — see
[the five reasons it froze while building](#five-reasons-it-froze-while-building).

**It plays (`▶ Play` / `Space`).** Video is 640 px wide, at 24 pictures a second
from the proxy or 15 reading the recording directly — each one becomes a JPEG and
a data URL, so how many are requested depends on what is being read. **The audio
plays with it.** This is playback for **checking that the seams are right**, not
for watching the programme, and that is enough for it. The clock runs on **the
edited timeline**, so cuts take no time — playback continues straight across an
interval boundary.

The audio follows the same intervals as the video (`playback_audio.rs`), so it
jumps at the seams too. It **keeps no clock of its own**, though — once samples
are in the ring buffer, the sound card's own clock plays them at the right rate.
That is independent of the video's wall clock, so they drift apart slowly over a
long run, which is accepted for the same reason `audio.rs` allows 10.7 ms of error
at a seam. **It is playback for hearing whether the cut audio is right**, not for
watching end to end.

**During playback the strip is slid, not rebuilt.** Pictures only arrive 15 times
a second, so redrawing the window on each arrival gives 15 jumps a second (and it
was more noticeable before, at once every 0.4 s). So the cells sit on **a reel
wider than the window** and playback merely rewrites `translateX`. Playback
advances the edited timeline at 1× wall clock, so the current position between two
pictures is computed, not guessed — from the last picture's time and when it
arrived — and re-placed on every repaint. The arriving picture's time is eased in
rather than taken directly (tens of milliseconds of decode jitter otherwise looks
like running backwards). The reel is only redrawn when the playhead nears the
drawn edge, and since the same pictures land in the same places, the swap is
invisible. Measured at 160 px/s, tracked for 1.8 s with no jumps and no stalls
(GOP · 6 s, 2–7 px every 16 ms).

| Strip action | Behaviour |
|---|---|
| Left click | Go to that frame |
| **Right drag (held)** | **Smooth search back and forth.** Right of centre goes forward, left goes back, and further out is faster |
| Middle click | Find and jump to the **next scene change** from there |
| Wheel | Flow one step at a time |

On the keyboard, `←` `→` move one frame, with `Shift` one second, `s` /
`Shift+s` go to the next/previous scene, and `i` / `o` set IN / OUT.

**The output inherits the input's name.** The default is the same directory and
the same container as the original with `cut_` prefixed to the name — open
`2026年08月20日00時00分-ＢＳフジ…#8.ts` and you get
`cut_2026年08月20日00時00分-ＢＳフジ…#8.ts`. Broadcast recording names carry the
date, the station and the episode number, and that is the only handle for finding
them later. Collapsing that to `cut.ts` is a loss. The container follows the input
too (`.ts` gives `.ts`) — nobody wants everything moved to MP4 every time. The
file dialog completes with the first extension in its list, so the input's own is
sorted to the front.

The container can be chosen in the dialog's type field (`MPEG-2 TS (.ts)` /
`MP4 (.mp4)` / …). They are **separate entries per container** rather than one
"video" filter, because selecting one swaps the extension; without that, the
container would become "an extension you have to remember".

TS output is verified on real material too (60 seconds of terrestrial,
**1798/1798 frames, 99.2 % lossless, interlacing preserved**). The engine branches
on the container name, and only the MP4 family needs the `avc3`/`hev1`
manoeuvre — an Annex-B container carries parameter sets in the stream natively and
just works. One thing does differ: **in TS the start is 0.050 s, not 0.000 s**.
MPEG-TS makes no promise of starting at zero, and it is a constant applied equally
to video and audio, so it does not affect sync.

**Multiple intervals are supported.** Every added cut is sorted and merged with
overlapping ones, and the output intervals are built as the complement. The
engine's precondition — ascending, non-overlapping — always holds. On the seek
bar, green is the output itself, light blue the current selection, and a red
vertical line a cut seam. Right after a cut the selection collapses onto the seam.

Files exported from the GUI have been verified with `tests/verify_real.py` too
(real terrestrial material, 872/872 frames, 99.5 % lossless, A/V 2.9 ms). Multiple
intervals on real material give **1799/1799 frames, 99.0 % lossless, A/V 0.6 ms**
across two intervals.

A five-interval, 22-minute-34-second export produced by automatic commercial
removal gives **40589/40589 frames, 100.0 % bit-exact, uniform timeline,
interlacing preserved**.

> `verify_real.py` reports an "A/V skew" of 399 ms on this material, but that is
> **a property of the material** and not an error in the output. This recording's
> audio track ends 383 ms before the video (visible in `start_time`+`duration`).
> The last of the five intervals is a 1.5-second fragment sitting exactly at that
> end, so the whole difference shows up there. Export each interval on its own and
> the other four are all within ±18 ms — inside one AAC frame (21.3 ms). This
> metric is the **difference in length** between video and audio, not accumulated
> drift.

## Fast thumbnails and Smart Scene Search (`thumbs.rs`)

The goal was to reproduce TMSR6's "search over a flowing filmstrip" and "hover a
thumbnail of the seek target". **Neither can keep up if it decodes on demand.**
One seek plus a GOP decode is hundreds of milliseconds, so issuing a request every
time the pointer moves builds a queue.

The answer is **to decode everything once, right after opening**. And **intra
pictures alone** suffice: non-keyframe packets are never even handed to the
decoder (handing them over means they are parsed only to be thrown away).
Broadcast material has an I picture every 0.5 s, which is more than fine enough
for hover and for scene detection.

**The thinning has to happen on the packet side.** Doing the same thing with
libavcodec's `skip_frame` (`AVDISCARD_NONKEY`) breaks — see below.

**Scene detection comes free on the same pass.** The way to find a scene change is
to compare adjacent I pictures, and those I pictures are already being decoded for
the thumbnails, so the extra cost is a few hundred bytes of signature. The
signature is luma collapsed to a 16×9 average — a scene change swaps out a wide
area of the screen, and anything finer would respond to camera shake just as
strongly.

**All key pictures are retained** (capped at 4000, thinning the excess). The
scroll-search pictures come from here, so the retention interval is the granularity
of the search.

| Material | Scan time | Thumbnails | Scenes |
|---|---|---|---|
| Nihonkai TV, 30 min (3.5 GB) | 21.6 s | 3606 / 37 MB (0.50 s apart) | 602 (3.0 s apart) |
| AT-X, 30 min (1.8 GB) | 8.8 s | 1863 / 14 MB (1.00 s apart) | 360 (5.0 s apart) |
| NHK E-Tele, 6.5 min (700 MB) | 3.2 s | 774 / 5 MB (0.50 s apart) | 129 (3.0 s apart) |

The spacings are measured off the pictures, not the floor the pass was given —
which is a different number and, until this was corrected, the one being
reported. See below.

Peak resident memory is 112 MB for a 30-minute recording.

**The threshold comes from the material.** The baseline is "3× the typical
difference", but on a quiet educational programme the typical value is 0.0016,
too small to mean anything, so there is a floor; and on material that moves
throughout, marks would appear every half second, so there is a ceiling of "one
every 3 seconds on average", with the threshold taken as the quantile that implies.
On AT-X the material-derived value won (median 0.0672 × 3); on the other two the
ceiling did.

### `Track::interval` is measured, not asked for

**It used to report the floor, and the floor was an order of magnitude out.**
What the pass is given is "hold nothing closer together than this", worked out
as the recording's length over the 4000-picture cap. What it holds are key
pictures, and a recording puts those where it likes. On a 2 m 46 s BS Fuji
recording the floor comes to **0.042 s** and the pictures land **0.50 s** apart
— and 0.042 was what the track reported, what the status line printed, and what
two decisions in `thumbs_at` were taken against.

`Track::interval` is now **the median gap between neighbouring held pictures**.
The median rather than the mean or the smallest: on that recording the gaps run
from 0.27 s (an extra entry point where the picture changed) to 6.5 s (a stretch
with none at all), and neither end is what the film strip is drawn at.

Two consequences, both in `thumbs_at`:

- **"Is the caller asking for something finer than we hold?"** now compares two
  numbers arrived at the same way — the median gap *asked* for against the
  median gap *held*. It has to be the median on the asking side too: GOP mode
  asks at the recording's own entry points, and one short GOP anywhere in the
  window would otherwise send the whole strip off to be decoded (1.7 s for
  sixty cells) when every time it asked for is a picture already in hand.
- **"Is the nearest held picture near enough, or is this a hole?"** no longer
  asks the spacing at all. Every time asked for down that path is a key
  picture's own, so the held picture has to *be* that picture: the tolerance is
  two nominal frames, which under 2:3 pulldown is one picture's worth. Taking
  it from the spacing made it scale with the recording's length for no reason
  to do with the question — a hundredth of a second on a three-minute
  recording, half a second on a half-hour one — and on the corrected spacing it
  would have grown to 0.50 s, wide enough to caption the picture beside a hole
  with the hole's own time. Thirty minutes of AT-X is where that showed: 25 of
  its 1889 entry points are closer together than the floor and so are not held,
  and a tolerance of 0.45 s let each of those cells be answered by the picture
  next to it.

Frame mode, meanwhile, was only saved from answering a 0.033 s request with
pictures 0.50 s apart by 0.033 falling narrowly under 0.038. It is now 0.033
against 0.45, which is not a coincidence to rely on.

The seek index keeps the field in its format so that indexes already on disc
stay readable, but **the value in them is not trusted**: the spacing is taken
from the pictures the file carries, so an index written when the field meant
the floor still loads with the right answer. The stored number stands in only
when there are fewer than two pictures to measure anything from.

### Verifying it without looking

Scene detection is not verified by "looking about right". **A commercial boundary
is always a hard cut**, and it has been found through two completely independent
routes (silence and logo), so it can serve as the answer key
(`tests/run_scene_tests.sh`).

```
602 scenes, CM 4 blocks: edges 8/8, 15s junctions 31/32
```

All 8 edges of the commercial blocks, and 31 of the 32 points on the 15-second
grid inside them, had a scene mark.

### "The filmstrip breaks on BS Fuji and BS Nittele" — one frame is not always one I picture

The report was that GOP-view cells showed **pictures from the wrong time** and
that **the pictures themselves were corrupted in places**. The decisive
observation was that it does not happen in frame view. Frame view decodes afresh
every time through `shots_at()` (`preview::walk()`), while **GOP view comes from
the thumbnail track built at open time**. What was broken was `thumbs::build()`.

That code set `skip_frame = AVDISCARD_NONKEY` on the decoder. **That setting is
per picture**, meaning "do not read the slices of non-I pictures". But
**interlaced MPEG-2 does not always put one picture in one frame.** Broadcast
encoders switch between frame coding and field coding depending on the content,
and **a field-coded entry point is two pictures: an I top field plus a P bottom
field**. `AVDISCARD_NONKEY` throws the second away for being a P — leaving
**either a half-decoded picture or no frame at all**. That is what "corrupted" and
"holes" were, and the cells over a hole were filled by whatever distant picture
`Track::nearest()` returned. Measured, pictures **up to 16 seconds off** were
being displayed.

It is material-dependent, hence "sometimes". Counting directly in the bitstream:

| Material | Entry points | Of which field-coded |
|---|---|---|
| BS Fuji (Thunder 3 #2, 28 min) | 3371 | **350 (10.4 %)** |
| BS Fuji (Anime Guild #8, 24 min) | 2879 | **669 (23.2 %)** |
| BS11 | 422 | 1 (0.2 %) |
| Terrestrial NHK E-Tele / AT-X | 776 / 487 | 0 |

**`skip_frame` was removed.** Non-keyframe packets are not handed over in the
first place, so it was redundant to begin with and did nothing but harm. Scan time
went from 16.8 s to 17.1 s, essentially unchanged. Retained thumbnails went from
**3020 to 3371** (= the full count of entry points), and every one is 0.000 s from
its access point. The JPEGs are **byte-identical** to re-decoding the same times
through `shots_at()`.

**Two more things were fixed along the way.**

- **The last GOP was being dropped.** The decoder holds one picture back for
  reordering, so without `send_eof()` after the packets are exhausted, **only the
  entry point at the end of the file never comes out**. `preview::walk()` already
  did this.
- **`Track::nearest()` does not care about distance.** With a hole in the track it
  returns *something* however far away — and the filmstrip lines that up **with a
  caption from a different time**. `thumbs_at` now treats "further than the
  retention interval" as a hole and really decodes that one cell. Material thinned
  by the 4000 cap (25 out of 1889 on 30 minutes of AT-X) is rescued by the same
  path.

### "No audio" — only sample-accurate × TS was broken

Chasing a report from real material, **exactly one of the four combinations** was
broken. Two causes had stacked up.

1. **The output stream always advertised the source's parameters.** On a
   re-encode the packets come from our own encoder, yet the output-side parameters
   were copied from the source. MP4 does not notice, but **MPEG-TS uses that
   parameter set's extradata to re-wrap raw AAC into ADTS**, so a muxer handed
   somebody else's identity writes out bytes with no sync word. To a decoder that
   is noise; to a player it looks like silence. Fixed by constructing the encoder
   **before declaring the stream** and taking the identity from it.
2. **Timestamps were not converted on the re-encode path.** The copy path
   converted seconds to the output time base, while the re-encode path wrote the
   encoder's sample count directly. MP4's audio time base happens to be the sample
   rate, so it worked by accident, but **TS forces 90 kHz and a 799-second track
   came out as 426 seconds** (exactly 48000/90000).

Both are the kind that pass by coincidence in MP4, and **they were missed because
only one container had been tried**. TS × sample-accurate has been added to
`tests/run_audio_content_tests.sh`.

### Two more found along the way

- **An access point's time can be negative.** Source times are formed by
  subtracting the container's start time, so the first picture can land at
  **-0.0000003 seconds**. The head of a range is rounded to that, so cutting from
  the beginning dies with "cannot seek to that time". A floor of 0 is now applied
  when the index is built.
- **"Seek to the start" did not mean the start of the file.** It aimed at the
  container's start time, but MPEG-TS binary-searches byte positions by time, so
  if the first PES found has a time later than the header claims, **it lands after
  the file's first entry point** — and from there, there is nowhere to go back to.
  Aiming at 0 or below now means "the very first" rather than a time
  (`cut::seek_to`).

  **The same trap remained on the preview side (`preview::walk`).** Aiming at the
  first entry point lands on the second, reproducibly, on all four pieces of real
  material to hand, and on a recording with an entry point at time 0 **the first
  picture simply cannot be read** (with nowhere to go back to, the `seek_margin`
  retry lands in the same place). The strip's first cell was empty during
  building, and `shot_at` quietly returned **the picture at 0.6 s**. This too now
  means "the very first", and the boundary is **the first entry point** rather
  than 0 — nothing before it can be decoded anyway, which removes an entire pass
  of aiming, missing and retrying.

### Refinement — the mark is an I picture, the cut is just before it

What is found is "the I picture that first showed the new image"; the cut itself
is inside the preceding GOP. So just before jumping, that one GOP is fully decoded
and the place where consecutive pictures differ most is located
(`thumbs::refine()`, about 95 ms). Measured, it moves 0 to 0.47 s earlier.

There was a trap here. **Refinement always moves earlier, so searching onward from
the result comes back to the same scene.** "Next scene" does nothing. Fixed by
checking per candidate whether it really advanced, and moving to the next
candidate if not.

### Right-drag scroll search

TMSR6's "search over a flowing filmstrip" flows **only while the right button is
held**. Right of centre goes forward, left goes back, and further from centre is
faster. The speed is the cube of the distance (up to 60×), so near the centre you
can crawl a frame at a time, and taken to the edge it crosses a 30-minute
recording in 30 seconds.

**Nothing is decoded while it flows.** With hundreds of milliseconds per picture,
no amount of care makes decoding smooth. The already-scanned thumbnails (one per
key picture, 0.45 s apart) are used as they are. The filmstrip draws from the same
array, so pictures and strip flow together.

At slow speeds the coarseness shows, though. So **only below 2.5 seconds per
second, a real decode is slipped in from behind every 320 ms**. At speed the
persistence of vision hides it; slowly, the decode keeps up — cost paid only where
it is needed.

The moment the button is released, that position is re-rendered at full
resolution.

### Hover

Trace the seek bar and a picture of that position floats up. It is a lookup in
`Track::nearest()` and **decodes nothing at all** — while it is not ready yet, it
shows the time and says "preparing". Showing what you have beats making people
wait on principle; it feels faster.

As a by-product, **a 2-second-step filmstrip can be drawn instantly from the scan
results** too.

## The seek index (`seek_index.rs`)

Two passes stand between opening a recording and being able to work on it, and
both give **exactly the same answer every time the same file is opened**.

- **The access-point index** (`index::PacketScan`) — every packet read once, in
  decode order. Open GOPs and leading pictures can be learned nowhere else.
  About a second per gigabyte from cache, far worse from a disc.
- **The thumbnail track** (`thumbs::build`) — every key picture decoded once:
  the pictures the hover, the strip and the scroll search show, and the scene
  index that falls out of comparing them. Around 15 seconds for half an hour of
  1440x1080 MPEG-2.

The same answer does not need working out twice. **Both are written to one file
and kept** — that is the seek index. On 30 minutes of Nihonkai TV (3.7 GB):

| | First time | Second time |
|---|---|---|
| The walk (3607 access points) | 3.35 s | 0.09 s |
| The thumbnail track (3607 pictures, 602 scenes) | 15.4 s | 0.12 s |
| Total | 18.1 s | 0.13 s |

The file is 37 MB, of which the index itself is 133 kB; the rest is all JPEG.

**This is not a proxy.** Nothing is re-encoded and nothing is made to stand in
for the recording; the pictures still come from the recording itself. All the
index says is **where in the file they are**, which is the part that was being
worked out from scratch every time.

### What makes the substitution invisible

**The seam is `index::IndexSource` itself.** A held index hands back what the
walk would have handed back, so nothing below `scan_with` can tell which one
answered. That the two agree is checked on every run by
`tests/run_index_tests.sh` — the access-point count, the open-GOP count, the
mean GOP, the plan for the same cut, and the scene list.

**Never pick up a stale one.** The hash in the filename is taken from **the
path, the size, the modification time and the format version** (the same FNV-1a
as `proxy::cache_path`), so a file re-recorded under the same name gets its own
index. If one that does not fit is read anyway, the open fails, the index is
deleted and the recording is read instead — being one open slower beats quietly
using the wrong answer.

Writing goes to `.part.scix` and is renamed into place at the end. Close the
application while 37 MB is being written and **the half-written file never looks
like a finished one**.

Old ones are cut off at **32 files or 1 GB, whichever runs out first**
(`prune`). More files and a smaller budget than the proxy's eight and 4 GB,
because one is three orders of magnitude smaller — about 40 MB per half hour, so
a gigabyte holds more than twenty. The index for a recording finished last week
is still worth keeping, which is exactly the point.

### Byte offsets — taking the guesswork out of seeking

The index also carries the **byte offset** of every access point
(`AccessPoint::pos`).

It matters because MPEG-TS has no seek table: given a timestamp, libavformat
**bisects the file on byte position**. The landing is near the target rather
than on it, and in decode order an I picture sits *before* its leading pictures,
so overshooting by a little means missing the entry point entirely. Without an
index the remedy is to aim `seek_margin` seconds early and read forward, paying
a few GOPs of decoding every time the pointer moves — and sometimes that was
still not enough.

Given the byte there is nothing to guess. `index::seek_to_entry` jumps there
with `AVSEEK_FLAG_BYTE`, and the next packet out of the demuxer is the one
wanted. On 30 minutes of Nihonkai TV with the index already held, `--preview`
(including process startup):

| Time | Timestamp seek | Byte seek |
|---|---|---|
| 300.5 s | 0.23 s | 0.12 s |
| 900.25 s | 0.25 s | 0.14 s |
| 1500.125 s | 0.26 s | 0.16 s |
| 1799.9 s | 0.30 s | 0.17 s |

Startup is 0.09 s of that, so the picture itself goes from **around 0.15 s to
around 0.05 s**. That the JPEG is byte-identical either way is checked by
`run_index_tests.sh`.

**Only the stream containers.** MP4 and Matroska demuxers walk a sample table of
their own, which moving the file position does not touch — and both carry a real
seek table, so a timestamp seek is exact there already. `Source::byte_seekable`
draws that line (`mpegts` / `mpeg` / raw elementary streams). The old path comes
back with `SMARTCUT_BYTE_SEEK=0`.

## Proxy editing (`proxy.rs`)

**Off by default. `SMARTCUT_PROXY=1` builds one.**

The idea has not changed. Continuing to decode the loaded recording as it is, is
**too expensive just to look at pictures**, so **right after opening, the whole
thing is decoded once and rewritten small**. From then on the preview, the
filmstrip, playback and scene search read from that file. Cutting and export
keep reading the recording itself — the proxy is **for looking, not for
cutting**.

What changed is what "too expensive" was made of. Three reasons were given, and
[the seek index](#the-seek-index-seek_indexrs) removed two of them for orders of
magnitude less.

| Why it is expensive | The index | The proxy |
|---|---|---|
| MPEG-TS seeks by byte position, lands past the target, and has to go back `seek_margin` and try again | **Gone.** It holds the byte offset, so there is nothing to guess | Gone |
| The packets are walked again on every open to reach the same pictures | **Gone.** It is written down | Gone |
| 1440x1080 MPEG-2 is simply heavy to decode per picture | Remains | **Gone.** The pictures are small, all-keyframe, and carry no B pictures |

For that third one the proxy costs 85 seconds and 2.3 GB per half-hour
recording. But decoding one picture of broadcast 1440x1080 MPEG-2, with an index
in hand, is **50 milliseconds** — not heavy enough to be worth that price. So it
is off by default, and kept for when the material really does get heavy: 8K,
high bit depth. What follows is the record for when that day comes.

### Two conditions for the substitution to be invisible

**The clocks must match.** Each proxy picture carries the original picture's
display time on the same basis as `crate::scan`. A proxy time **is** a source
time; there is no conversion table, so there is nothing to get wrong and nothing
that can drift out of sync when the cuts move.

> This one bit once. Containers have opinions of their own about where a timeline
> begins. MP4 advertises its own `start_time`, writes an edit list, and sometimes
> quietly shifts everything so the first sample lands at 0. Some of that is undone
> on reading back, and whatever is not leaves **every proxy picture uniformly
> 10 ms early**. That is a third of a frame, so a wrong picture does not look
> wrong. Now the container's claims are ignored entirely and **the first proxy
> picture's time on the recording's side is written into a side table**, with the
> whole timeline translated to match.

**The access points must match.** A keyframe is forced at every access point of
the original recording and nowhere else (`sc_threshold` / `x264-params
scenecut=0` stop the encoder from deciding for itself). As a result the proxy's
index lands on the same times, and the thumbnail track built from it sits on the
same key pictures as one built from the original. On 30 minutes of Nihonkai TV,
`frame I:3607` — exactly the recording's 3607 access points.

### The thumbnail track comes free on this pass

The pass that builds the proxy is decoding the whole recording anyway, so
**handing the pictures that land on access points to `thumbs::Collector`** yields
the thumbnail track and the scene index at the same time (the same pictures and
the same computation as the `thumbs.rs` scan). 602 scenes on a 30-minute
recording — identical to the dedicated pass.

From the second time on the proxy is in the cache, so the track is rebuilt by
scanning **the smaller file** — 1 second for 2.8 minutes of BS Fuji (the same
material that took 10 seconds to build).

> **This used to say: "The track itself is not saved: it is JPEGs, 37 MB for a
> 30-minute recording, and reading it back is about as much work as rebuilding
> it."** That was true while rebuilding meant a pass over a small proxy. The
> moment the proxy stopped being built by default, "rebuilding it" went back to
> 15 seconds against the recording — against 0.12 seconds to read it back. It is
> now written out alongside [the seek
> index](#the-seek-index-seek_indexrs).

| Material | Proxy build | Result |
|---|---|---|
| BS Fuji, 2.8 min (300 MB, anime) | 7.1 s | 52 MB / 960x540 / 4995 pictures |
| Nihonkai TV, 30 min (3.7 GB) | 85.5 s | 1503 MB / 960x540 / 54089 pictures |

On the 4-core development VM. **When the same material was 720x404 CBR 523 kbps
it took 7.6 s for 13 MB, and the 30-minute one took 120 s** (in the previous
version of this table) — the width is 1.33× larger, the pictures are a different
thing entirely, and both times came down. The reason is
[just below](#does-it-have-to-be-h264).

Size varies 3× with the material (1.1 GB/hour for anime, 3.0 GB/hour for live
action). Flatter pictures compress smaller, so **the cache is bounded by bytes,
not by file count** — see [the cache](#the-cache).

What it costs comes back frame by frame — **though the amount coming back got
smaller once there was an index**. On 30 minutes of Nihonkai TV,
`--preview 900.25` (including process startup, with the index already held):

```
from the recording   0.14s   <- 0.09 s of that is process startup; the picture itself is 0.05 s
from the proxy       0.10s
```

Before the index the same table read "from the recording 3.05 s (2.8 s of it the
packet scan) / from the proxy 0.10 s". **Most of what the proxy was buying was
the scan**, and what is left is one picture's decode. **The filmstrip decodes 41
frames at once**, so if anywhere, that is where it still tells.

### Separating the reading side from the writing side

For a long time this pass **did everything on one thread**. libavcodec uses one
thread unless told otherwise, so decoding was slow to start with, and that same
thread also carried the scaling and the handoff to the encoder. Of four cores,
effectively two were working.

Only two things changed. **Pass the core count to the decoder**
(`crate::video_decoder`; MPEG-2 gets slice threading and H.264 frame threading —
the codec chooses), and **move scaling, encoding and muxing to another thread**
(`proxy::write_side`, a synchronous channel of depth 8). The reading and writing
sides do about the same amount of work — on 6.5 minutes of terrestrial, 18 seconds
of decoding against 13 of scaling plus 6 of encoding — so what used to add up in
series now overlaps, and **the same material went from 40.3 s to 23.3 s**.

The writing side finishes when the channel closes. If `stop` is raised part-way,
the sender is dropped, the thread joined, the partial file deleted and `cancelled`
returned — unchanged from before, so **an interrupted proxy never looks like a
finished one**.

### Does it have to be H.264?

No. But **H.264 was not the cause of the cost either**, so it was measured before
deciding. 2.8 minutes of 1440x1080 MPEG-2 on the 4-core VM; decoding alone is
3.4 s, which is the floor:

| Encoder | Added cost | Per hour | SSIM |
|---|---|---|---|
| libx264 `veryfast` CBR (old) | 4.2 s | 0.29 GB | 0.9913 |
| libx264 `ultrafast` CRF (new) | 3.7 s | 1.25 GB | 0.9946 |
| mpeg2video `-g 15` qscale | 2.4 s | 0.39 GB | 0.9825 |
| mpeg2video all-intra | 2.4 s | 2.7 GB | 0.9816 |
| MJPEG q=3 | 3.6 s | 3.2 GB | 0.9827 |

(SSIM is measured against **the source scaled to the same size**, so it isolates
the coding loss. Resolution loss is separate, and larger — see below.)

Intra-only (MJPEG, all-intra MPEG-2) is attractive because any frame can be
decoded on its own, and that part is real. But **it was not faster**. The cost on
this path is the sheer pixel count, not motion search, and scaling is effectively
free (1800 frames of 1440x1080→960x540 in 0.2 s on one thread). In exchange the
size grows 3–10× and the quality loses to H.264. Lowres decoding
(`AVCodecContext.lowres`) was tried too: 1.18 s → 1.13 s. MPEG-2's cost is in
bitstream parsing, not the IDCT.

**Both of the things that worked were inside H.264.**

1. **`veryfast` → `ultrafast`.** What a slow preset buys is bits, not pictures. A
   proxy is a temporary file that will be gone this week, so this leans the
   opposite way from a delivery encode — **spend disk, buy time**. Measured on 30
   minutes of live action, `ultrafast` gives 85.5 s / 1503 MB against `veryfast`'s
   132.3 s / 719 MB. The size halves, but **the wait when you open the file
   getting 1.5× longer** hurts more. The size is absorbed by the cache budget.
2. **CBR → CRF.** The old setting targeted an average bitrate of
   `width × height × fps × 0.06` (930 kbps at 960x540). On top of that **an IDR is
   forced at every access point of the original recording**, so an I picture every
   0.5 s eats most of the budget and everything else starves. Do the same forcing
   under a fixed quantiser and **the only price is size**.

### How the encoder is chosen

It tries `h264_nvenc` → `h264_videotoolbox` → `h264_amf` → `h264_qsv` →
`libx264` → `mpeg4` and **uses whichever opens**. Hardware is tried first for the
reasons in the [design notes](design.md#licence-and-patents-decide-before-shipping),
and `mpeg4` is always present in libavcodec, which makes it the last resort.

**"It opened" is not "it works".** A hardware encoder that is merely present in
the build will open on a machine with nothing behind it, and **refuse only when
the first picture is handed over** — by which time the file is part-written and
there is no route to the next candidate. So one throwaway picture is pushed
through first, and the encoder that accepted it is re-opened for the real run.

> The last resort had a trap of its own. **MPEG-4 part 2 writes its own time base
> into the bitstream in 16 bits** (`vop_time_increment_resolution`), so MPEG-TS's
> 90 kHz is too large and **it will not open at all** — that is, on exactly the
> material this tool exists for, the last resort was inoperative from the start.
> It is now given 60 kHz and the packets that come out are converted back to
> 90 kHz (real broadcast timestamps divide evenly; when they do not, the error is
> under a microsecond).

The width is 1280 and the quality CRF 22 (`SMARTCUT_PROXY_WIDTH` /
`SMARTCUT_PROXY_QUALITY` change them, and `SMARTCUT_PROXY_ENCODER` picks the
encoder itself). Quality is expressed in x264 CRF, and `quality_for` maps it onto
each other encoder's scale — a fixed quantiser for hardware encoders (`constqp` /
`global_quality` / `cqp`), qscale for the MPEG family. **Whichever you get, it is
one knob meaning one thing.**

Width matters because the preview now requests **the stage's real pixel count**
(`stageWidth`). It used to ask for a fixed 960, but with a 720-wide proxy
`encode_jpeg` capped there, and a wider stage got the browser to stretch it —
**the proxy's width was the ceiling on picture quality**, which is what "far too
low quality" really was. It matters far more than the coding loss. The same
ceiling is why 960 was later raised to 1280: on a screen with a device pixel
ratio above 1 the stage asks for the full 1920, and 960 was again the number it
got back.

The cap is measured in **square-pixel width**, not coded width. 1440x1080
broadcast material is displayed 16:9, so keeping all 1080 lines needs 1920 across
— stop at 1440 and the height has to drop to 810, throwing away a quarter of it.
The proxy has square pixels, so there the two agree (1920 is the number
`stageWidth` caps at).

**The proxy's own cap counts the same way.** `SMARTCUT_PROXY_WIDTH` is first
capped at the material's square-pixel width (1920 for 1440x1080 broadcast
material; capping at the coded width of 1440 was a counting error, and asking for
1920 built only 1440x810), and above that sits an absolute **1920x1080**
(`proxy::MAX_WIDTH` / `MAX_HEIGHT`). Since the stage caps at 1920, a larger proxy
**pays build time and disk only to be scaled down before anyone sees it**. The cap
applies to the picture itself, so material taller than 16:9 — 4:3 at 2880x2160,
say — hits the 1080 height first and the width comes down to 1440 to match.

| Width | Build | Size |
|---|---|---|
| 960 | 7.0 s | 48 MB |
| 1152 | 8.2 s | 61 MB |
| 1280 | 9.1 s | 87 MB |

1280 is the default: the time in that table is **paid the moment you open a
file**, and this is the last row where the picture is still improving faster than
the wait grows. On a slow disk, `SMARTCUT_PROXY_WIDTH=960`; with cores to spare,
up towards the 1920 cap.

The output is written with **square pixels** — broadcast 1440x1080 is 16:9 on
screen, so scaling it as-is would hand a squashed picture to the timeline. The
scaling itself evens out interlacing combs, so the proxy needs no deinterlacer.
B pictures are not used (with decode order matching display order, there is one
fewer thing between the pointer and the picture).

Audio is not put in the proxy. Playback audio is read from the recording directly,
and speed is not a problem there.

### The cache

`<cache>/dev.smartcut.app/proxy/<name>-<hash>.mp4`. The hash is taken from
**path, size, mtime, width and quality**, so a file re-recorded under the same
name, or a change of width or quality, gets its own proxy. Writing goes to
`.part.mp4` and is renamed at the end — an interrupted proxy never looks finished
(and it counts as complete only with its `.marks` sidecar present too).

Old ones are pruned at **8 files or 4 GB, whichever comes first**. A file count
alone is not a limit — one proxy's size moves with the recording's length and the
requested quality, so the same "8 files" can be 300 MB or 8 GB. At the 1280
default a 30-minute recording builds a 2.3 GB proxy — roughly 4 GB per hour of
programme — so 4 GB is two 30-minute programmes. The newest one is never pruned
even over budget, because that is the one being edited; **that is also why the
budget has to clear one proxy on its own.** At 2 GB with a 1280-wide proxy it no
longer did, and every file but the newest was deleted as soon as it was written —
reopening yesterday's recording would rebuild it from the recording every time.

### Editing works without a proxy

Which is now the default. Set `SMARTCUT_PROXY=1` and then have no encoder open,
or an unwritable cache, and each of those only leaves the preview slow; editing
itself still works. When the build fails the reason is shown under the strip and
it falls through to [the seek index](#the-seek-index-seek_indexrs) path. The
same applies while it is building: the track cannot speak yet, so the recording
answers with its own pictures.

### Five reasons it froze while building

"You can edit while it builds" was the claim, but in practice **moving the strip
while the proxy was building froze it solid**. There were five reasons, each
independent.

The first three stopped the window locking up, but **it still "froze"**. Measured,
it had not stopped: one strip update took 1.5–1.9 s, and right-drag search asks
for a redraw every 70 ms, so the pictures were **always more than a second
behind**. On a 24-minute recording, when the playhead had reached 23 seconds the
strip was still showing around 5. And the preview during a search was drawn from
`hover_thumb` (the track's holdings), which is empty until the build finishes, so
**not one picture moved**. Not stopped, but indistinguishable from stopped.

**1. Decoding ran on the window's thread.** Tauri's `#[tauri::command]` runs
**on the thread that paints the window** unless it is `async`. `thumbs_at`,
`preview` and `scene_search` were all synchronous functions, so the webview could
not repaint a single pixel until a decode finished. With a proxy that is tens of
milliseconds and goes unnoticed; without one it is a seek plus a GOP decode from
the recording — while the encoder occupies all four cores. Every command that
touches pictures now goes through `off_thread()` (`spawn_blocking`), as do
`open_source`, which reads the index, and `make_plan`, which may go and read the
first picture.

**2. A wide window decoded "everything the window spans".** `shots_at()` treats
evenly spaced times as one run and **seeks once, then keeps decoding forward**.
For one-frame steps that is right. But "GOP · 3 min" asks for 13 times 14 seconds
apart, and the same rule would **decode three whole minutes to keep 13 pictures**.
Broadcast material has an access point every 0.5 s and a seek lands on one —
**past 2 seconds it is cheaper to seek again**. That cap was added to the
run-breaking condition.

**3. Redraw requests piled up faster than decoding.** Right-drag scroll search
calls for a strip redraw every 70 ms, and playback calls for one every frame. When
one takes a second the queue only grows, and the pictures that come back are
**from a position long since passed**. Wheel decoding already had a "one at a
time, newest request wins" mechanism, so the strip got the same one (`askStrip`).

**4. The thumbnails being built were not handed over until the end.** The proxy
pass decodes the recording's access points **in order from the start**, and the
thumbnail track accumulates in `thumbs::Collector` as a by-product. But the track
only entered `Thumbs` **after the pass finished**, and for the one or two minutes
before that the strip, the scroll search and the keyframe cards all decoded from
the recording on the grounds that "we have nothing" — **not using pictures that
were already in hand**. Now they are collected every 0.5 s via
`Collector::take_new()` and appended (`share` in `proxy::build`, `hold()` on the
GUI side). Anything already passed **answers instantly from memory**, and since
`hover_thumb` can answer, the pictures move during a search too. What has been
handed over does not remain in `Built::track`, so the head and tail are stitched
back together at the end — and the stitched track matches a one-pass track built
from the cache in both picture count and scene count.

That is why `Track` carries `covered` (**how far it can speak for**).
`nearest()` returns its last held picture when asked past the end, so without it
**a picture from a different time appears where nothing has been decoded yet**.
`thumbs_at` was already checking the time difference against its holdings;
`hover_thumb` was not.

**5. The GOP strip was decoding the insides of GOPs.** "GOP · 6 s" asks for 13
times 0.5 s apart. That is tighter than #2's 2-second cap, so it becomes one run
and **decodes six whole seconds to keep 13 pictures** — 180 pictures of 1440x1080
MPEG-2, 1.8 s. But in GOP mode the cells are **by definition all access points**,
and an access point's picture can be decoded on its own (`thumbs::build` has
always relied on this). So **when every requested time is an access point,
non-key packets are not handed over** (`keys` in `preview::walk`). 13 pictures
decoded, 0.24 s.

The discarding happens at the packet stage, not through `skip_frame` — for the
same reason as the note in `thumbs::build`: a field-coded entry point is one
packet holding an I top field plus a **P** bottom field, so `AVDISCARD_NONKEY`
would halve the picture. That the resulting JPEGs do not differ from a full decode
by a single byte is confirmed by `examples/keysdiag.rs`.

While building, the strip is now **0 seconds for anything already passed and
0.24 s beyond it**. `examples/proxydiag.rs` runs a build while hammering the strip
and measures both.

## Engine-side additions the GUI needed

- `preview::frame_at()` — returns the picture at time T as a JPEG. The webview
  cannot play MPEG-2 TS, so the scrubbing pictures are made here. An open GOP can
  only be decoded from its head, so it seeks to the preceding access point.
- `preview::shot_at()` / `shots_at()` — returns, besides the JPEG, that picture's
  real time and type (I / P / B). The strip pulls several consecutive pictures out
  of one seek — but breaks the run when two times are more than 2 seconds apart,
  where seeking again is cheaper than decoding through. When every request is an
  access point, the packets in between are not handed over (#5 above).
- `thumbs::build()` / `build_with()` / `refine()` — the thumbnail track and the
  scene index. See above. `build_with` takes the same three things the proxy
  build does — progress, handoff and cancellation — because the pass that makes
  the index runs for 15 seconds too, and the strip needs something to show
  meanwhile.
- `seek_index::SeekIndex` — saving and loading the access-point index and the
  thumbnail track. See above. It implements `IndexSource`, so a loaded one goes
  straight into `scan_with()`.
- `index::seek_to_entry()` — seeks directly to an access point's byte offset.
  See above. Returns `None` where it cannot, and the caller falls back to a
  timestamp seek.
- `proxy::build()` / `open()` — building and opening the proxy. See above. The
  build takes three things: progress, handoff and cancellation. The handoff
  (`share`) passes accumulated thumbnails over **while it is still building**, and
  the cancellation exists to throw away a pass still running for a previous file
  when a new one is opened.
- `cut_with_progress()` — a progress callback. Export is moved off the UI thread
  with `spawn_blocking`.

## Where it got stuck

- **The screen does not update in a VM.** WebKitGTK's compositor draws nothing
  without a GPU and never updates after the first paint — which looks exactly like
  a freeze. `WEBKIT_DISABLE_COMPOSITING_MODE=1` fixes it. This UI does not need
  compositing, so the app sets it by default.
- **`sync.sh`'s `--delete` was deleting the GUI build artifacts**, because the
  exclusion was only `rust/target`. Fixed to exclude `target/` generally.
- Real material (a byte-sliced TS) produced `no pictures decoded`. Nothing before
  the file's first access point can be decoded, so an interval's start is now
  clamped to it. That fix took the copy ratio from 96.5 % to 99.6 %.
- **A seek could land one GOP late, and that silently returned the wrong
  picture.** MPEG-TS seeks by byte position, so it can land past the target, and
  crossing a sequence header additionally makes the first GOP undecodable. Both
  produce the same symptom: the picture you asked for does not come out. Nothing
  on screen looks wrong — the frame number and the picture simply disagree. Now,
  if the first picture is past the target, it goes back `seek_margin` and retries,
  and `tests/run_preview_tests.sh` verifies "does the time you asked for come
  back". It was found because scene refinement never did anything (`seen=1`).
- **On pulldown material, "tolerance of one frame interval" is wrong.** Under 2:3
  pulldown one picture is two or three fields, so the interval alternates between
  33.4 ms and 50.1 ms. Assuming 29.97 fps and picking "the nearer" grabs the wrong
  side of the gap. It now actually compares the two neighbours and returns the
  nearer, and the GUI snaps the playhead to the returned picture's real time so
  that counter and picture cannot disagree.
- **Which side of a seam a frame belongs to.** An interval is `[a, b)`, so the
  picture at `b` is the first one the cut took. But the output-time to source-time
  conversion answered a seam with "the end of the preceding interval", and
  **exactly one frame of the material just cut stayed on screen**. The engine was
  discarding it correctly, so the frame count and the output were right — only the
  display was off, which is the hard kind to find. Fixed by settling the boundary
  as "a seam is the head of the following interval".
- **Dropping cost more than keeping.** The first playback implementation encoded a
  JPEG even for pictures it was too late for, and then threw it away — encoding is
  an order of magnitude above the 3 ms decode, so "drop it" was the most expensive
  path in the program. **Measured at 0.3× speed.** Adding a place to decide
  **before** encoding (`Pace::Show` / `Skip` / `Stop` on `play_from()`) was the
  entire fix: 10.01 seconds of material in 10 seconds — exactly real time. **A
  discard path is pointless unless it is cheaper than the keep path.**
- **"Audio playback error: The requested device is no longer available" — the DAC
  that is currently making sound is reported as unplugged.** cpal's default output
  on Linux is ALSA's `default`, and what that points at depends on the machine's
  `alsa.conf`. On a PipeWire desktop without `pipewire-alsa`, `default` points at
  **the bare sound card**, which PipeWire is holding, so it returns `EBUSY`. cpal
  maps `EBUSY` to `DeviceNotAvailable`, hence the wording about removal — while it
  is in fact plugged in and other applications on the desktop are playing through
  it. `aplay -D default` fails with the same `EBUSY`, which makes it quick to rule
  out an application-specific cause. When `default` cannot be opened, `pipewire` /
  `pulse` / `sysdefault` are now tried **by name**
  (`playback_audio::open_output`): a sound server's PCM is the very device other
  applications are already using. Enumeration only happens after a failure — cpal
  **opens every PCM** the ALSA hints list in order to enumerate devices, which is
  not work for the path that succeeds. To fix the machine instead,
  `sudo apt install pipewire-alsa` fixes where `default` points. **Windows has no
  equivalent tolerance** — that story is under "Where it got stuck" in
  Distribution (Windows).
- **A seam's thumbnail must not come from the scan results.** Strip cells are fast
  because they are filled with already-scanned key pictures, but **a cut seam is
  not a GOP head**. Show "the nearest key picture" there and half the time you get
  **the last GOP that was supposedly cut** — the material you removed appears to be
  sitting on the seam. Seam cells and keyframe cards now decode that exact time
  (`exact` in `thumbs_at`). There are at most a handful of seams on screen, so it
  is only a few extra decodes. It is easy to spot: **only the seam cell's timecode
  breaks the 0.50-second spacing** (e.g. …51.21 → 51.71 → 52.07), which is proof
  that it stands on the cut position itself rather than on a GOP head.
- **`[IN, OUT)` cannot be expressed at the end.** A half-open interval is right in
  the middle of a recording and wrong at its end — **there is nowhere to put OUT
  past the last picture**, so "cut to the end" left the final picture hanging off
  the output as a one-frame interval. Fixed by snapping IN/OUT to the end of the
  timeline when they are on the first or last picture. Not a settings problem: the
  representation was simply not expressive enough at the edges.
- **The timeline's origin is the first access point, not 0 seconds.** Nothing
  before it can be decoded and the planner rounds it up, so it never reaches the
  output. Yet the on-screen timeline counted from 0, so the counter said `54111`
  while `54091` frames were written — 20 frames out. Fixed by putting the origin on
  the first access point.
- **Focus left on a `<select>` steals the arrow keys.** After choosing a spacing,
  `←` `→` changed the spacing instead of stepping frames. Excluding `SELECT` in
  keydown does not stop the browser's default behaviour, so `blur()` on `change` is
  needed to give the keyboard back.
- **A `max-height` on the segment list makes the editor dance vertically.** The
  list's height changes with the number of cuts, and the strip, the seek bar and
  the button row all move with it. The height is fixed now. Kinder to hands that
  remember coordinates, and to `xdotool`.
- **Automated interaction on a VM is unreliable.** DOM updates do not reach X, so
  screenshots go stale and coordinates derived from them are wrong. Resizing the
  window by 1px forces a repaint, so that has to be interleaved before every check.
  It does not happen on real hardware, but it is worth knowing when driving the GUI
  mechanically with `xdotool`.
