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

Only the first of those is **an actual signal rather than a guess**. So when a
recording carries subtitle marks they are always used, and the other two cues
are for recordings that do not. Stations divide cleanly into those that write
the marks and those that do not, which is why all three are needed.

**When the subtitle marks are found, the logo is not read at all.** It is more
reliable and ten times faster, so the logo pass is skipped even with "use the
logo too" ticked. On the Nihonkai TV recording that took the analysis from 50
seconds to 7.

There are three possible answers: **subtitle marks**, **logo and silence**, and
**no commercials**. The last one is a recording where the logo never went away
from start to finish — that is an answer, not a failure to find anything. Which
one was used is shown next to the number of blocks.

## Check that the blocks are multiples of fifteen seconds

On the three recordings that contained commercials, every detected block came
out as **an exact multiple of fifteen seconds** (150.0 / 120.0 / 120.0 / 60.0 s,
135.0 / 105.0 s, and 300.0 s).

The detector does not use that property at all, which is exactly why it makes
good independent evidence that the boundaries landed where they should.
**A block of 119.8 seconds instead of 120.0 means something is slightly off.**

## When it gets one wrong

| What you see | What to do |
|---|---|
| **The block is a little short or long** | Move IN or OUT to the keyframe you want, then cut. The marks are a starting point, not a verdict |
| **A break was missed** | `S` and `Shift+S` step through the scene changes. That is the fastest way to find a boundary by hand |
| **The block swallows part of the programme** | Delete the offending mark with the `×` on its card and cut around it by hand. This is the expensive mistake, and the detector is tuned to avoid it, but material with an unusual rhythm can still trip it |
| **Nothing is found** | The recording may genuinely have no commercials. Otherwise it carries neither subtitle marks nor a logo |

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
