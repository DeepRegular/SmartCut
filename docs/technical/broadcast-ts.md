# Broadcast workflow compatibility

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](broadcast-ts.ja.md)

Tools built around Japanese broadcast recordings — DGIndex among them — read the
PIDs, the PMT, the service number and the descriptors before they read a single
frame. If those differ from the recording, the file either does not open or the audio
track cannot be found.

This page covers what SmartCut does to keep a cut `.ts` looking like the recording it
came from.

## The output inherits the recording's layout

Lined up against the source, the early output looked like this:

| | PMT PID | Video PID | Audio PID | Audio descriptors |
|---|---|---|---|---|
| Source (BS Fuji) | 0x100 | 0x1100 | 0x1101 | `0x52` `0x0a` |
| Output (originally) | 0x1000 | 0x100 | 0x101 | **none** |
| Output (now) | 0x100 | 0x1100 | 0x1101 | `0x0a` |

**The PIDs are derived from the video stream's**, not from "the lowest PID in the
file". A broadcast recording carries subtitles and data broadcasting as *streams*
too, and their PIDs sit below the range the muxer accepts (0x0020–0x1FFA). The first
attempt picked one of those up, passed 18 as `mpegts_start_pid`, and produced a
0-byte output.

`0x52` (component_tag) is the one thing that does not get written, because ffmpeg's
muxer does not write it.

### Every stream goes back on the PID it arrived on

Numbering from the video's PID gets you as far as the table above, and no further.
The muxer reads `AVStream.id` as **the PID to write the stream on** — anything 16 or
over is used as it stands rather than numbered from `mpegts_start_pid` — so handing
each stream the PID it had in the recording puts the sound and the captions back where
they were.

That the audio matched under the old scheme was luck: 0x1100 plus one happens to be
0x1101. A recording whose PIDs are spread out (0x100f / 0x104f / 0x120f) did not match
at all.

The descriptors are a different matter, because the muxer will not write those. They
are restored by the pass described below.

## Putting the recording's own tables back

> Since 0.3.1 the default output is the partial transport stream described further
> down. This section is what `--tables broadcast` does — and the machinery under it is
> what both shapes are built on.

A broadcast also talks about itself. Which service this is, what the station is
called, what is on now and what follows, what time it is: PAT, PMT, SDT, EIT, TOT.
**That is what a recorder's library view, a player's channel display and every
downstream tool actually read.**

None of it survives a mux. libavformat writes its own PAT, PMT and SDT out of what it
knows, which is the streams and nothing else. The captions come out with their ARIB
descriptors because the muxer knows that codec; everything else — the audio's
component tag, the copy-control descriptor, the superimpose stream's identity — is
simply not written. With EIT and TOT, something worse than loss happens: ask the muxer
to copy PID 0x12 and it accepts it as an anonymous private stream and **puts it on a
PID of its own choosing**, where nothing will ever look for it.

So they are put back afterwards, by one pass over the finished file:

- **the PMT is rebuilt** with the recording's own descriptors,
- **the SDT is replaced with the recording's own section** — which is how the service
  name arrives in ARIB's own character encoding without this program having to
  understand a byte of it,
- **EIT present/following and TOT are injected** on the PIDs they belong on,
- and every continuity counter is renumbered.

Where an injected section goes is decided by the output's own clock. Each kept range
uses a snapshot read at **the source byte** its opening picture arrives at — which the
access-point index knows, and which is what makes this cheap on a file of several
gigabytes — so a cut spanning a programme boundary describes both programmes. The
clock in the TOT is moved on as the output runs; left as the snapshot it would name
the same second for the length of the file.

Two descriptors are not carried across: the conditional access descriptor (0x09) and
ARIB's own version of it, the access control descriptor (0xF6). Both say which system
scrambles the service and which PID the entitlement messages arrive on. The output is
not scrambled and has no ECM stream, so restating either would be describing a file
that does not exist — and 0xF6 arrives twice on a Japanese broadcast, once in the
programme loop and once beside the captions. A disc written with them left in
announced an ECM stream on a PID the disc does not have, in a file nothing had
scrambled; neither of the reference discs carries one.

The network information table is read for one thing only, and not carried at all:
**which button on a remote control the recording came off**. It is in the transport
stream information descriptor (0xCD) there and nowhere else, and without it the three
digits a viewer knows a terrestrial channel by cannot be worked out. See
[Writing a disc](bdav.md#the-channels-number).

## The map is not fixed for the length of a recording

The tables above are read out of the head of the file, because PAT, PMT and SDT
repeat every few hundred milliseconds and a few megabytes is many copies of each.
**That is not the same as the first copy being the right one.**

A station adds the caption stream to its map when the programme starts, and a second
sound track with it, and takes them out again when the programme ends. A recording
that begins a few seconds early — which is every recording — therefore opens on a map
that names neither. Taking the first map that arrives and asking no more of it wrote
cuts whose caption packets were all there, on the right PID, and whose map did not
mention them: nothing downstream could find them, neither a player nor this program
reading the file back to write a disc's index, which fell through to a guessed stream
type.

Measured: of thirty-two stations sampled a recording each, **four were wrong this
way**; of four hundred recordings sampled at random, **ninety-five change their map
after the first copy of it**, and every stream that appears late is a caption or a
second sound track — exactly what a cut carries. Some stations do it inside the first
second and some a little past eight megabytes in, which is where the window this
reads used to stop.

So the map is asked for **by name**. `si::read_service` takes the PIDs the caller is
going to carry, the reading goes on until the map in hand describes all of them, a
later map that names more of them replaces the one before, and the window opens once
further — to 64 MB — when the first pass came up short. A map that changes does it
near the start, and a recording that never names a stream in its first minute was
never going to. A caller with nothing particular to look for is still answered by the
first map and pays for no extra reading.

## The file describes services it is not a recording of

A recorder that keeps one service off a multiplex keeps that service's packets and
that service's map — and the multiplex's *own* service description and event
information, whole. So a one-hour recording off one channel arrives describing five
services: what each of them is called, what is on each of them now, and what follows.
Nothing in the file marks which of the five it is a recording of.

`si::programme` read the first description it found. On a satellite multiplex that is
not the recorded service anything like reliably: a recording off an animation channel
came back named after a musical revue, off a channel it was never on, at a time it did
not go out — and a disc built from it was labelled that way in a recorder's list.

Measured over sixty recordings drawn at random from the corpus: **the name matched
what the recorder itself called the file in twenty-four of them**, and the service
name matched the channel in thirty-one. Reading it the way described below: **sixty of
sixty**, both fields.

Which service it is, is in the file after all, one table further back. The programme
association table names every service the multiplex carried and where each one's map
is; the recorder kept one of those maps. So the service whose map is present is the
service this is a recording of — and where that does not settle it, because no
association table survived or because every map did, the tables are read as they were
before, first description first. `si::recorded_service` is that pass, and what it
finds filters both the event information and the service description, which carries
several services in the one section too.

## What is put back is trimmed to what was written

The same reasoning is needed one step further along. The broadcast describes the data
broadcast and the superimposed crawl as streams it is sending, and a cut carries
neither. Copy the programme description across whole and **the output announces an
entry point into something that is not in the file** — on a BS Fuji recording, a data
content descriptor pointing at the data broadcast (component tag 0x40) survived into a
file that has no data broadcast in it.

ARIB names a stream by a one-byte component tag rather than by PID. Three descriptors
point at another stream that way — component (0x50), audio component (0xC4) and data
content (0xC7) — and all three put the tag third in the body. A descriptor naming a tag
**the recording's map described and the output does not carry** is dropped, and the
section is closed again with a new length and a new CRC.

Both halves of that rule are required. A tag that appears in no map this program read
is left alone, because the broadcaster's own tables disagreeing is not something to
settle here.

A downmixed track counts as not carried. A folded track no longer has the channel
arrangement its audio component descriptor names, so it comes out of the programme
description for the same reason it comes out of the map.

**Only the description of the programme on now is trimmed.** Present and following
arrive as two sections, and only the first is about this file. The second is a note
about what came next on the air, a programme whose streams were never going to be in
here. Judging its tags against what this file carries would be meaningless, and would
throw away the one true thing it says.

## Written as a partial transport stream (the default)

A cut of a broadcast recording is, in the standards' terms, a **partial transport
stream**. DVB describes one in EN 300 468 Annex C and ARIB in TR-B15; a Blu-ray
recorder writes one, and so does TMPGEnc MPEG Smart Renderer.

A partial stream carries none of the tables that describe a live multiplex. In place
of NIT, SDT, EIT, TDT and TOT there is **one SIT** — a selection information table, on
PID 0x001F, table 0x7F — and everything goes in it:

| Where | What |
|---|---|
| The transmission loop | `partial_transport_stream_descriptor` (this file's own measured peak rate) and `network_identification_descriptor` (JPN and BS/CS/TB, read off the original network id) |
| The service loop | `partial_transport_stream_time_descriptor` (when the programme was broadcast and how long it ran), `service_descriptor` (the service name, from the recording's SDT) and **every descriptor the present event carried** — the name, the long text, the genre, the components, from the recording's EIT |

**The bytes are the recording's own.** The service name and the programme name travel
in ARIB's own character encoding without this program having to understand any of it.
Only the frame around them is written here.

The peak rate is the one number that cannot be copied across: the recording was not a
partial stream and never described itself as one, so nobody wrote that number down.
The output is therefore measured, in one pass, over a one-second window. Measured over
a shorter window, a single large picture reads as a burst the file never sustains, and
the table would name a rate no device needs to provide.

The programme that followed does not go in. Present and following arrive as two
sections and only the first is about this file; the SIT says what this file *is*.

A SIT is built per kept range and its version only moves when the contents do, so a cut
spanning a programme boundary names each programme over its own stretch. A version that
changed when nothing else had would be telling a player to re-read a table it already
has.

A section has a ceiling of 4096 bytes, and a real recording's extended event descriptors
ran to a kilobyte on their own. What does not fit is dropped whole descriptors at a
time, from the end, which is why the times and the service name are written first.

## Written with the broadcast's own tables (`--tables broadcast`)

The partial stream is the standard's answer, but **what the software around Japanese
recordings actually reads is SDT and EIT**. TVTest does, EDCB does, ffmpeg does; none of
them reads a SIT. Run `ffprobe` over a partial stream and no service name comes back —
the same is true of TMSR's output.

So the older shape is still there. `--tables broadcast` rebuilds the PMT, replaces the
SDT with the recording's own, and injects EIT present/following and TOT on the PIDs they
belong on. The clock in the TOT runs on from the source time each range opened at, so it
jumps at every cut — which is what keeping the broadcast's own wall clock means.

`--tables muxer` (formerly `--no-tables`) leaves the muxer's own PAT, PMT and SDT
standing. It is there for the tool that wants a plain stream rather than a recording,
and for working out which description a downstream tool was reading when tools
disagree. Which one you get is on the `--analyze` line.

## The PIDs are the recording's; the clock rides with the video

A partial stream does not specify where the PIDs go. TMSR renumbers them into the
0x1100s; **SmartCut keeps the recording's own** — the map, the video, the sound and the
subtitles all come back on the PID they arrived on.

One thing does not carry across. **A Japanese broadcast sends its clock on a PID of its
own** — 0x0100 on BS Fuji, 0x01FF on the terrestrial services — and libavformat's mpegts
muxer has no way to be told to do that, so the PCR rides in the video's adaptation
fields.

The PMT says so, correctly, and the file is consistent with itself: it is legal MPEG-2
and legal ARIB, and anything that reads PCR_PID out of the PMT is unaffected. A dedicated
PID could be synthesised, but then the video would carry a second copy of the clock, and
the gap between where an inserted packet lands and the time it claims — about 63
microseconds at 24 Mbit/s — is outside what MPEG allows a PCR to be out by. A correctly
placed clock on the video beats an invented one that only meets the letter of the
spec.

## The re-encoded head claimed to be "59.94 fps"

This surfaced from a report that the frame rate did not line up in DGIndex → AviSynth
(MPEG2Dec). **Cut anywhere other than a lossless point and the head becomes a re-encoded
region — and SmartCut is the one writing its sequence header.** It disagreed with the
source:

```
source   frame_rate_code=4 (30000/1001)
output   frame_rate_code=7 (60000/1001)   <- only the re-encoded head
```

**Indexing tools trust the first sequence header they meet for the whole file**, so a
29.97 recording was being handed downstream as 59.94.

The cause was the unit of the output timeline. The field-based time base — one tick, one
field — was being handed to the encoder as it was, and **MPEG-2's frame rate code is
derived from the time base**, so it doubled. Setting `framerate` separately has no
effect, because mpeg12enc looks only at the time base.

Fixed by giving the encoder a **picture-based time base**, numbering the pictures handed
to it, and keeping separate track of where on the timeline each number lands and how many
fields it occupies. The field-based timeline on the output side is unchanged.

This is not the kind of thing a frame-hash comparison reveals, so
`tests/run_ts_layout_tests.sh` gained a check that the re-encoded head's sequence header
matches the source.

## Reading the captions, to draw them over the preview

As far as a cut is concerned an ARIB caption is something to **carry**, not something
to read: the bytes move to the output and the ends of each range are mended. The
editor asks a different question. **What is on screen at this instant** — because
whether a seam lands in the middle of a line is not something the picture will tell
you.

Nothing in the FFmpeg this program ships against can decode one. Debian's FFmpeg 7.1
has no ARIB caption decoder, and a build with `libaribcaption` in it is not something
to depend on. So they are read here (`caption.rs`, `Layout`). Decoding the characters
themselves is what `arib.rs` already does for programme names; what was missing is
**where to put them**.

A caption statement opens with its own layout. This is what a broadcast sends:

```
CS  SWF(7)  SDF(620;480)  SDP(170;30)  SHS(4)  SVS(24)  SSM(36;36)
APS(5,0)  [≫カローラは]  APS(6,0)
```

- `SWF` is the screen format. **7 is a 960 x 540 caption plane**, and that can be read
  off the broadcast rather than out of the standard: a 620 x 480 display area placed
  at (170, 30) leaves exactly 170 either side and 30 above and below.
- `SSM` is how big a character is (36 x 36) and `SHS`/`SVS` the space left around it
  (4 and 24). Added together they make the **character field**, 40 x 60 here, so the
  display area holds 15 columns and 8 rows.
- `APS(row, column)` is where writing begins — **the row comes first**, and both count
  from 0 at the top left of the display area. Rows 5, 6 and 7 of the eight are where
  an ordinary caption goes, which is the bottom of the screen. What they count is the
  field the **size in force** makes: a channel writing in `MSZ` that sends column 25
  means 58 + 25 x 20, a line up against the right edge. Counted at full width it is
  1058, which is off a 960-dot plane — and `SSZ` halves the row as well, which puts
  the line off the bottom. Three of five channels measured do this, and what went
  missing was the replies.
- `SDF` is how big the display area is, and what it is for here is the **right edge**:
  a character that will not fit goes at the left edge of the next line down, which is
  what a receiver does and what the channels write for. One puts two speakers in a
  single statement — the first line, a colour change, then the reply, with no position
  between them — and the reply ran off the plane. A line that ends exactly on the edge
  fits: the channels that right-align a line land on it to the dot.
- `ACPS(x; y)` is the other way to place a line, and the one most channels use: a
  place in the plane's own dots rather than a row and a column. What it names is the
  **bottom left** of the character field — 509 on a 540-dot plane is the last row, not
  a row past the end of one — and a broadcast names it before the size that says how
  tall that field is as often as after, so what it sets is kept as a bottom and turned
  into a top when a character is drawn. Reading past it put every caption on those
  channels in the top left corner.
- Colour comes from the eight codes `RDF`…`WHF` and from `COL` by number. Only the
  foreground is followed; behind the text the preview draws a translucent black box of
  its own. The colour a broadcaster names for the background is an entry in a
  128-colour map, and what a preview needs is for the characters to be readable.
- A statement that carries `CS` and nothing else is the caption **coming down**. The
  same shape is what marks a commercial break — see [Detecting commercials](cm-detection.md).
- `TIME` is a statement timing itself. `TIME(0x20, n)` waits n tenths of a second, and
  the `CS` after that wait takes the line down **then**, not at once: `CS … text …
  TIME(2.0) CS` is a line that stands for two seconds. Read as a plain clear it wiped
  the text in front of it, and the channels that write captions that way showed none
  at all. So a statement comes back as a list of **pages** (`caption::Page`), each with
  the moment it goes up and, where the statement says one, the moment it comes down.

Half widths are handled too: the alphanumeric and half-width katakana sets take half a
field per character, and `MSZ` draws a full-width character in half a field, which is
how any caption of more than fifteen characters to a line is written.

### The characters the broadcaster draws

Some of what a caption says has no code in any of the sets. The arrow that carries a
sentence into the next line, the double brackets a speaker's name sits in, the `ü` in
a German line — ARIB's answer is to send **the dots**: the statement carries the
pattern of the glyph in a data unit beside the words (`data_unit_parameter` 0x30 for
the one-byte sets, 0x31 for the two-byte one), designates a downloaded set with an
escape that puts `0x20` before the final byte, and then writes the cell it put the
glyph in.

There is nothing to spell that with, and answering `〓` — which is what a receiver
with no glyph shows — is what this did. **It is not a rare corner.** Of 62 recordings
sampled two to a channel across the 32 channels here, 51 carry captions and 27,829
statements were read out of them: 971 of those statements, 3.5%, carried a geta mark,
and *every one* came from a downloaded set rather than from a cell of the ordinary
sets that this cannot name. Fifteen of the 26 channels that caption their programmes
send glyphs at all. (Not every one of those cells was a glyph: some were hiragana in
a set a macro had designated back, which is the section after this one. A reader that
runs neither cannot tell them apart, which is rather the point.)

So the dots come back with the run (`caption::Glyph`: one bit a dot, the shades
flattened) and the window draws them in the character cell, the same box a glyph from
a font goes in. The 43 distinct pictures in the sample are sent at 36 x 36 — the
character cell — and a handful at 18 x 36, which is what a broadcaster sends for a
line written in `MSZ`. The cell is what they are drawn in either way, so a size the
pattern was not sent for is a glyph scaled rather than a glyph refused.

Two things about the cells are worth knowing, because both look like bugs from the
outside:

- **The same cell is redefined line by line.** A channel sends one cell — 0x4121, the
  first of DRCS-1 — and redefines it in every statement that uses it: an arrow in this
  line, a bracket in the next. A reader that took the first picture and kept it would
  draw an arrow for the rest of the programme.
- **A cell is written whose picture is not in the statement.** So the pictures are
  remembered (`Layout`, beside the format, and for the same reason), and a statement
  that writes a cell nothing has ever defined is still the geta mark. Scrubbing a
  timeline is the ordinary way to get one: the window reads a stretch around the
  instant, and the statement that defined the cell can be behind it.

### The two bytes in front of a speaker's name

A caption switches sets with `SS3` — `0x1D` and a byte — and the set it finds there is
**not the katakana**. Three of the four slots a caption starts with are the ones a
programme name starts with; the fourth is the **macro set**, whose cells are stored
escape sequences. `1D 60` designates the kanji, the alphanumerics, hiragana and the
macros themselves into the four slots and invokes the first and the third; `1D 61` is
the same with the katakana in place of the alphanumerics. Broadcasters use them as
shorthand, and a line naming its speaker switches sets three times in six bytes:

```
1D 60  0E  MSZ  (    1D 61  0E  ヨ ハ ネ ス    1D 60  0E  )
```

Read as characters, each of those was a stray katakana — and the sets they name were
never designated, so everything after them was read against the wrong ones. That line
came back as `ム(メhOM9ム)` where the broadcast said `（ヨハネス）`. It is also how a
channel designates the hiragana **back** into G2 after writing a downloaded glyph
there, so on that channel every kana after a glyph was read as another glyph and shown
as the geta mark, cell after cell — the geta marks the fix above would otherwise have
been blamed for.

The three macros the standard opens with are run; a cell this does not know — the
thirteen that designate the mosaic sets and the downloaded sets of data broadcasting —
is run as nothing rather than as a guess, which leaves the sets alone and writes no
character. And the starting state is now asked for by name (`arib::Start`), because
the two kinds of text this program reads do not share it.

Read again with both of those right, the same 62 recordings come back with **no geta
mark at all**: 1,213 downloaded glyphs drawn across 27,829 statements in the 51
recordings that carry captions, and not one cell left that this cannot name
(`capdiag <file> stats`). That is every statement the bytes hold — the same count a
reader that ignores the tables and looks for caption packets directly arrives at,
which it takes the section below to get to.

### The captions the head of the file does not mention

A recorder starts before the programme does, and a broadcaster announces its caption
stream when the programme starts. So the program map at the head of the file names the
video and the sound and nothing else, and a few seconds in it is replaced by one that
also names the caption PID. libavformat probes the head of a file and stops after five
megabytes — about two and a half seconds of a broadcast — so on such a recording it
never sees the second map. No subtitle stream is listed: the editor offers no caption
track, the commercial detector has no caption marks to read, and the cut has nothing
named to carry.

Over the same 300 recordings, two from each of the 32 channels here, 279 carry
captions and **7 of them are announced late** — between 8.4 and 8.8 megabytes in, just
past where the probe stops, with the first caption packet 10 to 12 megabytes in. The
other 20 carry no captions anywhere in the file, which is what the bytes say when they
are looked at directly rather than through the tables.

So a transport stream that comes back with no subtitle stream at all is opened a
second time with a 32 megabyte probe (`input.rs`, `captions_may_come_later`). That
ceiling is not a read: the analysis stops at five seconds of stream whatever it says,
which is about ten megabytes at a broadcast's bitrate. It costs about 0.3 s on a
recording that really has no captions, and nothing at all for the nine in ten that
listed them the first time — asked where it is needed rather than always, which is how
the other two reopenings in that module work.

What comes out is text and where to put it, and — for the characters the broadcaster
drew — the dots of those. **The window does the drawing**, with the fonts it has and
an outline around each glyph, which is why the characters stay sharp at any size and
why there is no glyph rendering here beyond unpacking the dots nobody has a font for.
A glyph the font draws wider than the field it was given is squeezed into it: a font
that has never heard of ARIB draws a full-width bracket full width whether the
broadcaster asked for half a field or a whole one, and the bracket a speaker's name
opens with went under the name beside it.

Nor is the whole file read. A preview sweeps a timeline, so only the stretch around
the instant is read and kept (`subs.rs`: 30 seconds back and 30 on). Reading back is
what makes the answer right — the caption on screen was put there by an *earlier*
statement — and playback crosses the end of a window about once a minute. A disc's
subtitles are read the same way; see
[The subtitles a disc draws](disc.md#the-subtitles-a-disc-draws).

## Audio can be written out on its own as `.aac`

In a DGIndex → x264 → mux workflow, the audio reaches the muxer as a **bare ADTS file**.
There is a case where the step producing that `.aac` turned out to be the weak link: the
`.aac` DGIndex produced started with `FF F8 04 22`. The sync word is right, but what
follows declares "Main profile / 88.2 kHz / 0 channels", which is not a thing that can
exist. L-SMASH builds its AudioSpecificConfig from that first frame, so it gives up before
reading anything (`failed to find the matched importer`).

The copied frames do not differ from the source by a single byte:

```
source              ff f8 4c a0  MPEG-2 with CRC, profile=LC, rate=48000, ch=2
copied frame        ff f8 ...    the recording's own bytes, by construction
frame written here  ff f9 ...    MPEG-2, no CRC
```

The decisive observation was that **it only breaks on a frame that is written rather than
copied**. On copy the broadcast's own bytes go through, so the ADTS stays `ff f8` — the
form Japanese broadcast AAC always takes. Hand the same frame to ffmpeg's ADTS writer and
it **hard-codes** `ff f1` (MPEG-4, no CRC). The header length changes from 9 bytes to 7,
so a tool that expects ARIB and goes looking at 9 bytes misreads the configuration — and
`04 22` (Main / 88.2 kHz / 0 ch) is exactly what "could not read it" defaults to. Those
two bits cannot be changed from the ffmpeg side.

So they are not written from the ffmpeg side any more. `adts.rs` builds the header here,
and a frame SmartCut writes announces the version the recording announces: a broadcast is
MPEG-2 AAC, and so is the seam. See [audio](audio.md#writing-mpeg-2-aac---aac).

What is still not the recording's shape is the CRC. `protection_absent` is set on the
frames written here, so their header is 7 bytes rather than 9. That is a per-frame field,
so they sit legally among frames that carry one, but they are not byte-for-byte what the
recording would have had. `--aac mpeg2|mpeg4` overrides the version, and while frames are
being copied a request that disagrees with the recording is refused rather than producing
a stream that is two kinds of AAC at once.

How many such frames there are is the mode's business. Under `smart`, the default, it is
the frames a boundary falls inside — four out of 5606 on a two-interval cut, and none at
all when the seam falls in silence, as a commercial cut does. Under `reencode` it is all
of them, and under `copy` none.

`write_audio_es()` (`--audio-es` on the CLI) **reads the finished output back** and
writes the audio out as ADTS, so what ends up next to the video is, by construction, the
very audio inside it. It has been confirmed to pass through L-SMASH's `muxer`
(`Track 1: MPEG-4 Audio`, 48 kHz stereo, matching duration).

The sidecar itself is kept out of the GUI, because what made the workaround worth having
— a seam declaring itself MPEG-4 in the middle of an MPEG-2 stream — is now dealt with
where it arises. The audio *modes* are on the output settings screen, alongside the
channel count and the bitrate (see [audio](audio.md#the-output-settings-screen)), and
they start on the engine's own default, which is smart rendering.

## To hand it to L-SMASH, use the bare stream

A report said that feeding the output to L-SMASH's `muxer` stops with `[importer: Error]:
failed to find the matched importer.`, so it was checked. **Feeding it the source `.ts`
gives the same error.** `muxer` reads bare streams only — Annex-B H.264/H.265, ADTS AAC,
AC-3 and so on — and a transport stream is out of scope from the start. There is nothing
wrong with the output's AAC.

What was confirmed:

| What was passed | Result |
|---|---|
| The output `.ts` directly | `failed to find the matched importer` |
| **The source `.ts` directly** | **The same error** |
| `.aac` extracted from the output (ffmpeg) | `Track 1: MPEG-4 Audio` — works |
| `.aac` extracted from the output (naive concatenation of the PID's PES) | works |
| `.aac` extracted from the source | works |

The output's PMT carries the same stream_type as the source, `0x0F` (ADTS AAC), and the
leading bytes are the `ff f8` sync word. Extraction works even with a naive PID dump.

```bash
ffmpeg -i cut_xxx.ts -map 0:a:0 -c copy cut_xxx.aac   # hand this to muxer
```

Note that **L-SMASH does not handle MPEG-2 video** (`remuxer` says `no support to remux
this stream`), so this material cannot be turned into MP4 by L-SMASH alone in the first
place. The assumed workflow has a separate step converting the video to H.264, and carries
only the audio across.
