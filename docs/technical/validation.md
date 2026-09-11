# Validation and known limits

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](validation.ja.md)

## Validation results

`tests/run_tests.sh` decodes the output, matches it against the source's frame
hashes, and checks the frame count, the alignment, and how many frames are
bit-exact.

| Case | Lossless copy ratio |
|---|---|
| H.264 single interval | 180/222 (81.1%) |
| H.264 multiple intervals | 300/336 (89.3%) |
| H.264 middle removed (through to the end) | **540/540 (100%)** |
| H.264 exactly on access points | **180/180 (100%)** |
| H.264 interval shorter than a GOP | 0/21 — falls back to a full re-encode |
| HEVC | 300/342 (87.7%) |
| H.264 29.97 fps | 300/342 (87.7%) |
| H.264 open GOP (referenced leading pictures) | 0/342 — rejected as a start point, which is the correct behaviour |
| **MPEG-2 TS open GOP** | **328/342 (95.9%)** |
| MPEG-2 TS multiple intervals | 296/299 (99.0%) |
| MPEG-2 TS through to the end | 447/449 (99.6%) |
| Matroska output | 180/222 (81.1%) |

All 13 cases agree on both frame count and alignment.

## Validation against real material

Synthetic fixtures hide certain problems, so validation also runs against actual
broadcast recordings. `tests/verify_real.py <src> <out> <ranges>` checks frame count,
alignment, bit-exact ratio, timeline, interlacing and A/V length difference in one
go.

| Material | Result |
|---|---|
| Terrestrial NHK E-Tele (MPEG-2 1440x1080i, true 29.97) | **899/899, 98.2% lossless, timeline matches, interlacing preserved, A/V 2.0 ms** |
| BS11 (MPEG-2 1920x1080i) | **899/899, 98.2% lossless, same as above** |
| AT-X (MPEG-2 1440x1080, **2:3 pulldown**) | **719/719, 99.9% lossless, 2:3 pattern preserved, A/V 2.0 ms** |
| A pressed Blu-ray in **VC-1** (1920x1080i animation), 10 s mid-GOP to mid-GOP | **308/308, 90% of the video byte-identical**, the partial GOPs written afresh at 48 dB |
| A pressed Blu-ray in **VC-1** (1920x1080p film, heavy grain), the same 10 s | **246/246, 90% byte-identical**, written afresh at 45 dB with the grain intact |
| H.264 720p from YouTube (29.24 fps) | 878 frames, A/V 2.6 ms |
| VP9 + Opus (webm→mp4) | Passes if the plan is copy-only |

**A VC-1 cut has to be checked differently.** `verify_real.py` lines the two files up
by frame number, and a frame number is exactly what a piece of a Blu-ray does not
have: it begins mid-GOP, the decoder drops what it cannot decode, and everything after
that is off by however many pictures that was.

So [`tests/run_vc1_tests.sh`](../../tests/run_vc1_tests.sh) checks the bytes instead,
in two ways. First, that a long run of the cut appears **verbatim** in the recording,
which needs no alignment at all. Second, that the pictures SmartCut wrote itself come
back out of the decoder every player uses at a measured distance from the ones they
replaced. Both checks are needed: a subtly malformed bitstream still decodes, and a
wrongly scaled transform decodes without complaint into a picture of the wrong
brightness. The encoder and what it costs are in
[the Rust core](rust-core.md#vc-1-the-codec-with-no-encoder).

Everything a real transport stream can throw at you turned up along the way:

- **A `start_time` of 29288 seconds**, because PCR is based on wall-clock time
- **Every access point an open GOP** — 776 out of 776, with droppable leading
  pictures
- **Missing frames** — 28 of them in a 668 MB recording, from dropouts
- **Multiple coexisting streams** — ARIB subtitles, data broadcasting and so on

### How many audio frames were re-encoded

`tests/run_aac_tests.sh` applies the same standard the video is judged by to the
audio: walk the output's ADTS frame by frame and compare each one with the
recording's own bytes. The material is a Nippon TV recording, two intervals, four
boundaries.

| Mode | Container | Frames verbatim | Re-encoded | ADTS |
|---|---|---|---|---|
| `copy` | TS | **5606/5606 (100%)** | 0 | all MPEG-2 LC |
| `smart` | TS | **5602/5606 (99.929%)** | **4** | all MPEG-2 LC |
| `smart` | MP4 | 5602/5606 (payloads) | 4 | — |
| `reencode` | TS | 0/5607 | 5607 | all MPEG-2 LC |
| `smart --aac mpeg4` | TS | 5602/5606 | 4 | **all MPEG-2** (see below) |
| `reencode --aac mpeg4` | TS | 0/5607 | 5607 | all MPEG-4 |
| `smart`, **a commercial block cut out**, all four boundaries in silence | TS | **1392/1392 (100%)** | **0** | all MPEG-2 LC |

The last row is what an ordinary cut looks like, and there the output is
**byte-identical to `copy`**.

As long as frames are being copied, asking for a flavour of AAC that the recording
does not carry cannot be honoured: the result would be a stream that is two kinds of
AAC at once. The request is refused with a note, and the recording's own version is
used instead. A whole-track re-encode copies nothing, so there the request is
honoured; that is the `reencode --aac mpeg4` row.

An MP4 keeps the payloads and throws the ADTS framing away, so payloads are what can
be compared there. Same payload, same sound.

### A bug found on real material: interlacing lost in re-encoded regions

Only the re-encoded partial GOPs came out with `interlaced_frame=0`, so the combing
disagreed with the copied parts. The cause was failing to pass
`AV_CODEC_FLAG_INTERLACED_DCT` and `INTERLACED_ME`, and the field order, to the
encoder. On broadcast material this is fatal, because picture quality changes at
every cut point. Fixed.

### 2:3 pulldown support (a field-based timeline)

Most anime carries 24 fps film material in a 29.97 stream via `repeat_first_field`.
The decoded pictures therefore arrive with **alternating intervals of 0.0334 s and
0.0500 s** — per picture, the stream is not CFR.

Measured across the library, **13 of 40 files (32%)** were pulldown:

| Station | Pulldown ratio |
|---|---|
| AT-X | 8/14 (57%) |
| Disney Channel | 2/2 (100%) |
| BS Animax | 3/10 (30%) |
| BS11 / BS-TBS / BS Nittele / NHK / Kids / Tele-Asa ch2 | 0% |

Too common to ignore, so it is handled. The key is to **drop the unit of the output
timeline from the frame to the field**. Pulldown alternates two fields and three
fields, so on a field grid it is expressible in integers. The output time base is
`1/(2 × fps_numerator)` — 1/60000 for 29.97 — which makes one field exactly
`fps_denominator` ticks.

As a side effect, the way DTS is built had to change too. Summing each picture's
duration in decode order overtakes presentation once field durations vary, and the
muxer rejects it with `pts < dts`. It was replaced with the correct construction,
deriving DTS from the position in display order.

### The planner's phase problem (resolved)

Interval boundaries used to be snapped to an ideal grid, `round(t*fps)/fps`. That
disagreed with the real stream's frame phase — 0.010 s off on the test material — and
dropped one frame at the interval edges.

**Removing the snapping fixed it.** Segment durations are now measured from the
pictures actually written rather than from the planner's arithmetic, so there is
nothing left to align to a grid. Each segment is placed relative to its own first
picture, and the next segment starts after the length the previous segment actually
occupied. That follows both the phase and the pulldown automatically.

### The verification side had bugs of its own

These only surfaced on real material:

- **The ground truth was being sliced by frame number.** Real recordings drop frames,
  so `frame number = time × fps` does not hold, and the comparison target was 27
  frames off — which looked like "0% match". Fixed to work from PTS.
- **The time origin differed.** Cuts are relative to the *presentation* timeline (the
  format's `start_time`, the position a player shows as 00:00), but ffmpeg's decoded
  output is relative to the *video stream's* `start_time`. In broadcast recordings
  audio starts first, so the two differ by 0.346 s. A correction was added.

## Known limitations

- **A damaged recording loses the pictures inside the damage, and not the cut.** A
  broadcast has holes in it — a burst of noise, a dish that lost the satellite for a
  moment — and three things follow from one. libavcodec refuses the packets the damage
  falls inside; the display positions of the pictures they carried are then missing,
  which widens the gap the derived DTS has to clear; and the timestamps around the
  damage stop climbing, so two sound frames can claim one instant. Each of the three
  used to stop the whole cut, on a recording that was otherwise perfectly cuttable.
  Now the packets that will not decode are dropped, a picture with nowhere to go on
  the timeline is left out, and a sound frame that does not follow the one before it
  is left out; a caption statement that lands on a tick already taken is moved on by a
  tick rather than dropped, because a statement is a line of the programme and a tick is
  a ninetieth of a millisecond. Each is counted and said once when the cut finishes.
  What is copied is untouched by any of it. Measured over 82 recordings from 32
  stations, one was damaged badly enough to reach all three paths.
- **The first frame is 13 ms early** (Python implementation only). A raw elementary
  stream carries no timestamps at all — every packet is `N/A` — so ffmpeg synthesises
  them from `-r` and the POC. In doing so, the first `has_b_frames` packets come out
  with no PTS. Only the MP4 muxer tolerates that (`-avoid_negative_ts make_zero`);
  Matroska and MPEG-TS reject it, which is why MKV output is remuxed via MP4. From the
  second frame on the spacing is perfectly uniform. This does not happen in the libav
  implementation, which assigns PTS/DTS per packet itself.
- **An H.264/HEVC open GOP whose leading pictures are reference pictures cannot be
  used as a copy start point** (see [pitfall 3](algorithm.md#3-leading-pictures--the-heart-of-the-open-gop-problem)).
  This is inherent: avoiding it would mean keeping the leading pictures in the
  bitstream and hiding them with an edit list, which the elementary-stream
  concatenation approach cannot express. It is a non-issue on material with regular
  IDRs, which covers most broadcast H.264.
- **A copy spliced onto an entry point that is not an IDR can hand back one picture
  out of order** (see [pitfall 10](algorithm.md#10-the-picture-order-counts-either-side-of-a-splice-are-not-one-anothers)).
  Nothing is decoded wrongly — the pictures after a recovery point were checked
  against the recording itself — but the counts the two sides of the seam are ordered
  by were written by different encoders, so one picture of the outgoing scene can be
  handed back after the incoming one. On the disc it was found on, twelve cuts
  produced five such pictures; `--clean-joins` took that to two by reaching for an
  IDR instead, and the two are joins with no IDR within two seconds. It is off by
  default because it costs exactness: the stretch it re-encodes measures 51 dB
  against a copy of the same pictures, which is bit-exact. Material that reorders
  nothing — MPEG-2, VC-1 — is unaffected.
- **The Python leading-picture reference test samples one place in the file** and
  applies the result to the whole thing, assuming the encoder does not change its mind
  partway. The Rust implementation does not need this, since `nal_ref_idc` can be read
  directly while demuxing.
- **H.264 / HEVC pulldown (the SEI `pic_struct`) is not detected.** Japanese broadcast
  H.264 is CFR, so no actual harm has been observed, but unlike MPEG-2 it is not
  detected.
- **The Python reference implementation still snaps to the ideal grid** and therefore
  still has the phase problem (`mpeg2 ts multi` in `tests/run_tests.sh` is an xfail).
  It still serves as a test oracle, but the Rust implementation is ahead of it.
- **Supported codecs are H.264 / HEVC / MPEG-2 / MPEG-4 Part 2 / VC-1.** VP9 and AV1
  have no elementary-stream concatenation form and would need a different design.
- **Dolby Vision survives a copy but not a re-encode.** The RPU on every picture is
  copied with it, so a range whose ends fall on the recording's own entry points comes
  through with all of them; the pictures rewritten at a seam have none, because
  libavcodec will only configure libx265 for the profiles it can write. The cut says so
  and takes the recording's stream-level Dolby Vision claim off the output with it, so
  nothing is left saying what the pictures cannot back up. Measured on a profile 4
  recording, which libavcodec refuses outright; its GOPs are 4.2 seconds, so a range
  that misses an entry point re-encodes a long way. See
  [the Rust core](rust-core.md#dolby-vision-is-in-the-pictures-and-cannot-be-written-back).
- **An HLG transfer written the backward-compatible way is only kept for HEVC.** The
  sequence header says `bt2020-10` and an SEI beside it says HLG; both are reproduced
  across a seam by telling libx265 `atc-sei`, which libx264 has no equivalent of. No
  H.264 recording signalling HLG this way has been measured here.
- **A VC-1 partial GOP is written by SmartCut's own encoder, and it writes intra
  pictures only** ([the Rust core](rust-core.md#vc-1-the-codec-with-no-encoder)).
  There is no VC-1 encoder in libavcodec to use instead. Pictures cost more bits than
  the predicted ones they stand in for — still fewer than the disc's own I pictures —
  and two things in the format are refused by name rather than written wrongly: a
  pan-scan window, and a quantiser that varies at the picture edges (`DQUANT=2`).
  Field pictures are read but written back as interlaced frames.
- **One video track only**, and in the Python reference implementation one audio track
  only. The Rust engine reads every sound track the recording carries and writes them
  all, and carries both kinds of subtitle across when writing a `.ts`: the ARIB
  caption stream a broadcast sends, and the PGS a disc draws (see
  [audio](audio.md#multi-audio-broadcasts),
  [broadcast workflow compatibility](broadcast-ts.md) and
  [the subtitles a disc draws](disc.md#the-subtitles-a-disc-draws)). A DVD's
  subtitles have no stream type a transport stream can carry, so they are
  converted into a Blu-ray's kind and written inside it, which is the default;
  either disc's can be written beside the cut instead — as a VobSub pair, or
  as a `.sup` — which is the one way an `.mp4` of a disc keeps them
  ([above](disc.md#a-discs-subtitles-inside-the-cut-or-beside-it)).
  **Blu-ray menus (IGS) and text subtitles (TextST) are not carried**, and are
  named as left behind rather than dropped in silence: a menu's buttons point
  into a disc structure a cut does not have, and TextST is set in a typeface
  that lives on the disc rather than in the stream
  ([above](disc.md#a-menu-is-not-a-stream-and-the-index-is-the-only-thing-that-knows)).
- **The audio boundary is still rounded to a whole frame in every mode but
  `reencode`.** What `smart` removes is the audio from the far side of a cut being
  left in the seam; it does not change an interval being a whole number of frames
  long. As long as no container can put two frames at one instant, a seam gains up to
  10.7 ms of silence or loses up to 10.7 ms of sound.
- **The frames written here carry no ADTS CRC** (`protection_absent = 1`). It is a
  per-frame field, so they sit legally among frames that have one, but they are not
  byte-for-byte the same shape as the recording's.
- **`smart` reaches every lossy codec there is an encoder for, and no further.** AAC,
  AC-3, E-AC-3, MP2 and Blu-ray LPCM are smart rendered; DTS and TrueHD are lossless
  and are carried through byte for byte on purpose, and a codec with no encoder in this
  build is copied with a note saying so — on that material the default behaves exactly
  as `copy` does. What used to stop at AAC was the encoders' delay: only AAC's is a
  whole frame, and the others' packets fell between the recording's. They are fed a
  lead-in now, which puts them back on the grid
  ([Audio](audio.md#which-codecs-it-reaches)).
- **`--aac` reaches only the frames SmartCut writes.** Copied frames keep their own
  ADTS headers, so material cannot be converted from one flavour to the other: the
  payload may use tools the other version does not have, and flipping the bit alone
  would be a lie. A request that disagrees with the recording is refused with a note
  rather than producing a mixed stream — except under a whole-track re-encode, where
  nothing is copied and it is honoured.
