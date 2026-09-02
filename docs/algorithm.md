# Algorithm and pitfalls

[← Documentation](README.md) ・ [← SmartCut](../README.md) ・ [日本語](algorithm.ja.md)

## The algorithm

For a kept interval `[t_in, t_out)`:

```
... I ....... I=========================I ....... I ...
      ^t_in   ^k_first                  ^k_term   ^t_out
    |<-head->|<--------- body --------->|<-tail->|
      re-encode        stream copy       re-encode
```

`head` and `tail` begin or end partway through a GOP, so there is no choice but
to decode from the previous access point and rebuild them. The `body` between
them is emitted as the input's own bytes. Cut exactly on access points and there
is no re-encoding at all.

- [`probe.py`](../smartcut/probe.py) — stream parameters, the access point index,
  leading-picture detection and the reference test
- [`planner.py`](../smartcut/planner.py) — intervals to segment list
- [`bitstream.py`](../smartcut/bitstream.py) — Annex-B / MPEG-2 access unit splitting
- [`renderer.py`](../smartcut/renderer.py) — running ffmpeg and concatenating
- [`verify.py`](../smartcut/verify.py) — decoding the output and comparing it against
  the source frame by frame

## Pitfalls

Why "just cut on GOP boundaries and join" is not enough. Every one of these was
hit for real while building the prototype, and every one has a test pinning the
reproduction.

### 1. The parameter sets (SPS/PPS) do not match

Making the re-encoded part's SPS match the original stream bit for bit is
effectively impossible — a different encoder means different VUI and different
VBV. And yet an MP4 `avcC` box, like a Matroska CodecPrivate, **can only hold one
set of parameter sets**. Concatenate naively and either the copied part or the
re-encoded part gets decoded with the wrong SPS, and the picture falls apart.

The fix has two parts:

- Write each piece as a **raw Annex-B elementary stream** and join them by plain
  byte concatenation. An ES carries SPS/PPS in band at every IDR, so parameter
  sets that differ from piece to piece can legally coexist.
- Write the final MP4 with an **`avc3` / `hev1` sample entry**. That is the
  in-band form defined by ISO/IEC 14496-15, and it is not folded into `avcC`.

MPEG-2 does not have this problem in the first place: its sequence headers are
in band already.

### 2. The access point index has to scan *packets*

`ffprobe -skip_frame nokey` **misses access points in open GOPs**, because the
decoder cannot output an I picture whose references are absent. On the
prototype's test material it found only 3 of the 10 access points that were
actually there.

Looking at the packet's `K` flag avoids decoding entirely: it is faster, and it
is correct.

### 3. Leading pictures — the heart of the open-GOP problem

Pictures that come **after an I picture in decode order but before it in display
order** (leading pictures) reference the previous GOP. Start a copy there and
they cannot be displayed.

And here the paths diverge. The handling is the exact opposite depending on
**whether the leading picture is itself a reference picture**:

- **MPEG-2**: B pictures are never referenced. → Leading pictures can be
  dropped. Even an open GOP works as a copy start point.
- **H.264 / HEVC** (x264's `open-gop` and friends): B pyramids mean a leading
  picture can be a reference picture. → **Drop it and every later frame that
  referenced it breaks, taking the whole GOP with it.**

This was confirmed by measurement. Dropping leading pictures on x264 open-gop
material made all 60 frames of the first GOP of the copy region mismatch; keeping
them, they matched.

So `nal_ref_idc` (H.264) / the NAL type (HEVC) / `picture_coding_type` (MPEG-2)
are read out of the bitstream to **decide whether the picture is referenced**,
and if it is, that point is not used as a copy start point. As a copy *end*
point an open GOP is fine either way — the display range simply ends at
`lead_start`.

Removing leading pictures cannot be expressed in the container (it would need an
edit list), so the Annex-B access unit boundaries are parsed by hand and the
pictures cut out there ([`bitstream.py`](../smartcut/bitstream.py)). Note that
H.264's "first slice = start of picture" does not carry over to MPEG-2, where a
picture start code is followed by several headers before the slices, so the AU
split differs.

### 4. Specify intervals in frames and packets, not seconds

Pass `-t` a duration in seconds and it goes wrong twice over.

- Under `-c copy`, `-t` is evaluated against the **DTS**. DTS runs ahead of
  presentation time by the reorder depth, so extra I/P pictures from the next GOP
  sneak in (180 frames came out as 182).
- With fractional frame rates (30000/1001), rounding shifts the result by ±1
  frame.

In the end the only robust approach was to **count the display frames and the
packets to copy as integers and pass those** (`-frames:v N`). The packet count
follows exactly from the difference of decode-order indices in the access point
index.

### 5. Container start_time (MPEG-TS)

TS timestamps do not start at 0 (1.423 s on the test material). `-ss` is relative
to the start of the file, but ffmpeg re-bases the output by the `-ss` value
alone, so **start_time survives as a residual offset in the output timeline**.
A seconds-based `-t` takes that hit head on. Access point times are normalised by
subtracting start_time, and interval lengths are passed as frame counts, which
avoids both problems.

### 6. Re-encoded regions must be decoded *from earlier*

With open-GOP sources, seeking straight to the target position with `-ss` makes
ffmpeg discard the entire GOP it could not decode, so **the output starts up to
one GOP late** (0.2 s, measured). Decoding starts from an access point a few GOPs
earlier and the front is trimmed with an output-side `-ss`.

### 7. Audio has no GOP structure

Audio is cut **per kept interval**, not per video segment.

- `--audio-mode copy`: the source frames are used as they are. Interval
  boundaries snap to the nearest audio frame (up to ~24 ms for AAC).
- `--audio-mode reencode`: one pass through the `atrim` + `concat` filters.
  Sample accurate, but it re-encodes.

The Rust core adds a third, `smart`, which is its default: the frames a
boundary falls inside are re-encoded and the rest copied. See
[the Rust core](rust-core.md#smart-rendering-applied-to-audio---audio-mode-smart).

Handling AAC encoder delay / priming strictly via edit lists has not been
implemented.

### 8. The verification reference decodes from the start of the file

The reference side of `--verify` must not be built with `-ss`. On open GOPs the
reference itself shifts (for the same reason as #6), and **a correct cut gets
reported as wrong**. Decode from the beginning and slice by frame number.
