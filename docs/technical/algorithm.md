# Algorithm and pitfalls

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](algorithm.ja.md)

## The algorithm

For a kept interval `[t_in, t_out)`:

```
... I ....... I=========================I ....... I ...
      ^t_in   ^k_first                  ^k_term   ^t_out
    |<-head->|<--------- body --------->|<-tail->|
      re-encode        stream copy       re-encode
```

`head` and `tail` begin or end partway through a GOP, so there is no choice but to
decode from the previous access point and rebuild them. The `body` between them is
emitted as the input's own bytes. Cut exactly on access points and there is no
re-encoding at all.

Rebuilding assumes there is an encoder to rebuild with, and for one codec there is
none. **VC-1**, which most Blu-rays pressed before about 2010 are written in, has no
encoder in libavcodec, on a graphics card, or in any free implementation. SmartCut
writes those pictures itself — intra pictures only, which is all a `head` or a `tail`
needs, since nothing outside the fragment may be referenced. What that costs, and how
it was measured, is in
[the Rust core](rust-core.md#vc-1-the-codec-with-no-encoder).

The Python reference implementation is split as follows:

| File | What it does |
|---|---|
| [`probe.py`](../../smartcut/probe.py) | Stream parameters, the access point index, leading-picture detection and the reference test |
| [`planner.py`](../../smartcut/planner.py) | Turns intervals into a segment list |
| [`bitstream.py`](../../smartcut/bitstream.py) | Annex-B / MPEG-2 access unit splitting |
| [`renderer.py`](../../smartcut/renderer.py) | Runs ffmpeg and concatenates the results |
| [`verify.py`](../../smartcut/verify.py) | Decodes the output and compares it against the source frame by frame |

## Pitfalls

This section is why "just cut on GOP boundaries and join the pieces" is not enough.
Every one of these problems was hit for real while building the prototype, and every
one has a test pinning the reproduction.

### 1. The parameter sets (SPS/PPS) do not match

Making the re-encoded part's SPS match the original stream bit for bit is
effectively impossible: a different encoder means different VUI and different VBV.
And an MP4 `avcC` box, like a Matroska CodecPrivate, **can only hold one set of
parameter sets**. Concatenate naively and either the copied part or the re-encoded
part gets decoded with the wrong SPS, and the picture falls apart.

The fix has two parts:

- Write each piece as a **raw Annex-B elementary stream** and join them by plain byte
  concatenation. An elementary stream carries SPS/PPS in band at every IDR, so
  parameter sets that differ from piece to piece can legally coexist.
- Write the final MP4 with an **`avc3` / `hev1` sample entry**. That is the in-band
  form defined by ISO/IEC 14496-15, and it is not folded into `avcC`.

MPEG-2 does not have this problem in the first place, because its sequence headers
are in band already.

### 2. The access point index has to scan *packets*

`ffprobe -skip_frame nokey` **misses access points in open GOPs**, because the
decoder cannot output an I picture whose references are absent. On the prototype's
test material it found only 3 of the 10 access points that were actually there.

Looking at the packet's `K` flag avoids decoding entirely. It is faster, and it is
correct.

### 3. Leading pictures — the heart of the open-GOP problem

Pictures that come **after an I picture in decode order but before it in display
order** are called leading pictures, and they reference the previous GOP. Start a
copy there and they cannot be displayed.

Here the handling diverges completely, depending on **whether the leading picture is
itself a reference picture**:

- **MPEG-2**: B pictures are never referenced, so leading pictures can simply be
  dropped. Even an open GOP works as a copy start point.
- **H.264 / HEVC** (x264's `open-gop` and equivalents): B pyramids mean a leading
  picture can be a reference picture. Drop it and every later frame that referenced
  it breaks, taking the whole GOP with it.
- **VC-1**: B pictures are never referenced either, so it behaves like MPEG-2 — but
  its picture header cannot be read on its own. Even whether a picture states its
  type in three bits or in one is settled in the *sequence* header, which a transport
  stream restates in front of every entry point and libavformat hands over as
  extradata. If no sequence header ever turns up, SmartCut answers conservatively and
  treats every picture as a reference. The only cost of that is losing the chance to
  start a copy at an open GOP.

This was confirmed by measurement. Dropping leading pictures on x264 open-gop
material made all 60 frames of the first GOP of the copy region mismatch; keeping
them, they matched.

So SmartCut reads `nal_ref_idc` (H.264), the NAL type (HEVC) or `picture_coding_type`
(MPEG-2) out of the bitstream to decide whether the picture is referenced, and if it
is, that point is not used as a copy start point. As a copy *end* point an open GOP
is fine either way — the display range simply ends at `lead_start`.

Removing leading pictures cannot be expressed in the container (it would need an edit
list), so the Annex-B access unit boundaries are parsed by hand and the pictures cut
out there ([`bitstream.py`](../../smartcut/bitstream.py)). Note that H.264's "first
slice = start of picture" rule does not carry over to MPEG-2, where a picture start
code is followed by several headers before the slices, so the access unit split
differs between them.

### 4. Specify intervals in frames and packets, not seconds

Pass `-t` a duration in seconds and it goes wrong twice over:

- Under `-c copy`, `-t` is evaluated against the **DTS**. DTS runs ahead of
  presentation time by the reorder depth, so extra I/P pictures from the next GOP
  sneak in — 180 frames came out as 182.
- With fractional frame rates (30000/1001), rounding shifts the result by ±1 frame.

The only robust approach was to **count the display frames and the packets to copy as
integers and pass those** (`-frames:v N`). The packet count follows exactly from the
difference of decode-order indices in the access point index.

### 5. Container start_time (MPEG-TS)

TS timestamps do not start at 0 — the test material starts at 1.423 s. `-ss` is
relative to the start of the file, but ffmpeg re-bases the output by the `-ss` value
alone, so **start_time survives as a residual offset in the output timeline**.

A seconds-based `-t` runs straight into that. Access point times are therefore
normalised by subtracting start_time, and interval lengths are passed as frame
counts, which avoids both problems.

### 6. Re-encoded regions must be decoded from earlier

With open-GOP sources, seeking straight to the target position with `-ss` makes ffmpeg
discard the entire GOP it could not decode, so the output starts up to one GOP late
(0.2 s, measured). Decoding starts from an access point a few GOPs earlier, and the
front is trimmed with an output-side `-ss`.

### 7. Audio has no GOP structure

Audio is cut **per kept interval**, not per video segment.

- `--audio-mode copy` uses the source frames as they are. Interval boundaries snap to
  the nearest audio frame, which is up to about 24 ms for AAC.
- `--audio-mode reencode` makes one pass through the `atrim` and `concat` filters.
  Sample accurate, but it re-encodes everything.

The Rust core adds a third mode, `smart`, which is its default: the frames a boundary
falls inside are re-encoded and the rest are copied. It also takes a channel count
(`--audio-channels`), which is what folds a 5.1 recording to stereo; that has no copy
path at all, so it is a whole-track re-encode whatever the mode says.

Both are covered in detail in [audio](audio.md).

Handling AAC encoder delay and priming strictly via edit lists has not been
implemented.

### 8. The verification reference decodes from the start of the file

The reference side of `--verify` must not be built with `-ss`. On open GOPs the
reference itself shifts, for the same reason as #6, and **a correct cut gets reported
as wrong**. Decode from the beginning and slice by frame number.

### 9. Never plan a re-encode window with no picture in it

The `tail` runs from where the copy's coverage ends to the end of the range, `t_out`.
Those two are usually a few frames apart -- but where `t_out` lands just after an
access point (60.060 s asked for on a 29.97 fps stream whose picture sits at
60.05996 s), the gap is thinner than the spacing between two pictures.

Such a window **has no picture to decode**. The cutter refuses a window it can decode
nothing out of -- otherwise a re-encode that produced no frames would pass in silence
-- so **the run stops**, not just that clip.

A tail is therefore written only when it is at least half a frame wide, for the same
reason the head has to be a whole one; the Python reference draws the line in the
same place. As a net under both, a re-encode segment whose frame count works out to
zero is not emitted at all.

### 10. The picture order counts either side of a splice are not one another's

A decoder hands its pictures back in picture-order-count order, and the counts on
the two sides of a seam were written by different encoders. The re-encoded `head`
opens a coded video sequence of its own and counts from nought; the copied `body`
spliced on after it carries the counts the recording gave it.

Where the copied segment begins on an **IDR** that settles itself: the sequence
restarts and the decoder empties what it was holding. But a pressed Blu-ray mostly
puts the other kind of entry point there — an I picture with a recovery point, which
restarts nothing — and where the head's last count lands above the copied picture's,
the two come out of the decoder the wrong way round: one picture of the outgoing
scene handed back a frame *after* the incoming one. On one disc, a set of twelve cuts
had five such pictures.

**Nothing is decoded wrongly.** The pictures after a recovery point decode exactly,
checked against the recording itself; only the order one of them comes out in is
wrong. `--clean-joins` spends a little more re-encoding to reach an IDR instead,
which on that disc took the five to two — the two being joins with no IDR within
reach. What it costs is exactness: the stretch between the two entry points stops
being copied and becomes a re-encode, measured at 51 dB against a decode of the same
pictures with their own references. That is a good re-encode and it is still not the
recording, so this is off unless it is asked for.

How far it is worth reaching is the other half. `examples/idrdiag.rs` walks a
recording and prints the wait for a clean entry point from each of its own, and the
four H.264 discs to hand disagree completely: two wait a second or three at the
median, and one holds a **single** IDR across a twenty-six minute title, where the
median wait is thirteen minutes. So the reach is bounded at two seconds, the plan is
left exactly as it was wherever nothing clean is within that, and material that
reorders nothing — MPEG-2 and VC-1, which state each picture's display order within
its own group — never moves at all.

Reading this needs the recording in hand, and on a disc the index was never walked:
it came off the disc's own table. So planning gained an entry point that has the
recording, `plan_on`, while `plan` stays the arithmetic. It costs a handful of seeks
rather than a pass — the byte each entry point begins at is already known.
