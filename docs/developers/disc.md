# Reading a Blu-ray

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](disc.ja.md)

**A Blu-ray opens as a folder or as an `.iso`, either way, and both halves of
the specification are read: BDAV, the recording format, and BDMV, the format
of a film you buy.** What the list shows is the name of the programme, not
`00001.m2ts`.

```
$ smartcut Anime_Test.iso
disc  : Anime_Test.iso
        bdav -- Anime_Test
        3 recording(s)

  1  00:08:30.010  2026年08月17日01時00分-BS11イレブン-アズールレーン びそくぜんしんっ!にっ!! #07「神様的休息日の過ごし方。」  2 mark(s)
       0x1101  AAC stereo 48kHz
       0x1102  stream type 0x06
  2  00:08:30.010  …
```

A pressed disc carries no names, so its rows are the disc and the clip. It
also carries a great deal that is not the film, so the ones worth a look are
starred:

```
$ smartcut IsekaiQuartet_Season1.iso
disc  : IsekaiQuartet_Season1.iso
        bdmv -- Isekai Quartet Season 1
        62 recording(s)

   1  00:00:43.543  Isekai Quartet Season 1 00008  1 mark(s)
       0x1100  AC-3 stereo 48kHz eng
       0x1200  PGS eng -- a cut cannot carry this
   2  00:00:11.511  Isekai Quartet Season 1 00002  1 mark(s)
   …
*  8  00:11:52.003  Isekai Quartet Season 1 00014  4 mark(s)
       0x1100  TrueHD multi 48kHz eng
       0x1101  TrueHD stereo 48kHz jpn
       0x1200  PGS eng -- a cut cannot carry this
       0x1201  PGS eng -- a cut cannot carry this
```

A pressed disc is asked about rather than swallowed whole -- see
[the chooser](#the-chooser), below.

In the window, drop an `.iso` or a disc folder and it asks which of the
recordings on it you meant. A folder holding several discs works the same way,
so an evening's worth of them becomes one list.

## The two dialects

**BDAV** is the **recording** format -- what a set-top recorder writes, and
what an authoring tool produces when it is asked for a disc of recordings.

```
BDAV/
  info.bdav              which playlists there are, and in what order
  PLAYLIST/00001.rpls    one recording: which clip, from when to when
  CLIPINF/00001.clpi     that clip's own index
  STREAM/00001.m2ts      the transport stream itself
```

**BDMV** is the format of a film you buy: a pressed disc, or a copy of one.
The shape is the same and the names are not, and there is a great deal more of
it -- menus, a Java application, a second copy of the whole index under
`BACKUP` -- none of which is a recording.

```
BDMV/
  index.bdmv             the titles, for a player's own menu
  PLAYLIST/00009.mpls    one way through the disc: which clips, in order
  CLIPINF/00014.clpi     that clip's own index
  STREAM/00014.m2ts      the transport stream itself
  META/DL/bdmt_eng.xml   what the disc is called
  BACKUP/                all of the above again
```

The two differ in four places -- the directory's name, the playlist's extension
and magic, where the list of playlists comes from, and whether a playlist
carries a programme name -- and agree everywhere else, **including the byte
layout of a play item and of a chapter mark**. So
[`disc.rs`](../../rust/crates/core/src/disc.rs) is one reader that is told
which dialect it is looking at, rather than two readers that would be the same
reader twice.

There is one thing BDMV has that cannot be read here. `index.bdmv` names
**titles**, and a title is a navigation or Java program rather than a playlist:
working out which playlist "T05 Extra 01" plays means running the disc's own
menu code, which is a Blu-ray player and not this. So the playlists are taken
from the directory, sorted -- which is the order the authoring tool numbered
them in, and on every disc looked at that is the order a person would have
chosen anyway.

The stream is an ordinary MPEG-2 transport stream in 192 byte packets -- 188 of
packet behind four bytes saying when it arrived -- which libavformat reads
without being told anything. **So the streams were never the difficulty.** What
was missing was the index: a directory of `00001.m2ts`, `00002.m2ts`,
`00003.m2ts` says nothing about which programme is which, and a disc holding a
night's recordings is exactly the case where that matters.

## Reading an image without mounting it

An `.iso` is a UDF filesystem. Linux can mount one, but mounting wants root and
a loop device, and asking a cut editor to hold a mount for the length of an
edit is asking for a stale mount after a crash. Windows mounts an `.iso` by
double-clicking it, which is one more thing the user has to have done before
the program is any use.

So nothing is mounted. The image is read where it lies
([`udf.rs`](../../rust/crates/core/src/udf.rs)), far enough to answer one
question:

> **Which byte of the image does each file start at, and how many bytes long is
> it?**

A `.m2ts` written by a burner is one unbroken run of bytes, so the answer is a
**byte range** -- and a byte range is something libavformat can be handed
directly, through its `subfile` protocol:

```
subfile,,start,747520,end,1232594944,,:file:/rec/Anime_Test.iso
```

The demuxer reads that as a stream and never learns there is a filesystem
around it. Everything downstream -- the packet scan, the seek to an access
point, the copy -- is the same code it was for a plain file.

> The inner name is given as `file:` because the option list ends at the
> **first colon**: a Windows path would otherwise be read as the protocol `c`.

### The gigabyte limit

A UDF allocation descriptor carries 30 bits of length, so no single one of them
can describe more than 1 GiB. A 1.2 GB clip is therefore two descriptors:

```
len=1073739776  lbn=77      -> byte 747520
len=158107648   lbn=524364  -> byte 1074487296
```

747520 + 1073739776 = 1074487296. **They follow each other exactly.** A burner
lays the pieces down back to back, so what is fragmented is the bookkeeping and
not the file. [`Entry::contiguous`](../../rust/crates/core/src/udf.rs) joins the
runs that follow each other and returns `None` for a file genuinely written in
pieces -- which a burnt disc does not produce, and which is better refused than
read at the wrong offset.

### The metadata partition

An image written to UDF 2.50 or later has a **metadata partition**: the file
entries are kept together on it while the file data stays on the plain
partition beside it. A logical block number on that partition is a position
*inside a file*, and becomes a sector only by walking that file's own extents.
ImgBurn writes UDF 2.60 and uses one; genisoimage writes UDF 1.02 and does not.
Both are read.

**Encrypted discs are not handled and will not be.** AACS is a decryption
problem and this program has none of it.

## The programme's name is ARIB text

**On BDAV.** A `.rpls` does not carry the name in UTF-8. It is written in the ARIB STD-B24
eight-unit code -- a descendant of ISO 2022 that holds four graphic sets at
once (kanji, alphanumerics, hiragana, katakana) and moves between them with
shifts and escapes, with the size and colour controls in the same byte stream.

[`arib.rs`](../../rust/crates/core/src/arib.rs) **reads** it; it does not render
it. The sizes and colours are dropped and the characters come out in order. The
JIS X 0208 to Unicode table is not written down here: EUC-JP is JIS X 0208 with
the high bit set on both bytes, so the table a UTF-8 world already has is the
table this needs. One dependency, `encoding_rs`.

Two things are worth getting right:

- **Rows 85 and up of JIS are ARIB's own symbols.** JIS leaves them
  unassigned and ARIB fills them with the bracketed markers a listing carries,
  `[新]`, `[字]`, `[終]`. Sending those through the mapping table of an
  encoding that *does* fill those rows -- which is where a general purpose
  decoder would send them -- produces entirely different characters. So they
  come out as `〓`, which is what a receiver with no glyph for them shows.
- **The last eight cells of a kana set are punctuation, not kana.** Rows 4 and
  5 of JIS are not full, and ARIB spends what is left on `ー` `。` `「` `」`
  `、` `・`. Miss them and a programme name reads
  `#07〓神様的休息日の過ごし方。」`.

A `.mpls` carries no name at all -- a film's titles live in the menu, which is
a Java application -- so a pressed disc's rows are named by the disc and the
clip: `Isekai Quartet Season 1 00014`. What the disc calls itself comes out of
`META/DL/bdmt_*.xml`, scraped rather than parsed: the file is a document with a
dozen namespaces declared and one interesting element in it, and pulling in an
XML parser to reach `<di:name>` would be the largest dependency in the program
by some way.

## What a row is

**One clip, one row** -- not one playlist, one row.

On BDAV those are the same thing until a playlist plays more than one clip,
which is what a recorder writes when a programme ran past the length it splits
its streams at. Those become a row each, named `(1/3)`, `(2/3)`, `(3/3)`.

On BDMV they are not the same thing at all. A pressed disc names the same clip
from several playlists: once in a playlist of its own, again inside the "play
all", and again in whatever the menu's chapter list points at. On the disc this
was written against, **45 playlists name 328 play items between them, and there
are 62 distinct clips.** One row per play item would offer the same episode
three times.

So a row is keyed on the part of a clip a playlist plays -- the clip's number
and its IN and OUT to the tick -- and the first playlist to name it makes the
row. Keyed on the part and not on the clip alone because a recorder can write
two programmes into one stream and a playlist for each half, and those are two
recordings.

Joining a multi-clip playlist into one timeline is a different piece of work:
each clip carries its own clock, and splicing two of them is not the same
operation as cutting one. Until that exists, showing the pieces is the honest
thing -- every second of the disc is reachable, and the list says plainly that
it is in pieces. A pressed disc's "play all" is *not* a recording in pieces,
which is why its rows are not numbered `(1/15)`: the disc never asserted that
relationship, and claiming it would be inventing one.

### The chooser

A recording is one thing and needs no question asked about it. A pressed disc
is not: those 62 rows are twelve episodes among fifty logos, warnings, menu
loops and eight second transitions, and the disc calls all of them `000NN`.

So the index is laid out and the question is asked once, before anything is
opened -- which is why every answer in it is in terms the index can give: a
length, a size, and a track named by the PID it sits on.

Clips over **five minutes** are offered already ticked, which on that disc is
exactly the twelve episodes; the rest are folded away behind one checkbox. The
threshold is well under the shortest thing anybody keeps a disc for and well
over the longest thing a menu is made of. A disc where nothing clears it is not
a disc holding nothing, so the longest clip is ticked instead. A BDAV disc is
all offered: it is a disc of things somebody chose to record.

### What a clip carries

The tracks under each row come out of that clip's `.clpi`, which is the only
source cheap enough: a chooser that had to demux thirty gigabytes to draw
itself would not be a chooser anybody waited for. What the demuxer says later
is the authority at cutting time; this is what the disc says it wrote.

```
Video      / H.264 1080p 23.976fps / PID 0x1011 / the video cannot be left out
Sound      / TrueHD multi 48kHz    / eng / PID 0x1100
Sound      / TrueHD stereo 48kHz   / jpn / PID 0x1101
Subtitles  / PGS                   / eng / PID 0x1200 / a cut cannot carry this
Menu       / IGS                   / eng / PID 0x1400 / a cut cannot carry this
```

The two dialects sign that file differently and write the same thing after it
-- a pressed disc's opens `HDMV`, a recorder's opens `M2TS` -- and a recorder
cuts the language field short, so a BDAV row shows `AAC stereo 48kHz` with no
language rather than three bytes of whatever followed. A broadcast's private
streams, the captions among them, are named by their number: what a private
stream holds is not something the disc's index says, and the editor's own
track menu -- which reads the recording rather than the index -- names them
properly.

Three cases, and only one of them is a choice. The video is what a cut is *of*.
The graphics a Blu-ray's subtitles and menus are made of are each a little
display list rather than a run of timed packets, and there is nowhere on a cut
timeline to put one -- they are listed to say they are being left behind, not
to offer anything about them.

Every episode on a disc carries the same tracks, so there is a button that
copies one row's answer to every row with the same track list. Answering the
same question twelve times is not answering it once.

### A track is named by its PID

The chooser answers **before anything is open**, and a stream index is
something libavformat makes up once it has read the recording. So the answer
travels as a list of PIDs and is resolved on the far side: by the editor when
the row is opened in it, and by the backend when it is written out.

A PID can name more than one stream. A Blu-ray's lossless sound arrives as a
TrueHD track with an AC-3 track folded into it, **both on the one PID**, and
libavformat hands them over separately:

```
stream|index=1|codec_name=truehd|channels=6|id=0x1100
stream|index=2|codec_name=ac3   |channels=6|id=0x1100
```

Switching that track off has to switch off both halves of it, which is what
asking by PID means and what asking by index would have got wrong.

It is also why **only the first stream on a PID is written**. A cut puts each
stream back on the PID it arrived on, and two cannot share one -- ask the
muxer for that and it says so and stops:

```
[mpegts] Duplicate stream id 4352
Error: Invalid argument
```

The first stream on a PID is the one the programme map named; anything after
it is a piece the demuxer split out. So the TrueHD is written -- a TrueHD
elementary stream on its own is a track a player decodes -- and the AC-3 core
folded inside it is left out, and said to have been left out, in the track
menu and in what the command line prints:

```
        not carried: a compatibility stream folded into the track written on pid 0x1100
```

The chooser's answer holds only until the editor gives one. The track menu
there writes stream indices into the edit, and from that moment the edit speaks
for the row -- otherwise a track switched back *on* in the editor would be
switched off again on the way out by an answer given before anybody had seen
the recording.

### The marks

Chapter marks are read **only when they can be believed**. The size of a mark
is given away by the section's own length, but what sits where inside one is
not the same in both dialects: BDMV's mark is fourteen bytes -- a byte
reserved, the mark's kind, the play item it belongs to, then the time -- and
the marks a BDAV recorder writes are longer and carry a name and a thumbnail
reference beside the time. So the layout is not assumed. Each candidate is
tried and the one whose times **all land inside the clip they claim** is the
one that is used. When none of them does, the marks are left out: a chapter
point in the wrong place is worse than no chapter point.

Reading **which play item a mark belongs to** is what makes a "play all"
usable. Without it every mark is a number with no clock under it: the fifth
episode's chapter points are on the fifth episode's own timeline, which shares
nothing with the first's beyond both starting near eleven seconds. A layout
that cannot say which clip it meant is believed only on a playlist that plays
one -- otherwise every episode's chapters would land on episode one, and
putting down none is better.

That is also why the marks on a row are taken from the **shortest** playlist
that offers them. A disc names an episode both in a playlist of its own and
inside the "play all", and only the first of those can be read without knowing
which episode it meant.

### The marks become keyframes

A recording opened off a disc arrives in the cut editor with those chapters
already marked. On a Japanese recording they are frequently the commercial
breaks themselves -- written down, exactly, by the machine that made the
recording, which is the same answer [commercial
detection](../user-guide/cm-detection.md) spends minutes looking for.

The one thing in the way is that a mark and the timeline are on **different
clocks**. A playlist counts in the stream's own 45 kHz, and everything the
editor draws is rebased to the container's start time, so a mark is placed at

```
entry.start + mark - src.start_time
```

`entry.start` being the playlist's IN point, carried beside the marks for
exactly this. Nothing on the disc-reading side knows `start_time` -- that comes
from opening the stream -- which is why the marks travel with the row as far as
the editor rather than being turned into times where they are read.

They are placed on a first visit only, and a `.keyframe` file beside the
recording wins over them: that file is somebody's answer, and the disc's is the
answer when nobody has given one. Marks landing outside the material are
dropped rather than clamped, for the reason above. The times can be seen
without the window: `smartcut <disc> --title N` prints them on the recording's
own clock.

## Where a cut goes, and what it is called

There is nowhere to write inside a disc: inside an image there is no folder at
all, and inside a copied disc there is nothing that belongs to anything but the
disc. So a cut is written **beside the disc**, under the **programme's own
name**.

```
/rec/Anime_Test.iso
/rec/cut_2026年08月17日01時00分-BS11イレブン-アズールレーン ….ts
```

The characters a filesystem will not take (`\ / : * ? " < > |`) become their
full width forms, which is what a Japanese recorder does with the same problem;
the name still reads.

**The container is `.ts`.** Asked for a `.m2ts`, libavformat writes Blu-ray's
own shape: 192 byte framing, and Blu-ray's own PID numbering. Both are the
muxer's to decide and neither is the layout that
[the broadcast's own tables](../technical/broadcast-ts.md) describe, so "the
same as the input" means a `.ts` for a recording that came off a disc. It is
the same stream. Ask for M2TS on the output settings screen and that is still
what you get -- and it says, there and then, that the tables are being left to
the muxer.

## One name for everything

This program hangs everything off **one string**: the list holds it, the seek
index and the proxy are cached against it, the output is named beside it, and
the demuxer is handed it. A recording inside a disc should not change that.

So a recording inside an image is named as though the image were a directory:

```
/rec/Anime_Test.iso/BDAV/STREAM/00001.m2ts
```

Nothing there is invented -- the image really does hold a file of that name.
The one unusual thing about it is that `/rec/Anime_Test.iso` is a file rather
than a directory, and that is exactly what
[`input.rs`](../../rust/crates/core/src/input.rs) notices. Splitting the path
where it stops being a directory gives three answers at once:

| | |
|---|---|
| **URL** | `subfile,,start,…,end,…,,:file:/rec/Anime_Test.iso` — what libavformat is given |
| **file** | `/rec/Anime_Test.iso` — what the operating system is asked about, so a cache keyed on size and modification time still has something to weigh |
| **range** | those bytes, for the passes that read the transport stream themselves |

The third is for [`si.rs`](../../rust/crates/core/src/si.rs). The pass that
reads the broadcast's own tables (PAT, PMT, SDT, EIT, SIT) walks the file
directly, so it has to open a **range inside an image** and to find packets
**192 bytes apart**. Both are in, which is why a cut taken from a disc carries
the broadcast's tables the same as a cut taken from a `.ts`.

## What this does not do

| | |
|---|---|
| **Encrypted discs** | Out of scope. Nothing here decrypts AACS. A pressed disc copied by a tool that removed it reads like any other |
| **Writing a disc** | Not supported. That is authoring, and a different problem |
| **Joining clips** | Not supported. A multi-clip playlist is shown as a row per clip (above) |
| **BDMV titles** | `index.bdmv` names titles and a title is a navigation program. Which playlist "T05 Extra 01" plays is not worked out; rows are named by the disc and the clip |
| **The CLPI EP map** | Not used for seeking. The stream list beside it is read, for the chooser; the index is still built by scanning packets. Adding the EP map to `IndexSource` in [`index.rs`](../../rust/crates/core/src/index.rs) would save that pass |
| **Blu-ray's own streams** | PGS and IGS cannot go on a cut timeline and are dropped, which the chooser says. A TrueHD track is written without the AC-3 core folded into its PID (above). LPCM (`pcm_bluray`) survives neither MP4 nor MKV and degrades to `bin_data` in TS; DTS-HD and VC-1 are read and listed but their cuts are untested |

## What was checked

Two real discs and a synthetic one.

**A real BDAV disc** — written by TMSR6, made into a UDF 2.60 image by ImgBurn:
3.4 GB, three clips, 1920x1080 MPEG-2 with AAC and ARIB captions, a partial
transport stream. All three programme names read correctly, the byte ranges
agreed with an independently written Python UDF reader, a 60 second cut came out
99.2% untouched, and the broadcast's own tables came through (PIDs 0x1100,
0x1101, 0x1102 and the SIT on 0x1F). **Copying the same clip out to a folder and
cutting the same range produced a file identical down to the md5.**

**A real BDMV disc** — a twelve episode season backed up by MakeMKV and made
into a UDF 2.50 image by ImgBurn: 33 GB, 45 playlists, 328 play items, 62
distinct clips. The disc's own name came out of `META`, the 62 rows came out
deduplicated, the twelve episodes were the twelve offered ticked, each carried
the four chapter points its own playlist wrote, and the byte ranges opened
through `subfile` and demuxed -- H.264 on 0x1011, TrueHD with its AC-3 on
0x1100 and 0x1101, PGS on 0x1200 and 0x1201. A ten second cut of one episode
came out at 91.8% copied, with both TrueHD tracks on their own PIDs and every
sample of them decoding.

**A synthetic disc** — [`tests/run_disc_tests.sh`](../../tests/run_disc_tests.sh)
builds one of each dialect out of the ordinary fixtures: the transport stream
is remuxed into 192 byte packets,
[`tests/disc_index.py`](../../tests/disc_index.py) writes the index files
around it, and genisoimage wraps each in a UDF 1.02 image. Then all four shapes
are asked the same questions, the cut each of them produces is compared **byte
for byte** -- against each other and against the plain stream -- and the tables
are checked to have gone back in. Thirty-five checks, all passing.
