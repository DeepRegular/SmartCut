# Commercial detection

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](cm-detection.ja.md)

SmartCut can find the commercial breaks in a recording for you. Select clips in
the list and press `Ctrl+D`, or use the **Detect commercials** button on the
right. The cut editor has the same button in its info bar.

## Detection places marks — that is all

![The cut editor with the detected marks](../images/usage-editor.png)

**It never cuts anything by itself.** All it does is put a keyframe (a mark) at
the start of each commercial block and at the point where the programme comes
back. Open the cut editor and those marks are waiting in the left-hand column,
each with a thumbnail.

You decide what to remove. There used to be a "keep everything except the
commercials" button; it was taken out again, because nobody pressed it. You end
up wanting to check the boundaries with your own eyes anyway.

## A detection can be kept

A detection reads the recording for minutes. So it is written down, and you
never wait for the same one twice: **open the same recording again and the
earlier detection comes back**, with "from an earlier run" on the row.

That copy lives in SmartCut's own cache. To carry one to another machine, or to
hand it to somebody else, use **≡** → **Save the detection…** in the cut editor,
which writes `recording.cm.json`. **Read a saved detection…** in the same menu
reads one back, and a file left beside the recording is picked up when it is
opened.

To look at the band first and decide every time, turn off the preference **Turn
a detection into keyframes** (it is on out of the box). ≡ → **Turn the detection
into keyframes** still places the marks when you ask.

**Where a `.keyframe` sits beside the recording, the detection is not mixed into
the marks.** The `.keyframe` is your answer about where the breaks are and the
detection is the program's, and marks from both in one column cannot be told
apart afterwards. The detection is still shown, as the band under the timeline
and the line beside it, and **≡** → **Turn the detection into keyframes** puts
its marks down if you want them. Which file is read first is a preference.

## How well does it work?

Five real recordings, measured against a ground truth built by eye:

| Material | Cue used | Programme cut by mistake | Commercial left in |
|---|---|---|---|
| Nihonkai TV variety (30 min) | subtitle marks | 0.2 s | 0.1 s |
| AT-X anime (30 min, no logo) | silence only | 0.0 s | 0.1 s |
| BS Fuji anime (28 min) | subtitle marks | 0.1 s | 3.4 s |
| BS Fuji anime (24 min, no commercials) | logo + silence | 0.0 s | 0.0 s |
| NHK E-Tele (6 min, no commercials) | logo + silence | 0.0 s | 0.0 s |

**The two kinds of mistake are counted separately on purpose.** Roll them into
one number — "98% accurate" — and the number can improve while the mistake that
actually hurts gets worse.

They are not worth the same. **Mistake a piece of programme for a commercial and
that piece disappears.** Miss a commercial and you can see it straight away and
fix it on the spot. So the detector is tuned to **leave it in when in doubt**.

## How it finds them

Japanese commercials are laid out in **fifteen-second units**, and every join
between them carries a short silence. Two things separate those from a pause
inside the programme:

- **The silence is longer.** About a second at a join, 0.1–0.4 s inside a
  programme.
- **They line up on a fifteen-second grid.** The next join is exactly 15, 30 or
  60 seconds away.

Neither one settles it alone. So SmartCut scores the silences by length and by
how they line up, and groups the high-scoring runs into commercial blocks.

There are three cues. It uses whichever ones the recording carries.

| Cue | What it is | Cost on a 30-minute recording |
|---|---|---|
| **Subtitle marks** | a mark the broadcaster's equipment writes into the subtitle stream at every switch | 3 s |
| **Silence** | silences that line up on the fifteen-second grid | 3 s |
| **Station logo** | the corner logo that is on during the programme and gone during the commercials | 30 s |

Only the first of those is **an actual signal rather than a guess**, so where a
recording carries subtitle marks they are what is read, and the other two cues
are for recordings that do not.

**Not every recording's marks cover the whole of it.** Some stations mark only
where the programme stops and starts and nothing in between. Marks like that
are put aside, and the logo and the silences decide instead.

**Where the marks are used, the logo is not read at all.** The marks are the
better answer, and the analysis finishes in a fraction of the time.

There are three things to go on: **subtitle marks** (`n caption resets`),
**logo and silence** (`logo + silence`), and, where no logo can be found,
**silence alone** (`silence only (no logo)`). Which one was used is shown in
front of the number of blocks.

A recording where the logo never went away from start to finish says
`nothing that looks like a commercial` — that is an answer, not a failure to
find anything.

## A quick check: the blocks should be multiples of fifteen

Commercials are sold in fifteen-second units, so a detected block ought to come
out at 60, 120 or 135 seconds and not at something in between. **A block of
119.8 seconds where it should be 120.0 means a boundary landed slightly off** —
worth a look before you cut.

## The few seconds a subscription channel drops in

Where a terrestrial broadcast had its commercials, some subscription channels
put two to nine seconds of their own animated ident instead. A break that short
is not looked for out of the box: **Also find the short inserts (a channel's own ident)** in Preferences
turns it on for the cut editor's **Detect commercials** (`--inserts` on the command
line).

It is off because the same test catches a programme's own full-screen caption
card. What finds an insert is the sound — it is cut in, so the programme's audio
stops for it — and a card takes the corner just as thoroughly, so a programme
that lays its cards over silence gives both. Measured over twenty episodes of
one such programme, about a quarter of what came out was a caption card. Worth
having where you know your channel does this, and worth a look at each mark
either way.

## When it gets one wrong

| What you see | What to do |
|---|---|
| **The block is a little short or long** | Move IN or OUT to the keyframe you want, then cut. The marks are a starting point, not a verdict |
| **A break was missed** | `↑` and `↓` (and `S` / `Shift+S`) step through the scene changes. That is the fastest way to find a boundary by hand |
| **The block swallows part of the programme** | Delete the offending mark with the `×` on its card and cut around it by hand. This is the expensive mistake, and the detector is tuned to avoid it, but material with an unusual rhythm can still trip it |
| **Nothing is found** | The recording may genuinely have no commercials. Otherwise it carries neither subtitle marks nor a logo |
| **A few seconds of channel ident are left in** | See above: ask for the short inserts |

## Cut on the marks and nothing is re-encoded

Cut on the detected marks with both ends on a point **the stream can be cut at**
— press `Snap to lossless`, or leave them alone if they are already there — and
the whole commercial cut becomes **a plain copy**. Not one frame is re-encoded.

On a 30-minute commercial-broadcast recording, 22.6 minutes of programme
remained as five ranges, at **100.0% lossless copy**.

```
output 1506.005s — lossless copy 1505.971s (100.0%) / re-encoded 0.033s (0.0%)
```

The join sits in the middle of about a second of silence, so moving the cut a few
hundred milliseconds **cannot be heard**. That one small concession is what makes
the whole commercial cut lossless.

---

What goes on inside the detector — how the scoring works, how the logo is found,
and how the accuracy was measured — is in
[Commercial detection internals](../technical/cm-detection.md).
