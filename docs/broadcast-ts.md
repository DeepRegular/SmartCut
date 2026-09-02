# Broadcast workflow compatibility

[← Documentation](README.md) ・ [← SmartCut](../README.md) ・ [日本語](broadcast-ts.ja.md)

## The output TS inherits the recording's layout

**Tools built around broadcast recordings, DGIndex among them, read the PIDs, the
PMT, the service number and the descriptors before they read a single frame.** If
those differ from the recording, the file either does not open or the audio track
cannot be found. Lined up against the source, the early output looked like this:

| | PMT PID | Video PID | Audio PID | Audio descriptors |
|---|---|---|---|---|
| Source (BS Fuji) | 0x100 | 0x1100 | 0x1101 | `0x52` `0x0a` |
| Output (originally) | 0x1000 | 0x100 | 0x101 | **none** |
| Output (now) | 0x100 | 0x1100 | 0x1101 | `0x0a` |

**The PIDs are derived from the video stream's.** Not from "the lowest PID in the
file" — a broadcast recording carries subtitles and data broadcasting as *streams*
too, and their PIDs sit below the range the muxer accepts (0x0020–0x1FFA). The
first attempt picked one of those up, passed 18 as `mpegts_start_pid`, and
produced a 0-byte output.

`0x52` (component_tag) is the one thing that does not get written, because
ffmpeg's muxer does not write it.

### The muxer options had never been arriving

This came out while fixing the above. `output_with()` in `ffmpeg-next` passes the
dictionary you give it to the **protocol layer** (`avio_open2`) only; it never
reaches the muxer's private options. Muxer options have to be passed to
`write_header` (`write_header_with()`). Which means that everything we thought we
were setting, `video_track_timescale` included, **was being silently discarded**.
None of it shows up in a frame comparison, hence `tests/run_ts_layout_tests.sh`.

## The re-encoded head claimed to be "59.94 fps"

This one surfaced from a report that the frame rate did not line up in
DGIndex → AviSynth (MPEG2Dec). **Cut anywhere other than a lossless point and the
head becomes a re-encoded region — and we are the ones writing its sequence
header.** It disagreed with the source:

```
source   frame_rate_code=4 (30000/1001)
output   frame_rate_code=7 (60000/1001)   <- only the re-encoded head
```

**Indexing tools trust the first sequence header they meet for the whole file**,
so a 29.97 recording was being handed downstream as 59.94.

The cause was the unit of the output timeline. The field-based time base (one
tick = one field) was being handed to the encoder as it was, and **MPEG-2's frame
rate code is derived from the time base**, so it doubled. Setting `framerate`
separately has no effect, because mpeg12enc looks only at the time base. Fixed by
giving the encoder a **picture-based time base**, numbering the pictures we hand
it, and **keeping separately track of where on the timeline each number lands and
how many fields it occupies**. The field-based timeline on the output side is
unchanged.

It is not the kind of thing a frame-hash comparison reveals, so
`tests/run_ts_layout_tests.sh` gained a check that the re-encoded head's sequence
header matches the source.

## Audio can be written out on its own as `.aac`

In a DGIndex → x264 → mux workflow, the audio reaches the muxer as a **bare ADTS
file**. There is a case where the step producing that `.aac` turned out to be the
weak link: **the `.aac` DGIndex produced started with `FF F8 04 22`** — the sync
word is right, but what follows declares "Main profile / 88.2 kHz / 0 channels",
which is not a thing that can exist. L-SMASH builds its AudioSpecificConfig from
that first frame, so it gives up before reading anything
(`failed to find the matched importer`).

The frames that were copied do not differ from the source by a single byte:

```
source              ff f8 4c a0  MPEG-2 with CRC profile=LC rate=48000 ch=2
copied frame        ff f8 ...    the recording's own bytes, by construction
frame written here  ff f9 ...    MPEG-2, no CRC
```

The decisive observation was that **it only breaks on a frame that is written
rather than copied**. On copy the broadcast's own bytes go through, so the ADTS
stays `ff f8` (MPEG-2, with CRC) — the form Japanese broadcast AAC always takes.
Hand the same frame to ffmpeg's ADTS writer and it **hard-codes** `ff f1`
(MPEG-4, no CRC). The header length changes from 9 bytes to 7, so a tool that
expects ARIB and goes looking at 9 bytes misreads the configuration — and
`04 22` (Main / 88.2 kHz / 0 ch) is exactly what "could not read it" defaults
to. Those two bits cannot be changed from the ffmpeg side.

So they are not written from the ffmpeg side any more. `adts.rs` builds the
header here, and a frame this tool writes announces the version the recording
announces: a broadcast is MPEG-2 AAC, and so is the seam
([the Rust core](rust-core.md#smart-rendering-applied-to-audio---audio-mode-smart)).
What is still not the recording's shape is the CRC — `protection_absent` is set
on the frames written here, so their header is 7 bytes rather than 9. That is a
per-frame field, so they sit legally among frames that carry one, but they are
not byte-for-byte what the recording would have had. `--aac mpeg2|mpeg4`
overrides the version, and while frames are being copied a request that
disagrees with the recording is refused rather than producing a stream that is
two kinds of AAC at once.

How many such frames there are is the mode's business. Under `smart`, the
default, it is the frames a boundary falls inside — four out of 5606 on a
two-interval cut, and none at all when the seam falls in silence, as a
commercial cut does. Under `reencode` it is all of them, and under `copy` none.

`write_audio_es()` (`--audio-es` on the CLI) **reads the finished output back**
and writes the audio out as ADTS. What ends up next to the video is by
construction the very audio inside it, and it has been confirmed to pass through
L-SMASH's `muxer` (`Track 1: MPEG-4 Audio`, 48 kHz stereo, matching duration).

This too is **kept out of the window**: the GUI offers neither it nor the audio
modes, and leaves the audio smart-rendered as the engine's own default has it.
What made the workaround worth having — a seam declaring itself MPEG-4 in the
middle of an MPEG-2 stream — is dealt with where it arises now.

## To hand it to L-SMASH, use the bare stream

A report said that feeding the output to L-SMASH's `muxer` stops with
`[importer: Error]: failed to find the matched importer.`, so it was checked.
**Feeding it the source `.ts` gives the same error** — `muxer` reads **bare
streams** only (Annex-B H.264/H.265, ADTS AAC, AC-3 and so on), and a transport
stream is out of scope from the start. There is nothing wrong with the output's
AAC.

What was confirmed:

| What was passed | Result |
|---|---|
| The output `.ts` directly | `failed to find the matched importer` |
| **The source `.ts` directly** | **The same error** |
| `.aac` extracted from the output (ffmpeg) | `Track 1: MPEG-4 Audio` — works |
| `.aac` extracted from the output (naive concatenation of the PID's PES) | works |
| `.aac` extracted from the source | works |

The output's PMT carries the same stream_type as the source, `0x0F` (ADTS AAC),
and the leading bytes are the `ff f8` sync word. Extraction works even with a
naive PID dump.

```bash
ffmpeg -i cut_xxx.ts -map 0:a:0 -c copy cut_xxx.aac   # hand this to muxer
```

Note that **L-SMASH does not handle MPEG-2 video** (`remuxer` says
`no support to remux this stream`), so this material cannot be turned into MP4 by
L-SMASH alone in the first place. The assumed workflow has a separate step
converting the video to H.264, and carries only the audio across.
