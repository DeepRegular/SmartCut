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

### Every stream now goes back on the PID it arrived on

The table above is as far as "number from the video's PID" gets you. The
muxer reads `AVStream.id` as **the PID to write the stream on** -- anything
16 or over is used as it stands rather than numbered from `mpegts_start_pid`
-- so handing each stream the PID it had in the recording puts the sound and
the captions back where they were. That the audio matched under the old
scheme was luck: 0x1100 plus one happens to be 0x1101. A recording whose
PIDs are spread out (0x100f / 0x104f / 0x120f) did not match at all.

The descriptors are a different matter -- the muxer will not write those --
and they are restored by the pass below.

## Putting the recording's own tables back

> Since 0.3.1 the default output is the partial transport stream described
> below. This section is what `--tables broadcast` does -- and the machinery
> under it is what both shapes are built on.

A broadcast also talks about itself. Which service this is, what the station
is called, what is on now and what follows, what time it is -- PAT, PMT, SDT,
EIT, TOT. **That is what a recorder's library view, a player's channel
display and every downstream tool actually read.**

None of it survives a mux. libavformat writes its own PAT, PMT and SDT out of
what it knows, which is the streams and nothing else: the captions come out
with their ARIB descriptors because the muxer knows that codec, and
everything else -- the audio's component tag, the copy-control descriptor,
the superimpose stream's identity -- is simply **not written**. EIT and TOT
are worse than lost: ask the muxer to copy PID 0x12 and it accepts it as an
anonymous private stream and **puts it on a PID of its own choosing**, where
nothing will ever look for it.

So they are put back afterwards, by one pass over the finished file:

- **the PMT is rebuilt** with the recording's own descriptors,
- **the SDT is replaced with the recording's own section** -- which is how
  the service name arrives in ARIB's own character encoding without this
  program having to understand a byte of it,
- **EIT present/following and TOT are injected** on the PIDs they belong on,
- and every continuity counter is renumbered.

Where an injected section goes is decided by the output's own clock. Each
kept range uses a snapshot read at **the source byte** its opening picture
arrives at -- which the access-point index knows, and which is what makes
this cheap on a file of several gigabytes -- so a cut spanning a programme
boundary describes both programmes. The clock in the TOT is moved on as the
output runs; left as the snapshot it would name the same second for the
length of the file.

The conditional access descriptor (0x09) is the one thing not carried across.
The output is not scrambled and has no ECM stream, so restating it would be
describing a file that does not exist.

## What is put back is trimmed to what was written

The same reasoning is needed one step further along. The broadcast describes
the data broadcast and the superimposed crawl as streams it is sending, and a
cut carries neither. Copy the programme description across whole and **the
output announces an entry point into something that is not in the file** --
on a BS Fuji recording, a data content descriptor pointing at the data
broadcast (component tag 0x40) survives into a file that has no data
broadcast in it.

ARIB names a stream by a one-byte component tag rather than by PID. Three
descriptors point at another stream that way -- component (0x50), audio
component (0xC4) and data content (0xC7) -- and all three put the tag third
in the body. A descriptor naming a tag **the recording's map described and
the output does not carry** is dropped, and the section is closed again with
a new length and a new CRC. Both halves are required: a tag that appears in
no map this program read is left alone, because the broadcaster's own tables
disagreeing is not something to settle here.

A downmixed track counts as not carried. A folded track no longer has the
channel arrangement its audio component descriptor names, so it comes out of
the programme description for the same reason it comes out of the map.

**Only the programme on now is trimmed.** Present and following arrive as two
sections and only the first is about this file; the second is a note about
what came next on the air, a programme whose streams were never going to be
in here. Judging its tags against what this file carries would answer a
question nobody asked, and would throw away the one true thing it says.

## Written as a partial transport stream (the default)

A cut of a broadcast recording is, in the standards' terms, a **partial
transport stream**. DVB describes one in EN 300 468 Annex C and ARIB in
TR-B15; a Blu-ray recorder writes one, and so does TMPGEnc MPEG Smart
Renderer.

A partial stream carries none of the tables that describe a live multiplex.
In place of NIT, SDT, EIT, TDT and TOT there is **one SIT** -- a selection
information table, on PID 0x001F, table 0x7F -- and everything goes in it:

| Where | What |
|---|---|
| The transmission loop | `partial_transport_stream_descriptor` (this file's own measured peak rate) and `network_identification_descriptor` (JPN and BS/CS/TB, read off the original network id) |
| The service loop | `partial_transport_stream_time_descriptor` (when the programme was broadcast and how long it ran), `service_descriptor` (the service name, from the recording's SDT) and **every descriptor the present event carried** -- the name, the long text, the genre, the components, from the recording's EIT |

**The bytes are the recording's own.** The service name and the programme
name travel in ARIB's own character encoding without this program having to
understand any of it. Only the frame around them is written here.

The peak rate is the one number that cannot be copied across: the recording
never described itself as a partial stream, because it was not one, so
nobody wrote that number down. So the output is measured, in one pass, over
a one-second window. Measured tighter, a single large picture reads as a
burst the file never sustains, and the table would name a rate no device
needs to provide.

The programme that followed does not go in. Present and following arrive as
two sections and only the first is about this file; the SIT says what this
file *is*.

A SIT is built per kept range and its version only moves when the contents
do, so a cut spanning a programme boundary names each programme over its own
stretch -- and a version that changed when nothing else had would be telling
a player to re-read a table it already has.

A section has a ceiling of 4096 bytes, and a real recording's extended event
descriptors ran to a kilobyte on their own. What does not fit is dropped
whole descriptors at a time, from the end, which is why the times and the
service name are written first.

## Written with the broadcast's own tables (`--tables broadcast`)

The partial stream is the standard's answer, but **what the software around
Japanese recordings actually reads is SDT and EIT**. TVTest does, EDCB does,
ffmpeg does; none of them reads a SIT. Run `ffprobe` over a partial stream
and no service name comes back -- the same is true of TMSR's output.

So the older shape is still there. `--tables broadcast` rebuilds the PMT,
replaces the SDT with the recording's own, and injects EIT present/following
and TOT on the PIDs they belong on. The clock in the TOT runs on from the
source time each range opened at, so it jumps at every cut -- which is what
keeping the broadcast's own wall clock means.

`--tables muxer` (once `--no-tables`) leaves the muxer's own PAT, PMT and SDT
standing. It is there for the tool that wants a plain stream rather than a
recording, and for telling apart which account a downstream tool was reading
when it disagrees. Which one you get is on the `--analyze` line.

## The PIDs are the recording's; the clock rides with the video

A partial stream does not specify where the PIDs go. TMSR renumbers them
into the 0x1100s; **this keeps the recording's own** -- the map, the video,
the sound and the subtitles all come back on the PID they arrived on.

One thing does not carry across. **A Japanese broadcast sends its clock on a
PID of its own** -- 0x0100 on BS Fuji, 0x01FF on the terrestrial services --
and libavformat's mpegts muxer has no way to be told to do that, so the PCR
rides in the video's adaptation fields. The PMT says so, correctly, and the
file is consistent with itself: it is legal MPEG-2 and legal ARIB, and
anything that reads PCR_PID out of the PMT is unaffected. A dedicated PID
could be synthesised, but the video would then carry a second copy of the
clock, and the gap between where an inserted packet lands and the time it
claims -- about 63 microseconds at 24 Mbit/s -- is outside what MPEG allows
a PCR to be out by. A correctly placed clock on the video beats an invented
one that meets the letter of the spec.

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
