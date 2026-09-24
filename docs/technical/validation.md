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
| MPEG-2 TS multiple intervals | 296/300 (98.7%) |
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
| **VP9 + Opus** 1080p23.98 from YouTube, two ranges, all four ends mid-GOP | **432/432, 54.9% byte-identical**, which is the share the plan promised to the tenth |
| **VP9 + Opus** 352x240 29.97, one 30 s range mid-GOP to mid-GOP | **900/900, 82.9% byte-identical**, decoded clean by libvpx and by libavcodec's own VP9 |
| **AV1 + Opus** 720p30, one 28 s range mid-GOP to mid-GOP | **840/840**, decoded clean by dav1d and by libaom |
| **Variable rate**, gaps 33 ms to 2.4 s, one 30 s range | **505/505 where the recording had them** (46 were a frame early before 0.8.0), and 30.0 s long (28.1 before) |
| **Variable rate**, gaps 16 ms to 117 ms, one 30 s range | **628/628 where the recording had them** (598 were out by up to three frames before 0.8.0) |
| **23.976 with a burst of 59.94** (a downloaded 24 min programme, H.264), three ranges | **6294/6294, no gap wrong by more than 0.6 ms** — 11 pictures were dropped outright before 0.8.0, and 227 gaps were wrong, the worst by 45 ms |
| The same, HEVC 10-bit 1440x1080, one 50 s range | **1259/1259**, worst gap 0.6 ms (was 8 dropped and a median displacement of 12 ms) |
| **AV1 10-bit + Opus**, a downloaded 23 min programme, 33 s GOPs, two ranges | **6712/6713**, every gap within 1 ms, decoded clean by dav1d |

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

### A recorder's own disc codes field pairs (a bug found on real material)

A SONY recorder's BD-RE writes 1440x1080 29.97 H.264 as **PAFF**: every frame is a
pair of field pictures, and the demuxer hands the two over separately. Nothing in the
broadcast corpus does this — broadcast 1080i is interlaced content coded as whole
frames — so it went unmet until a disc of recordings was read.

A forty-second cut of one lost **642 of the 1842 pictures** it wrote, and the copied
part decoded as blocky mush from its first picture on. Two causes, both from taking a
picture for a frame: each field was given two fields of the timeline instead of one,
and the writer's reorder queue, which counts frames, held one picture where the frame
needed two. See [pitfall 11](algorithm.md#11-a-recording-may-hand-over-two-pictures-per-frame).

With the queue measured in halves of a frame and the pair placed on two fields, the
same cut writes all 1842 and **every copied picture decodes bit-identically to the
recording** — 855 frames compared across a sixty-three second copy, with the only
differences in the re-encoded head, where they belong. The disc's older per-sequence
path comes out the same way.

Frame-coded material is untouched by any of it, and that was checked by byte rather
than by argument: the cut of a broadcast recording made before and after the change
is the same file, md5 for md5. That includes one recording which is frame-coded but
for seven field pairs in half a minute — libavcodec's MPEG-2 parser joins each pair
into one packet, so it is seven frames, not fourteen half-frames, and it comes out
exactly as it did.

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
- **An H.264 open GOP whose leading pictures are reference pictures cannot be
  used as a copy start point** (see [pitfall 3](algorithm.md#3-leading-pictures--the-heart-of-the-open-gop-problem)).
  HEVC is no longer one of these: its leading pictures are barred from being
  references for trailing pictures, so a copy may start at any of its entry points.
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
  nothing — MPEG-2, VC-1 — is unaffected. **A recorder's own disc cannot be helped
  at all**: `idrdiag` counts *one* IDR among the 1786 entry points of a
  fifteen-minute recording on one, so the median wait for a clean join is 446
  seconds and the two-second reach never finds anything. On a forty-second cut of
  it, libavcodec withholds the eighteen frames of the copy whose counts fall below
  the head's last — the pictures are there and decode exactly, and a decoder that
  does not reorder on output shows them.
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
- **Supported codecs are H.264 / HEVC / MPEG-2 / MPEG-4 Part 2 / VC-1 / VP9 / AV1.**
  The last two were written down here for a long time as having no elementary-stream
  concatenation form. They have one. Neither carries a parameter set: a VP9 key frame
  writes its own frame size and colour config into the uncompressed header, and all
  three AV1 encoders measured here (SVT-AV1, libaom, rav1e) emit a sequence header OBU
  ahead of every key frame — 10 headers for 10 key frames in the fixture counted. A
  sequence header may change at a key frame, which is exactly where a splice lands, so
  a partial GOP written afresh needs nothing patched between it and the copy that
  follows. `tests/run_vp9_av1_tests.sh` measures three things per codec: the frame
  count, a clean decode by the decoder players actually use rather than the one that
  wrote the seam, and the share of the output that is still the recording's own bytes.
- **Which AV1 encoder writes a seam is chosen by name, not by libavcodec.**
  `avcodec_find_encoder(AV_CODEC_ID_AV1)` answers libaom-av1, whose default `cpu-used`
  is 0: measured at 348 s for two seconds of 1080p24, which is not a seam being written
  but an export that looks hung. SVT-AV1 at preset 8 writes the same two seconds in
  3.97 s and lands within 0.002 dB of it. So `cut::encoders_for` names SVT-AV1, then
  rav1e, then libaom, and each is tried in turn — an encoder being present is not the
  same as its being able to write this recording, and SVT refuses 4:2:2 when it is
  opened rather than when it is looked up.
- **An AV1 level does not mean the same number at both ends.** A decoder puts the
  stream's `seq_level_idx` in `AVCodecContext.level` — 0 for level 2.0, counting up —
  and SVT-AV1 wants 20 for the same level, so the recording's own figure handed
  straight over produced `Level must be in the range of [2.0-7.3]` at every seam. For
  AV1 and VP9 the level is left for the encoder to work out; the codecs whose two ends
  agree are still told the recording's.
- **Every encoder ran on one core until 0.8.0.** `avcodec_alloc_context3` leaves
  `thread_count` at 1, not 0, and an encoder reads it literally, so seams were written
  on one core while the decode feeding them ran on four. It never showed on the codecs
  this started with — a seam of H.264 is a second of work either way, and the measured
  difference there is nil — but libvpx spent 6.56 s on two seconds of 1080p24 held to
  one core and 2.45 s given the machine.
- **H.264 and HEVC out of an MP4 or a Matroska file could not be written into a `.mkv`
  until 0.8.1.** Matroska keeps a length in front of each NAL, as an MP4 does, and the
  copied pictures were rewritten into a transport stream's start codes on the way in:
  everything but the re-encoded fringes was unreadable. A `.m4v` went the same way,
  being handed by its name to libavformat's `ipod` muxer (and HEVC was refused there
  outright); it is written as the MP4 it is now. So did a join of recordings framed
  differently — a transport stream and an MP4 in either order — whose second recording
  was copied in the first one's framing, pictures and AAC alike.
  `run_rust_tests.sh` checks every one of these pairings for decoder errors.
- **A broadcast whose second sound track belonged to the programme before it was
  read tens of times over, from 0.8.0 to 0.8.2.** Reads that look for one track stop
  by counting packets or by the clock, and 0.8.0 began throwing every other stream
  away before them. libavformat then returns nothing until it finds a packet of the
  track, and a track that is there for the first minute of the recording and never
  again is not found: each such read went on to the end of the file. Opening a
  two-and-a-half-hour recording took three hours instead of four minutes, and every
  seam of a cut read the file again. The pictures are kept for those reads now
  (`input::keep_with_pictures`), and a track with nothing in the ranges kept is left
  out of the cut with a note rather than failing it at the end.
  `run_bilingual_tests.sh` builds that shape and counts the bytes read.
- **Other reads could run to the end of the file, until 0.8.3.** The same shape of
  fault in three more places: a read that stopped on an error only at the end of the
  file (ffmpeg-next's packet iterator retries every other error at once and for ever,
  so a recording on a share that went away spun a core and could not be stopped);
  the look back for a DVD's or a Blu-ray's subtitles before each range, which on a
  track with nothing after the range's start read the rest of the title once per
  range; and a disc's entry-point map whose clock passed 2^33 inside a clip, whose
  entry points after that were all thrown away. Reads now go through
  `input::Packets`, which gives up after a run of failures, the look backs keep the
  pictures as their clock, and the map is unwrapped to match the stream libavformat
  hands over. A read that gave up is not the end of the file to a pass that writes:
  the cut stops with an error, rather than ending where the share went away and
  saying it had succeeded.
- **The map's unwrapping started again at every seam, in 0.8.4.** libavformat
  takes the first timestamp in the file, less a minute, as its reference for the
  whole file and never resets it. On a recording whose clock drops back low at a
  seam, the entry points of the stretch after it came out a day away from their
  pictures. The map is now unwrapped once, by the same rule.
- **Re-encoded sound was laid end to end, until 0.8.3.** Its packets were placed by
  the samples fed to the encoder, so every stretch the track did not have — a dropout
  in a broadcast, a track that starts after the pictures or stops before them, a
  joined recording without the track — moved everything after it earlier by as much.
  A hole is now filled with silence, and each range's sound starts where its pictures
  do. On a test recording with a two-second hole, the beeps after it came out two
  seconds early; they now land within 5 ms of where a copy puts them.
- **The logo and the scene signature read ten-bit pictures as eight-bit ones, until
  0.8.3.** A sample of HEVC Main10 is two bytes, and both passes read bytes: a corner
  at the right of the picture was read from near its middle. The logo strength of a
  test pattern came out 8413 at ten bits against 2325 at eight; it is 2305 now.
  `Luma8` reads either.
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
  Two things in the entry point are answered rather than refused. Where it allows
  overlap smoothing, pictures are held at PQUANT 8 or finer, because a decoder smooths
  every intra picture coarser than that and this encoder does not prepare for it
  (until 0.8.3 they were not: 37.9 dB against 42.3 now, on a patched progressive
  source). Range mapping (`RANGE_MAPY`/`RANGE_MAPUV`) needs nothing: libavcodec does
  not apply it ("Luma scaling is not supported"), so the pictures handed over are in
  the coded range and are written back in it. Narrowing them first was tried and
  measured at 17 dB.
- **One video track only**, and in the Python reference implementation one audio track
  only. The Rust engine reads every sound track the recording carries and writes them
  all, and carries every kind of subtitle across when writing a `.ts`: the ARIB
  caption stream a broadcast sends, the crawl it sends beside it, the TTML a 4K
  broadcast sends instead, and the PGS a disc draws (see
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
