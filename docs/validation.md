# Validation and known limits

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](validation.ja.md)

## Validation results

`tests/run_tests.sh` — decodes the output, matches it against the source's frame
hashes, and checks the frame count, the alignment, and how many frames are
bit-exact.

| Case | Lossless copy ratio |
|---|---|
| H.264 single interval | 180/222 (81.1 %) |
| H.264 multiple intervals | 300/336 (89.3 %) |
| H.264 middle removed (through to the end) | **540/540 (100 %)** |
| H.264 exactly on access points | **180/180 (100 %)** |
| H.264 interval shorter than a GOP | 0/21 — falls back to a full re-encode |
| HEVC | 300/342 (87.7 %) |
| H.264 29.97 fps | 300/342 (87.7 %) |
| H.264 open GOP (referenced leading pictures) | 0/342 — rejected as a start point (correct behaviour) |
| **MPEG-2 TS open GOP** | **328/342 (95.9 %)** |
| MPEG-2 TS multiple intervals | 296/299 (99.0 %) |
| MPEG-2 TS through to the end | 447/449 (99.6 %) |
| Matroska output | 180/222 (81.1 %) |

All 13 cases agree on both frame count and alignment.

## Validation against real material

Synthetic fixtures hide certain problems, so validation also runs against actual
broadcast recordings. `tests/verify_real.py <src> <out> <ranges>` checks frame
count, alignment, bit-exact ratio, timeline, interlacing and A/V length
difference in one go.

| Material | Result |
|---|---|
| Terrestrial NHK E-Tele (MPEG-2 1440x1080i, true 29.97) | **899/899, 98.2 % lossless, timeline matches, interlacing preserved, A/V 2.0 ms** |
| BS11 (MPEG-2 1920x1080i) | **899/899, 98.2 % lossless, same as above** |
| AT-X (MPEG-2 1440x1080, **2:3 pulldown**) | **719/719, 99.9 % lossless, 2:3 pattern preserved, A/V 2.0 ms** |
| H.264 720p from YouTube (29.24 fps) | 878 frames, A/V 2.6 ms |
| VP9 + Opus (webm→mp4) | Passes if the plan is copy-only |

Everything a real TS throws at you turned up:

- **A `start_time` of 29288 seconds** (PCR is based on wall-clock time)
- **Every access point an open GOP** (776 out of 776; the leading pictures are
  droppable)
- **Missing frames** (28 of them in a 668 MB recording, from dropouts)
- **Multiple coexisting streams** — ARIB subtitles, data broadcasting and so on

### A bug found on real material: interlacing lost in re-encoded regions

Only the re-encoded partial GOPs came out with `interlaced_frame=0`, so the
combing disagreed with the copied parts. The cause was not passing
`AV_CODEC_FLAG_INTERLACED_DCT` / `INTERLACED_ME` and the field order to the
encoder. On broadcast material this is fatal, because picture quality changes at
every cut point. Fixed.

### 2:3 pulldown support (a field-based timeline)

Most anime carries 24 fps film material in a 29.97 stream via
`repeat_first_field`. The decoded pictures therefore arrive with **alternating
intervals of 0.0334 s and 0.0500 s** — per picture, the stream is not CFR.

Measured across the library, **13 of 40 files (32 %)** were pulldown:

| Station | Pulldown ratio |
|---|---|
| AT-X | 8/14 (57 %) |
| Disney Channel | 2/2 (100 %) |
| BS Animax | 3/10 (30 %) |
| BS11 / BS-TBS / BS Nittele / NHK / Kids / Tele-Asa ch2 | 0 % |

Too common to ignore, so it is handled. The key is to **drop the unit of the
output timeline from the frame to the field**. Pulldown alternates two fields and
three fields, so on a field grid it is expressible in integers. The output time
base is `1/(2 × fps_numerator)` (1/60000 for 29.97), which makes one field
exactly `fps_denominator` ticks.

As a side effect, **the way DTS is built had to change too**. Summing each
picture's duration in decode order overtakes presentation once field durations
vary, and the muxer rejects it with `pts < dts`. It was replaced with the correct
construction, deriving DTS from the position in display order.

### The planner's phase problem (resolved)

Interval boundaries used to be snapped to an ideal grid, `round(t*fps)/fps`. That
disagreed with the real stream's frame phase (0.010 s off on the test material)
and dropped one frame at the interval edges.

**Removing the snapping fixed it.** Segment durations are now **measured from the
pictures actually written** rather than from the planner's arithmetic, so there
is nothing left to align to a grid. Each segment is placed relative to its own
first picture, and the next segment starts after the length the previous segment
actually occupied. That follows both the phase and the pulldown automatically.

### The verification side had bugs of its own

These only surfaced on real material:

- **The ground truth was being sliced by frame number.** Real recordings drop
  frames, so `frame number = time × fps` does not hold, and the comparison target
  was 27 frames off — which looked like "0 % match". Fixed to work from PTS.
- **The time origin differed.** Cuts are relative to the *presentation* timeline
  (the format's `start_time`, the position a player shows as 00:00), but ffmpeg's
  decoded output is relative to the *video stream's* `start_time`. In broadcast
  recordings audio starts first, so the two differ by **0.346 s**. Correction
  added.

## Known limitations

- **The first frame is 13 ms early.** A raw ES carries no timestamps at all
  (every packet is `N/A`), so ffmpeg synthesises them from `-r` and the POC. In
  doing so, the first `has_b_frames` packets come out with no PTS. Only the MP4
  muxer tolerates that (`-avoid_negative_ts make_zero`); Matroska and MPEG-TS
  reject it, which is why MKV output is remuxed via MP4. From the second frame
  on the spacing is perfectly uniform. This does not happen in the libav
  implementation, which assigns PTS/DTS per packet itself.
- **An H.264/HEVC open GOP whose leading pictures are reference pictures cannot
  be used as a copy start point** (#3). This is inherent: avoiding it would mean
  keeping the leading pictures in the bitstream and hiding them with an edit
  list, which the ES-concatenation approach cannot express. It is a non-issue on
  material with regular IDRs, which covers most broadcast H.264.
- The leading-picture reference test samples one place in the file and applies
  the result to the whole thing (assuming the encoder does not change its mind
  partway). The libav implementation does not need this, since `nal_ref_idc` can
  be read directly while demuxing.
- H.264 / HEVC pulldown (the SEI `pic_struct`) is not detected. Japanese
  broadcast H.264 is CFR, so no actual harm has been observed, but unlike MPEG-2
  it is not detected.
- The Python reference implementation still snaps to the ideal grid and therefore
  still has the phase problem (`mpeg2 ts multi` in `tests/run_tests.sh` is an
  xfail). It still serves as a test oracle, but the Rust implementation is ahead
  of it.
- Supported codecs are H.264 / HEVC / MPEG-2 / MPEG-4 Part 2. VP9 and AV1 have no
  ES-concatenation form and would need a different design.
- One video track and one audio track only. Subtitles and multiple audio tracks
  are not supported.
