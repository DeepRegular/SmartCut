# Reading a disc

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](disc.ja.md)

*The other direction — a night's cuts written back out as a disc — is
[Writing a disc](bdav.md).*

**A disc opens as a folder or as an `.iso`, either way.** Both halves of the
Blu-ray specification are read — BDAV, the recording format, and BDMV, the
format of a film you buy — and so is DVD-Video, which is
[further down](#dvd-video). What the list shows is the name of the programme,
not `00001.m2ts`.

```
$ smartcut Anime_Test.iso
disc  : Anime_Test.iso
        bdav -- Anime_Test
        3 recording(s)

  1  00:08:30.010  2026年08月17日01時00分-衛星第一-サンプル番組 てすとばんぐみ!にっ!! #07「架空の休息日の過ごし方。」  2 mark(s)
       0x1101  AAC stereo 48kHz
       0x1102  stream type 0x06
  2  00:08:30.010  …
```

A pressed disc carries no names, so its rows are the disc name and the clip
number. It also carries a great deal that is not the film, so the ones likely to
be worth watching are starred:

```
$ smartcut AnimeBox_Season1.iso
disc  : AnimeBox_Season1.iso
        bdmv -- Anime Box Season 1
        62 recording(s)

   1  00:00:43.543  Anime Box Season 1 00008  1 mark(s)
       0x1100  AC-3 stereo 48kHz eng
       0x1200  PGS eng
   2  00:00:11.511  Anime Box Season 1 00002  1 mark(s)
   …
*  8  00:11:52.003  Anime Box Season 1 00014  4 mark(s)
       0x1100  TrueHD multi 48kHz eng
       0x1101  TrueHD stereo 48kHz jpn
       0x1200  PGS eng
       0x1201  PGS eng
```

SmartCut asks about a pressed disc rather than swallowing it whole — see
[the chooser](#the-chooser), below.

In the window, drop an `.iso` or a disc folder and it asks which of the
recordings on it you meant. A folder holding several discs works the same way,
so an evening's worth of them becomes one list.

## The two dialects

**BDAV** is the **recording** format — what a set-top recorder writes, and
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
it — menus, a Java application, a second copy of the whole index under
`BACKUP` — none of which is a recording.

```
BDMV/
  index.bdmv             the titles, for a player's own menu
  PLAYLIST/00009.mpls    one way through the disc: which clips, in order
  CLIPINF/00014.clpi     that clip's own index
  STREAM/00014.m2ts      the transport stream itself
  META/DL/bdmt_eng.xml   what the disc is called
  BACKUP/                all of the above again
```

The two differ in four places — the directory's name, the playlist's extension
and magic, where the list of playlists comes from, and whether a playlist
carries a programme name — and agree everywhere else, **including the byte
layout of a play item and of a chapter mark**. So
[`disc.rs`](../../rust/crates/core/src/disc.rs) is one reader that is told
which dialect it is looking at, rather than two readers that would be the same
reader twice.

There is one thing BDMV has that cannot be read here. `index.bdmv` names
**titles**, and a title is a navigation or Java program rather than a playlist:
working out which playlist "T05 Extra 01" plays means running the disc's own
menu code, which is a Blu-ray player and not this. So the playlists are taken from the directory in sorted order. That is the order
the authoring tool numbered them in, and on every disc looked at so far it is
also the order a person would have chosen.

The stream is an ordinary MPEG-2 transport stream in 192 byte packets — a 188
byte packet behind four bytes saying when it arrived — which libavformat reads
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
**byte range** — and a byte range is something libavformat can be handed
directly, through its `subfile` protocol:

```
subfile,,start,747520,end,1232594944,,:file:/rec/Anime_Test.iso
```

The demuxer reads that as a stream and never learns there is a filesystem
around it. Everything downstream — the packet scan, the seek to an access
point, the copy — is the same code it was for a plain file.

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
not the file. [`Entry::contiguous`](../../rust/crates/core/src/udf.rs) joins the runs that
follow each other and returns `None` for a file genuinely written in pieces.
A burnt disc does not produce one, and refusing is better than reading at the
wrong offset.

### The metadata partition

An image written to UDF 2.50 or later has a **metadata partition**: the file
entries are kept together on it while the file data stays on the plain
partition beside it. A logical block number on that partition is a position
*inside a file*, and becomes a sector only by walking that file's own extents.
ImgBurn writes UDF 2.60 and uses one; genisoimage writes UDF 1.02 and does not.
Both are read.

**Encrypted discs are not handled and will not be.** AACS is a decryption
problem and this program has none of it.

What it does do is say so. A recorder encrypts the streams and leaves the
index in the clear, so such a disc lists its recordings perfectly and opens
none of them — and what libavformat says about bytes it cannot make sense of,
`Invalid data found when processing input`, is accurate about the bytes and
silent about the reason. An `AACS` directory beside `BDAV` is the disc saying
what the reason is. The chooser says it before anything is ticked, and an
open that fails says it instead of the demuxer's sentence. It is read only
where something has already failed or is being described, never as a refusal:
an image whose streams were decrypted where they lay would keep the directory
and open anyway, and the tools that move the streams out put the directory
somewhere of their own.

## What a playlist says about the recording

**On BDAV, a playlist is where everything a person reads lives.** Not only the
programme's name: a recorder writes the channel it came off and the three
digits that channel is known by, the moment the recording started, and several
hundred bytes of what the broadcaster said the programme was — the sentence a
listing prints, and the cast and staff under it. All of it at fixed offsets in
the description the playlist opens with, and all of it read here, because a
cut of that recording written onto a disc of its own should say the same
things. The offsets, and which tool writes which of them, are in
[Writing a disc](bdav.md#what-the-playlist-says).

`smartcut <disc> --title N` prints what it found:

```
title : アニメ 第03話「はじめての遠出」ほか
        衛星第一 (161)  2010-02-14 00:30:00
        美術科に入学した主人公が、学校の前のアパートでひとり暮らしを始める。…
```

## The programme's name is ARIB text

**On BDAV.** A `.rpls` does not carry the name in UTF-8. It is written in the ARIB STD-B24
eight-unit code — a descendant of ISO 2022 that holds four graphic sets at
once (kanji, alphanumerics, hiragana, katakana) and moves between them with
shifts and escapes, with the size and colour controls in the same byte stream.

[`arib.rs`](../../rust/crates/core/src/arib.rs) **reads** it; it does not render
it. The sizes and colours are dropped and the characters come out in order. The
JIS X 0208 to Unicode table is not written down here: EUC-JP is JIS X 0208 with
the high bit set on both bytes, so the table a UTF-8 world already has is the one
this needs. That leaves a single dependency, `encoding_rs`, and one table of its
own — the rows JIS never assigned, which no encoding a UTF-8 world has fills the
way ARIB fills them.

Two things are worth getting right:

- **Rows 85 and up of JIS are ARIB's own symbols, and they have to be named.**
  JIS leaves those rows unassigned and ARIB fills them with symbols of its own.
  Sending them through the mapping table of an encoding that *does* fill those
  rows — which is where a general purpose decoder would send them — produces
  entirely different characters, so a decoder that will not name them itself has
  to answer `〓`, which is what a receiver with no glyph for them shows.

  Answering `〓` for the whole of those rows was the first thing tried, on the
  grounds that a programme name does not carry weather symbols. **It does carry
  a season number.** A series in its third season is `Ⅲ` and that is row 94 cell
  3, not JIS and not an ASCII `III`; the name came back with the geta mark in
  the middle of it, and because the same text is what gets written to a disc,
  the geta mark went onto the disc and stayed there. The same rows hold the
  kanji JIS X 0208 left out — `髙`, `﨑` — which a Japanese name reaches for
  regularly. So all of them are named now, from ARIB STD-B62's own mapping:
  rows 85 and 86 the kanji, 90 the traffic marks, 91 the map marks, 92 the units
  and numbers, 93 the weather and the fractions, 94 the numerals. A cell nothing
  is assigned to is still the geta mark. So is a downloaded DRCS glyph *here*:
  a name is text, and a glyph a broadcaster sent the dots of is not text. The
  caption reader is handed those dots and draws them; see
  [The characters the broadcaster draws](broadcast-ts.md#the-characters-the-broadcaster-draws).

  **Cells 48 to 84 of row 90 are spelled rather than drawn.** They are the
  bracketed markers a listing carries — `[新]`, `[字]`, `[終]`, `[再]` — one
  boxed glyph on a television, and the word inside the box everywhere else.
  Unicode does have characters for them, but they are ones half the fonts on a
  machine cannot draw, and `[新]` is what a listing says.

  **Written out, the word goes back into the cell it came from.** Three
  characters where the recording had one is the name a listing prints rather
  than the name the broadcast sent: ten bytes where the cell costs two, and a
  television drawing `[`, `新`, `]` where it would have drawn the box. So the
  spelling is matched on the way out — but only the ones spelled as a word in
  brackets. Three of the thirty-seven are spelled as ordinary text, `■`, `●`
  and `ほか`, and a programme name carries those on its own account: a title
  ending `…遠出」ほか` would otherwise have its last two kana swallowed.

  **The trap has a second end.** `Ⅲ`, `①`, `㈱`, `℡`, `㎏` are all in row 13,
  which JIS X 0208 also leaves unassigned and which the encodings a UTF-8 world
  reaches for fill with a vendor's additions. So writing a name *out* through
  EUC-JP put the numeral in a row ARIB has nothing in, which a receiver draws as
  nothing. Row 13 is refused on the way out for the same reason rows 85 and up
  are refused on the way in, and ARIB's own cell is written instead — designated
  into G0 with an escape and designated back afterwards, which is exactly what
  the broadcast this was found in does.
- **The last eight cells of a kana set are punctuation, not kana.** Rows 4 and
  5 of JIS are not full, and ARIB spends what is left on `ー` `。` `「` `」`
  `、` `・`. Miss them and a programme name reads
  `#07〓架空の休息日の過ごし方。」`.

A `.mpls` carries no name at all, because a film's titles live in the menu,
which is a Java application. So a pressed disc's rows are named by the disc and
the clip: `Anime Box Season 1 00014`. What the disc calls itself comes out of
`META/DL/bdmt_*.xml`, scraped rather than parsed: the file is a document with a
dozen namespaces declared and one interesting element in it, and pulling in an
XML parser just to reach `<di:name>` would be by far the largest dependency in
the program.

## What a row is

**One clip, one row** — not one playlist, one row.

On BDAV those are the same thing until a playlist plays more than one clip,
which is what a recorder writes when a programme ran past the length it splits
its streams at. Those become a row each, named `(1/3)`, `(2/3)`, `(3/3)`.

On BDMV they are not the same thing at all. A pressed disc names the same clip
from several playlists: once in a playlist of its own, again inside the "play
all", and again in whatever the menu's chapter list points at. On the disc this
was written against, **45 playlists name 328 play items between them, and there
are 62 distinct clips.** One row per play item would offer the same episode
three times.

So a row is keyed on the part of a clip a playlist plays — the clip's number
and its IN and OUT to the tick — and the first playlist to name it makes the
row. Keyed on the part and not on the clip alone because a recorder can write
two programmes into one stream and a playlist for each half, and those are two
recordings.

Joining a multi-clip playlist into one timeline is a separate piece of work:
each clip carries its own clock, and splicing two of them is not the same
operation as cutting one. Until that exists, showing the pieces is the honest
thing to do — every second of the disc is reachable, and the list says plainly
that it is in pieces. A pressed disc's "play all" is *not* a recording in
pieces, which is why its rows are not numbered `(1/15)`: the disc never
asserted that relationship, and claiming it would be inventing one.

### The chooser

A recording is one thing and needs no question asked about it. A pressed disc
is not: those 62 rows are twelve episodes among fifty logos, warnings, menu
loops and eight second transitions, and the disc calls all of them `000NN`.

So the index is laid out and the question is asked once, before anything is
opened. That is why everything in the dialog is something the index can answer:
a length, a size, and a track named by the PID it sits on.

Clips over **five minutes** are offered already ticked, which on that disc is
exactly the twelve episodes; the rest are folded away behind one checkbox. The
threshold is well under the shortest thing anybody keeps a disc for and well
over the longest thing a menu is made of. A disc where nothing clears it is not
a disc holding nothing, so the longest clip is ticked instead. A BDAV disc is
all offered: it is a disc of things somebody chose to record.

### What a clip carries

The tracks under each row come out of that clip's `.clpi`, which is the only
source cheap enough: nobody would wait for a chooser that had to demux thirty
gigabytes to draw itself. What the demuxer says later is the authority at
cutting time; this is what the disc says it wrote.

```
Video      / H.264 1080p 23.976fps / PID 0x1011 / the video cannot be left out
Sound      / TrueHD multi 48kHz    / eng / PID 0x1100
Sound      / TrueHD stereo 48kHz   / jpn / PID 0x1101
Subtitles  / PGS                   / eng / PID 0x1200
Menu       / IGS                   / eng / PID 0x1400 / a cut cannot carry this
```

The two dialects sign that file differently and write the same thing after it
— a pressed disc's opens `HDMV`, a recorder's opens `M2TS` — and a recorder
cuts the language field short, so a BDAV row shows `AAC stereo 48kHz` with no
language rather than three bytes of whatever followed. A broadcast's private
streams, the captions among them, are named by their number: what a private
stream holds is not something the disc's index says, and the editor's own track
menu — which reads the recording rather than the index — names them properly.

Three kinds of track are listed, and only one of them is a choice. The video is
what a cut is *of*. The graphics a Blu-ray's subtitles and menus are made of are
each a little display list rather than a run of timed packets, and there is
nowhere on a cut timeline to put one. They are listed to say they are being left
behind, not to offer a choice about them.

Every episode on a disc carries the same tracks, so there is a button that
copies one row's answer to every row with the same track list. Being made to
answer the same question twelve times is not the same as answering it once.

**And the language goes with the cut.** A Blu-ray's programme map carries no
language descriptor — the language is in `CLIPINF` and nowhere else — so a
demuxer handed the `.m2ts` alone has nothing to go on, and a cut of a disc
that listed `eng` and `jpn` used to arrive with two sound tracks nobody could
tell apart: none in the transport stream it was written to, and none in the
clip index of a disc written from it either, since that index is read back off
the stream. So `disc::carry_disc_languages` fills the tracks in when a
recording on a disc is opened, matched on PID, and the map the cut writes
carries an ISO 639 descriptor for each. Only where the stream itself declared
none: a broadcast that said so said it nearer the sound than any index does.

### A track is named by its PID

The chooser answers **before anything is open**, whereas a stream index is
something libavformat only produces once it has read the recording. So the
answer travels as a list of PIDs and is resolved by whoever opens the streams:
the editor when the row is opened in it, and the backend when it is written
out.

A PID can name more than one stream. A Blu-ray's lossless sound arrives as a
TrueHD track with an AC-3 track folded into it, **both on the one PID**, and
libavformat hands them over separately:

```
stream|index=1|codec_name=truehd|channels=6|id=0x1100
stream|index=2|codec_name=ac3   |channels=6|id=0x1100
```

Switching that track off has to switch off both halves of it. Asking by PID
does that; asking by stream index would have switched off only one of them.

It is also why **only the first stream on a PID is written**. A cut puts each
stream back on the PID it arrived on, and two streams cannot share one — ask the
muxer for that and it stops with:

```
[mpegts] Duplicate stream id 4352
Error: Invalid argument
```

The first stream on a PID is the one the programme map named; anything after
it is a piece the demuxer split out. So the TrueHD is written — a TrueHD
elementary stream on its own is a track a player decodes — and the AC-3 core
folded inside it is left out, and said to have been left out, in the track
menu and in what the command line prints:

```
        not carried: a compatibility stream folded into the track written on pid 0x1100
```

The chooser's answer holds only until the editor gives one. The track menu
there writes stream indices into the edit, and from that moment the edit speaks
for the row — otherwise a track switched back *on* in the editor would be
switched off again on the way out by an answer given before anybody had seen
the recording.

### The sound a disc carries

A broadcast recording carries AAC and nothing else. A disc carries five other
things, and each of them is a different problem on the way out.

| On the disc | What a cut does with it |
|---|---|
| **LPCM** (`pcm_bluray`) | Copied into a transport stream, and **smart rendered**: LPCM has no encoder delay and no window overlap, so the frame a boundary lands inside is rewritten with the far side silenced and nothing else moves. Into an MP4 or an MKV it is written as big-endian PCM — `ipcm`, or `twos`/`in24` in a QuickTime file — at the recording's own width, because neither container has a box for Blu-ray's own framing |
| **DTS**, **DTS-HD**, **DTS-HD MA** | Copied, byte for byte, into every container |
| **TrueHD** | Copied, byte for byte. Written without the AC-3 core folded into its PID (above). In an MP4 it needs two allowances, and says so: the `mlpa` box is outside the standard, and the track has to open on one of the stream's own sync points — about 13 ms of head, in the streams measured here |
| **E-AC-3** | Copied |

**Nothing lossless is ever re-encoded.** The encoders libavformat has for DTS
and TrueHD write the lossy core and drop what makes them lossless, so a
"patched" boundary frame would be a hole in the track rather than a trim.
Asked for a whole-track re-encode of one, or for a downmix, which is a
re-encode by another name, a cut declines and says so rather than obeying or
failing.

**LPCM into an MP4 loses nothing.** The samples pass through a 32 bit float,
whose 24 bit mantissa holds every value Blu-ray LPCM can carry — and Blu-ray
LPCM goes no deeper than 24 bits. What comes out of the MP4 is what went into
it, sample for sample; only the box around it changed.

The stream types are the disc's own. libavformat's own transport stream muxer
knows none of the first three — asked to write LPCM it declares "private
data", and asked to write E-AC-3 it reaches for ATSC's `0x87` rather than
Blu-ray's `0x84` — but a cut written as a `.ts` keeps
[the recording's own tables](broadcast-ts.md), and that is what
the numbers come from.

### The subtitles a disc draws

A broadcast *writes* its subtitles: an ARIB caption statement is one packet
with one timestamp, and carrying one across a cut is carrying one packet. A
disc *draws* them. What travels is a **display set** — a composition saying
where things go, a window, a palette, and the picture itself run-length coded
— and each piece is its own PES packet with its own timestamp:

```
PCS  WDS  PDS  ODS  END      the subtitle appears
PCS  WDS  END                and later, the plane is cleared
```

So the packets are timed the way a caption's are and travel the same way, but
they are timed *in groups*, and a group means nothing in halves. That is the
whole of the difference, and it comes to three rules — [`pgs.rs`](../../rust/crates/core/src/pgs.rs):

1. **A display set is carried whole or not at all.** One that straddles the
   end of a range is left behind: its pieces would arrive without the
   composition that gives them meaning.
2. **What is on screen when a range opens is put up again.** The set that drew
   it was left behind with the material before the cut, so the cutter reads
   the eight seconds in front of every kept range, keeps the last display set
   that can stand on its own, and sends it again at the range's first frame —
   marked as opening an epoch, because for this output it does.
3. **What is on screen when a range ends is taken down.** The set that would
   have cleared it is in the material after the cut. The clear is the standing
   composition with its objects removed and its number advanced, which is
   exactly what the disc itself sends to empty a plane.

Rules 2 and 3 are why the eight seconds are read at all, and what a disc does
says how much is enough. Measured over half an hour of one disc's feature:
1334 display sets, 386 of them opening an epoch, 561 **acquisition points** —
the same subtitle sent again, whole, so that a player joining mid-way has it —
and 387 clearing the plane. A subtitle stands for 2.3 seconds at the median
and 116 at the longest, but the gap between one self-contained set and the
next never exceeds 2.8 seconds: however long a subtitle stays up, this disc
keeps re-sending it. Eight seconds covers that several times over, and covers
all but the longest-standing subtitle on a disc that never re-sends one at
all. The read costs a second per kept range and happens only where the
recording has graphics in it.

**Where they can go.** Into Blu-ray's own framing, where libavformat writes
the stream type a player expects. Into a plain `.ts` as well — but there the
muxer writes them as private data of no stated kind, and everything reads that
back as `bin_data`: carried, declared, and invisible. So the map written over
the muxer's own says `0x90` and registers the programme as HDMV, which is the
same correction [the disc's LPCM needs](#the-sound-a-disc-carries) and is made
in the same pass. Into an MP4 they cannot go at all, and a cut asked for one
says so — and offers the other answer, which is to write them
[beside the cut](#a-discs-subtitles-inside-the-cut-or-beside-it) instead of
inside it.

Menus (IGS) are the same kind of stream doing a different job: a button has a
state and a target, and both point into a timeline the cut has just taken
apart. Those are listed to say they are being left behind.

**They can also be drawn over the editor's preview.** That is a different
question from carrying them — what is on screen at this instant (`subs.rs`) —
and here libavcodec does the decoding: PGS and a DVD's subpictures both come
back as one byte per pixel and a table of colours, which
[`vobsub::drawn_from`](../../rust/crates/core/src/vobsub.rs) already puts into
one shape. That becomes a PNG cropped to the subtitle's own rectangle, and the
window places it on the same part of the screen. A DVD's palette is not in the
stream, so here too it is read off the disc's index
([below](#a-discs-subtitles-inside-the-cut-or-beside-it)). How a broadcast's characters are
read is in [Broadcast TS](broadcast-ts.md#reading-the-captions-to-draw-them-over-the-preview).

### A menu is not a stream, and the index is the only thing that knows

Two of the things a Blu-ray carries are named by its own index and by nothing
else, and both of them are left behind — [`disc.rs`](../../rust/crates/core/src/disc.rs):

| | |
|---|---|
| **A menu** (`0x91`, IGS) | Interactive graphics: pictures, and buttons with states and navigation commands. The commands point at playlists and titles that do not exist beside a cut, and the composition is timed against a playlist the cut has just taken apart |
| **Text subtitles** (`0x92`, TextST) | Text with styling, set in a typeface that lives in the disc's `AUXDATA` rather than in the stream. What travelled into a cut would be text nothing could draw — and nothing would try: libavcodec has the codec id `hdmv_text_subtitle` and no decoder behind it |

**Asking a demuxer what is on those PIDs gets an answer, and the answer is a
guess.** On the disc measured here the menu on `0x1400` came back as **MP3
audio**, in a clip that also carries pictures and real sound. A cut that
believed it would have written a menu into the output declared as audio; what
saved it was that the same probe could not find a sample rate to go with the
guess, and a track with no rate is left out a few lines later — under a note
calling it "the sound on pid 0x1400".

So the index answers instead. A PID it says carries either of these is kept
out of every list a probe would have put it in, and named as what it is:

```text
audio  : pcm_bluray 48000Hz 2ch  jpn  main   [stream 1 pid 0x1100]
        not carried: a menu on pid 0x1400
```

A clip that is *only* a menu — which is what a disc's menu clips are, graphics
and no pictures at all — says so rather than failing as a recording with
something missing.

### The marks

Chapter marks are read **only when they can be believed**. The size of a mark
is given away by the section's own length, but what sits where inside one is
not the same in both dialects: BDMV's mark is fourteen bytes — a byte
reserved, the mark's kind, the play item it belongs to, then the time — and
the marks a BDAV recorder writes are longer and carry a name and a thumbnail
reference beside the time. So the layout is not assumed. Each candidate is
tried and the one whose times **all land inside the clip they claim** is the
one that is used. When none of them does, the marks are left out: a chapter
point in the wrong place is worse than no chapter point.

Reading **which play item a mark belongs to** is what makes a "play all"
usable. Without it every mark is a number with no clock under it: the fifth
episode's chapter points are on the fifth episode's own timeline, which shares
nothing with the first's beyond both starting near eleven seconds. A layout
that cannot say which clip it meant is believed only on a playlist that plays a
single clip. Otherwise every episode's chapters would land on episode one, and
placing none at all is better than that.

That is also why the marks on a row are taken from the **shortest** playlist
that offers them. A disc names an episode both in a playlist of its own and
inside the "play all", and only the first of those can be read without knowing
which episode it meant.

### The marks become keyframes

A recording opened off a disc arrives in the cut editor with those chapters
already marked. On a Japanese recording they are frequently the commercial
breaks themselves — written down, exactly, by the machine that made the
recording, which is the same answer [commercial
detection](../user-guide/cm-detection.md) spends minutes looking for.

The one thing in the way is that a mark and the timeline are on **different
clocks**. A playlist counts in the stream's own 45 kHz, and everything the
editor draws is rebased to the container's start time, so a mark is placed at

```
entry.start + mark - src.start_time
```

`entry.start` being the playlist's IN point, carried beside the marks for
exactly this. Nothing on the disc-reading side knows `start_time` — that comes
from opening the stream — which is why the marks travel with the row as far as
the editor rather than being turned into times where they are read.

They are placed on a first visit only, and a `.keyframe` file beside the
recording wins over them: that file is somebody's answer, while the disc's marks
are only the default when nobody has given one. Marks landing outside the
material are dropped rather than clamped, for the reason above. The times can be
seen without opening the window: `smartcut <disc> --title N` prints them on the
recording's own clock.

## The disc's own index

**A Blu-ray has already made the pass.** Beside every stream, in
`CLIPINF/000NN.clpi`, it records where each picture a player may start at is —
on the presentation clock, and as a packet number in the file. That is the same
list the walk over the packets — [`index::PacketScan`] — spends a read of the
whole recording to arrive at.

So it is read instead. `disc::entry_points` parses the map and
`index::DiscIndex` presents it as an [`IndexSource`], which is the seam that
was left for exactly this. On the UHD disc measured here — one clip, 81 GB,
2 hours 34 minutes — opening the title went from **8 minutes 45 seconds to
under a second**, and the plan that came out of it was identical, segment for
segment, to the plan the walk produced.

The map is two tables. A **coarse** entry carries the top of a timestamp and
the top of a packet number and points at the first of the **fine** entries that
fill in the rest; a time is therefore one of each, put together. The layout is
`libbluray`'s, with one thing the format's own description leaves out: the map
for a stream opens with a four-byte offset to its own fine table, and the
coarse table starts after that.

**And a coarse entry does not always carry the top it should.** The seventeen
bits a fine entry holds for the packet number run out every 131,072 packets,
about twenty seconds of a recording, and a coarse entry is written each time
they do. On one clip of the recorder's discs measured here, nine of them state
a top one such step behind, which combined with the fine entry reads as the map
turning round and going back 25 megabytes — and put nine entry points, with
times from a later stretch of the clip, inside an earlier one. The other
nineteen clips on those four discs are clean, and so is every pressed disc
measured. Entry points are places in a file and a file goes one way, so a
position that would go backwards is stepped on by whole seventeen-bit turns
until it does not. Nineteen of the twenty clips come out byte for byte as they
did before.

**What the map cannot say** is whether a GOP is open, or whether the leading
pictures hanging off one may be thrown away — that is in the bitstream, not in
any index. So the points arrive with `leading_known: false` and
`index::refine_leading` measures the ones a boundary can actually land on: a
couple of dozen either side of each end of each range, a seek and a short read
apiece. Measuring every point inside the range instead — which is what it used
to do, invisibly, because a walk answers for itself and this never ran — took
ten minutes on those sixteen thousand entry points, which is to say the disc's
own index bought nothing at all.

It cannot say what the pictures weigh either, and a re-encoded stretch is
written at [the rate the recording came in at](rust-core.md#the-splice). Where
nothing counted them, the file's own rate less what the sound is worth stands
in — on a Blu-ray that is most of the difference, uncompressed sound being the
loudest thing in the file after the pictures.

[`IndexSource`]: ../../rust/crates/core/src/index.rs
[`index::PacketScan`]: ../../rust/crates/core/src/index.rs

## Where a cut goes, and what it is called

There is nowhere to write inside a disc: inside an image there is no folder at
all, and inside a copied disc there is nothing that belongs to anything but the
disc. So a cut is written **beside the disc**, under the **programme's own
name**.

```
/rec/Anime_Test.iso
/rec/cut_2026年08月17日01時00分-衛星第一-サンプル番組 ….ts
```

The characters a filesystem will not take (`\ / : * ? " < > |`) become their
full width forms, which is what a Japanese recorder does with the same problem;
the name still reads.

**The container is `.ts`.** Asked for a `.m2ts`, libavformat writes Blu-ray's
own shape: 192 byte framing, and Blu-ray's own PID numbering. Both are the
muxer's to decide and neither is the layout that
[the broadcast's own tables](broadcast-ts.md) describe, so "the
same as the input" means a `.ts` for a recording that came off a disc. It is
the same stream. Ask for M2TS on the output settings screen and that is still
what you get — and it says, there and then, that the tables are being left to
the muxer.

## One name for everything

This program identifies a recording by **one string**: the list holds it, the
seek index and the proxy are cached against it, the output is written beside it,
and the demuxer is handed it. Supporting recordings inside a disc should not
change that.

So a recording inside an image is named as though the image were a directory:

```
/rec/Anime_Test.iso/BDAV/STREAM/00001.m2ts
```

Nothing there is invented: the image really does hold a file of that name. The
one unusual thing about it is that `/rec/Anime_Test.iso` is a file rather than a
directory, and that is exactly what
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

### A clip whose clock restarts

A recorder stops and starts. Each time it does, the stretch it writes next
begins a **new sequence** with a clock of its own, and all of them go into one
`.m2ts`. The BD-REs measured here hold five, six or seven of them in a clip.
The playlist says so plainly: one play item per sequence, each naming the
sequence its IN and OUT are on.

Read whole, that file used to be unreadable in a way nothing announced. Its
entry-point map counts 3,598 points whose byte offsets rise all the way and
whose **times go backwards twice** — 7.1 seconds to 1.1 at the fourth point,
and again at the last. A time no longer picks out a place. libavformat declines
to measure the file at all, so every `--keep` was refused for beginning after a
recording that ends at zero. The stream is not damaged: read from the front it
decodes to the end.

**So the clocks are put back together as the file is read.** That is
[`restamp.rs`](../../rust/crates/core/src/restamp.rs), and it is the whole of
the join: each stretch is given a place on one timeline, one after another in
the order the file holds them, and every timestamp inside it is moved by the
difference. A presentation time lives in a fixed five bytes of a PES header and
a program clock reference in a fixed six of an adaptation field, so **nothing
moves and nothing changes length** — the clip's own entry-point map still
points where it pointed, the seek index still addresses the same bytes, and the
demuxer reads a stream whose clock runs from one end to the other.

Nothing is remembered either. Which stretch a byte belongs to is a property of
where it is in the file, so the correction for any packet is worked out from
its position alone. That is what makes it seekable: libavformat may jump
anywhere, and the packet it lands on is corrected by the same amount it would
have been corrected by had the file been read from the beginning.

Where each stretch goes is asked of two witnesses, because neither knows the
whole of it. The **sequence table** states what a stretch presents, which is
what a play item plays and not what the file holds: on the clip measured it
declares 892.94 seconds where the pictures run to 894.5, because a recorder
writes to the end of a group and edits inside it. The **entry-point map** knows
where pictures are, but only at the places a player may start, so it stops a
group short at either end. What the map does give is the rate — how many bytes
of this stretch a second of it takes — and the bytes past the last entry point,
read at that rate, are how much longer the stretch goes on. Neither witness is
overruled: whichever says the stretch is wider is the one taken, so nothing a
play item plays falls outside. The first stretch is left where it is, so a clip
read from the front reads at the times it always did.

**What neither witness covers is the material either side of what is played.**
A play item's IN is a picture; the sound at that moment began a frame or two
before it, and the file holds those frames. Given a place that begins at the
picture, they land behind the end of the stretch in front, where the output
timeline has already been written, and the cut leaves them out. Measured on the
recording here, a seam costs 8 sound frames on each track and 3 pictures. The
instants themselves are covered by the stretch in front of them, so what is
left out is the second account of them and not a hole — which is what the note
at the end of a cut now says. It used to say the recording was damaged.

The row is then the whole clip under the plain name it always had:

```
/rec/Recording.iso/BDAV/STREAM/00001.m2ts
```

A demuxer in this program is opened from a string and from nothing else, so
what it is handed is that name wrapped in a `restamp:` URL carrying the table.
[`input.rs`](../../rust/crates/core/src/input.rs) reads through an ordinary
libavformat i/o context of its own and corrects whole source packets on the
way past, which is why `subfile` and `concat` keep doing what they do.

**A seam is planned as a cut.** The pictures at the start of a stretch
reference pictures from before the recorder stopped, which were never written
down; copied straight across, a decoder shows the wreckage until the next
picture that restarts it, which on a recorder's own stream can be a minute
later. So `plan::plan_on` cuts every kept range at the seams before planning
it, and the far side of each opens with a re-encoded head that starts a coded
video sequence of its own. The output timeline closes up behind it because a
segment occupies the fields it writes and not the times it came from. On the
recording measured, a full cut is 6 ranges and **99.4% copied**.

A row is joined only where the playlist accounts for the whole clip: every
sequence it holds, in the order the file holds them, one play item each.
Anything else is left as it was — a row per item, each named by the packets it
plays, the way a DVD title is named by its sectors:

```
/rec/Recording.iso/BDAV/STREAM/00001.m2ts@8960-4605951
```

A clip with one sequence is named whole, which is every disc that came before
these and every disc this program writes. So is a clip whose sequence table
does not read as one: the sequences have to begin at the front of the clip,
each after the one before it, and all of them inside the file.

**Measured**, on four BD-REs a recorder wrote: 20 recordings that listed as 106
rows now list as 20, each the length its own index states — 44:19 to 47:57
against broadcast lengths of 44:41 and 45:10. The four reference discs, the
pressed Blu-rays and the DVDs list exactly as they did.

## DVD-Video

A DVD arrives by the same door and shares nothing else with a Blu-ray, so it is
read by [`dvd.rs`](../../rust/crates/core/src/dvd.rs) and not by
[`disc.rs`](../../rust/crates/core/src/disc.rs). `disc::read` asks both and the
one that recognises the disc answers; everything downstream is handed the same
`Disc`, `Entry` and `Track` as before.

```
$ smartcut documentary-01.iso
disc  : documentary-01.iso
        dvd -- documentary-01
        2 recording(s)

  1  00:59:51.400  documentary-01 1 (1/2)  18 mark(s)
       0x01e0  MPEG-2 720x480 NTSC 4:3
       0x0080  AC-3 2ch 48kHz  ja
  2  00:00:08.342  documentary-01 1 (2/2)  1 mark(s)
```

Three things about a DVD decide the shape of the reader.

### The stream is one stream, not nine

`VTS_01_1.VOB` through `VTS_01_9.VOB` are **one** MPEG program stream that the
format made the disc write in pieces of no more than a gigabyte, because ISO
9660 could not address more than that. The disc addresses it as one run of
sectors numbered from zero, so a cell beginning at sector 536,000 is 400 MB
into the *third* file.

The pieces are laid down one after another with nothing between them. On the
disc this was written against that is exact:

| | offset | end |
|---|---|---|
| `VTS_01_1.VOB` | 24,727,552 | 1,098,182,656 |
| `VTS_01_2.VOB` | 1,098,182,656 | 2,171,582,464 |
| `VTS_01_3.VOB` | 2,171,582,464 | 3,244,935,168 |
| `VTS_01_4.VOB` | 3,244,935,168 | 4,289,400,832 |

So inside an image the whole title set is **one byte range**, and a title is
one `subfile` the same as a Blu-ray clip. On a folder the pieces are nine
files, and libavformat's `concat` protocol joins them — with `subfile` around
the join to take the title out of it. Nothing is unpacked and nothing is copied
first, either way.

Nesting one protocol inside another needs saying: whatever is allowed at the
top level, the protocol *inside* one is checked against a whitelist holding
`file` and nothing else. `input::demux` sets `protocol_whitelist` for the URLs
this program writes, and never for a plain path.

### A title is a run of cells

Where a Blu-ray gives an episode a stream of its own, a DVD gives it a stretch
of the one stream, and the `.IFO` tables are the only thing that says where:

| table | what is taken from it |
|---|---|
| `VIDEO_TS.IFO`, `TT_SRPT` | the disc's titles in the order a player numbers them, and which title set each is in |
| `VTS_NN_0.IFO`, `VTS_PTT_SRPT` | for each of the set's titles, the `(chain, program)` pairs that are its chapters |
| `VTS_NN_0.IFO`, `VTS_PGCIT` | each program chain: its cells, their lengths, the sectors they play, and which cell each program starts at |
| `VTS_NN_0.IFO`, `VTSI_MAT` | the picture, and the sound and subpicture tracks the set declares |

A chapter's position is the sum of the lengths of the cells before it, which is
why the cells have to lie end to end for any of this to mean anything. A chain
whose cells do not is an angle block, or one assembled out of pieces of several
titles, and it is not read at all: the span its cells happen to lie inside is
not the title, and saying nothing is better than offering that.

Since a title is a stretch and not a file, its name has to say which stretch:

```
/rec/documentary-01.iso/VIDEO_TS/VTS_01_1.VOB@0-2081904
```

The sectors are the disc's own, counted from the start of the title set's
stream. A name that said only `VTS_01_1.VOB` would name four gigabytes of
everything the disc holds and no episode in particular.

### The clock is in the stream, not the index

Cell times are durations counted from the start of the title. The program
stream's timestamps begin wherever the author's multiplexer began them — on
this disc, at 0.281 seconds. A chapter "twelve minutes in" is not a time on
that clock until something joins the two.

The navigation pack does. Every VOBU opens with one: 2048 bytes a player reads
and does not show, holding `VOBU_S_PTM` — when the pictures behind it are to
be presented — and `VOBU_E_PTM`, when they stop. Reading the one at a title's
first sector costs a single 2048 byte read and gives exactly the number the
editor needs, which is why `Entry::start` for a DVD is a real measurement and
not the index's guess.

**And it is what says where a title has more than one clock.** A DVD is allowed
to assemble a title out of pieces that were multiplexed separately, and the
timestamps start again at the seam. The disc this was written against does it:

```
cell 18 ends at   323,571,480  (3595.24 s)
cell 19 begins at      36,010  (   0.40 s)
```

Splicing two clocks is not the same operation as cutting one, which is already
why a Blu-ray playlist of several clips becomes a row each. So a DVD title is
cut at its seams too, and the disc above comes out as an hour with eighteen
chapters plus an eight second tail beside it, rather than as one row whose
second half sits on top of its first. Two cells belong to the same run when the
second begins where the first left off, and where the first left off is read
from that cell's own last VOBU, which the index points at.

The disc's two titles are the same nineteen cells and the same nineteen less
the last, which between them are two things and not three, so rows are
deduplicated on the stretch of stream they name.

### Program streams leave the timestamps out

A program stream does not carry a presentation time on every picture. Where
the display order can be worked out from the pictures around it, the
multiplexer leaves it off — on this disc, one picture in four:

```
pts=31263  dts=22254  K     pts=N/A    dts=31263
pts=25257  dts=25257        pts=34266  dts=34266
pts=28260  dts=28260        pts=N/A    dts=40272
```

Every pass here is keyed on presentation time, so a picture without one used to
be skipped, and a lossless copy came out with three quarters of its frames.
libavformat will work them out — `fflags +genpts` reorders from the decode
timestamps and is exact — but only if it is asked before the file is opened.
`input::demux` asks for program streams and nothing else: a transport stream
times every picture it carries, so the flag would change nothing there, and a
flag that changes nothing is still a flag on the path every recording goes down.
(It reopens a recording for one other reason,
[below](#a-disc-that-does-not-say-how-often-pictures-arrive), on the same
principle.)

The other thing a program stream does not carry is **its own length.**
libavformat works one out from the timestamps at either end of the file, and on
a DVD — four gigabytes with a discontinuity in the tail — it came back with
8.3 seconds for an hour. The pass that reads every packet has the better answer
and now reports it, as `index::Index::end`; the container's own answer is kept
wherever it is the longer of the two, so a container that knows its length
keeps it.

### The container's seek table has to cover the recording, not just reach the end

`--index auto` asks the container for its seek table before walking the packets,
because reading a table is free and walking four gigabytes is not. A program stream
has no such table. What libavformat hands back for one is whatever its probe happened
to index on the way to the first few frames — on a twelve and a half minute title,
**eight entries covering the first four seconds**.

Taken at face value that is worse than no index at all. A cut anywhere past the
fourth second finds no entry point to copy from and re-encodes the whole title, and
`--scenes` pulls every boundary in the recording onto the same picture.

So the table is asked the one question that costs nothing: **does it cover the
recording**? A table that stops where the probe stopped is declined and the walk
answers instead. The same cut then copies 98.2%, and the scene list has 249 entries
rather than one repeated.

Asking only whether it *reaches the end* is not enough, and a 1999 DVD showed why.
libavformat measures a program stream's length by seeking to the far end of the file
and reading what is there, and it indexes that packet like any other. The table it
left behind for an hour-and-a-half title was **ten entries over the first five
seconds and one entry at 5110.6s of 5110.9s**. It reaches the end. It holds nothing in
between, and it passed. What came of it was not a bad cut but a bad editor: the film
strip draws its cells on entry points, so a window anywhere in that hour and a half
had two of them to draw with, and stepping by keyframe had nowhere to step to.

So the hole is asked about as well, and against the recording's length rather than
against the table's own spacing: gaps in a real table vary by more than a factor of
three, because an encoder puts an entry wherever the picture changes, and what is
being caught here is not an uneven table but an empty one. A gap wider than a tenth
of the recording is not a table a container kept.

The walk that answers instead read the whole four-gigabyte title in **3.2 seconds**,
which is the other half of why this is the right way round: the table was never
saving much on a DVD, and it was costing everything.

`examples/indexdiag.rs` asks each of the three sources separately and prints what each
one says, which is how a table that should have been declined is found: nothing goes
wrong loudly when an early source answers badly — the recording opens, and the editor
is drawn from whatever entry points it was handed.

### A DVD's sound is LPCM in a stream nothing else declares

DVD linear PCM rides in a private stream whose framing only a DVD describes. Written
out under the stream type that means "some private data" — which is what the muxer
reaches for — every reader named the track `bin_data` and played silence.

It goes out as **Blu-ray LPCM** instead, which is the shape a transport stream has for
exactly these samples. That alone fixes an `.m2ts` and a disc, where the muxer
declares it properly. A plain `.ts` needed
[the map written again](broadcast-ts.md#putting-the-recordings-own-tables-back),
and a recording that never was a broadcast had no map pass at all; it gets one now,
rebuilt from what the muxer itself wrote, with that one correction and nothing else
added.

### A disc's subtitles: inside the cut, or beside it

A DVD draws its subtitles the way a Blu-ray does — a run-length coded picture
with commands saying where to put it — and there the resemblance stops. A
Blu-ray's graphics have a transport stream type of their own, `0x90`, and
[travel inside the cut](#the-subtitles-a-disc-draws). **A DVD's have none.**
Written into a `.ts` they become private data of no stated kind and everything
reads them back as `bin_data`: carried, declared, and invisible. There is no
number to correct that with.

So there are two destinations, and `--subtitles` chooses between them — or the
one-line question the output settings ask where a disc is being cut:

| | |
|---|---|
| `pgs`, the default | **Inside the cut**, as the kind a transport stream carries. A Blu-ray's own go untouched; a DVD's are converted [below](#a-dvds-subtitles-converted-into-a-blu-rays) |
| `beside` | **Beside the cut**, as the `.idx` and `.sub` pair. A DVD's own go untouched; a Blu-ray's are read back out of their display sets [below](#a-blu-rays-subtitles-written-as-a-dvds) |
| `sup` | **Beside the cut**, as a `.sup` — the display sets themselves, outside any container. A Blu-ray's go byte for byte; a DVD's are converted as they are for the cut [below](#a-sup-the-display-sets-with-nothing-around-them) |

Inside is the default because one file is one file: a cut with its subtitles in
it is a cut everything opens. Beside is what a subtitle tool, an old set-top
box, or an MP4 wants — and an MP4 can only have them beside, whatever is asked
for.

The pair is the **VobSub** one the rest of the world already reads —
[`vobsub.rs`](../../rust/crates/core/src/vobsub.rs):

```text
cut_title.ts     the cut
cut_title.idx    what the subtitles are, and when each one appears
cut_title.sub    the subtitle pictures themselves, untouched
```

Nothing is re-encoded. The `.sub` holds the disc's own units back in the
program stream framing they arrived in, one to a sector, and the `.idx` is a
text file of times and file positions. A player opening the cut finds them by
name.

**The palette is not in the stream.** A unit says "colour 4" and nothing about
what colour four is: a DVD keeps sixteen of them in the index, beside the chain
of cells they belong to. So the cut goes back to the disc for the palette of
the chain the title is cut out of, converts it out of the studio-range luma and
colour differences a DVD writes, and puts it at the top of the `.idx`. Without
that a subtitle is legible only by accident.

**Both ends of a kept range are mended**, as they are for a Blu-ray's graphics
and more simply, because a unit carries its own timing. What was on screen when
the range opens is written again at its first frame; what is still on screen
when it ends is taken down by a unit that says stop and nothing else — ten
bytes, which is what a disc itself sends to clear a plane. That second half is
not optional: of the units on the disc measured here, **not one carries a stop
of its own**. Each subtitle stands until the next replaces it, so a cut that
ended in the middle of one would leave it standing with nothing coming.

#### A DVD's subtitles, converted into a Blu-ray's

**The default, because the cut should be one file.** This takes the picture out
of a DVD's unit and writes it as the kind of subtitle a transport stream does
carry — a Blu-ray's — so that the subtitles are inside the cut and every
reader that opens it finds them. Only a transport stream can hold them: asked
for an MP4, the cut says so and writes the pair beside it instead.

Nothing is resampled and no pixel is lost. **Both formats keep a subtitle in
the same shape**: an index per pixel, run-length coded, and a table of
colours. What changes is how the runs are spelled — four ways in one, four
different ways in the other — and how the colours are written down: a DVD says
red, green and blue and how opaque, a Blu-ray says luma, two colour
differences and how opaque. libavcodec's own decoder reads the picture out
(handed the palette from the disc's index, which is the one thing it cannot
know) and [`pgs.rs`](../../rust/crates/core/src/pgs.rs) writes it back.

What a converted stream needs beyond the pixels is timing, and that is where
the two formats disagree: a DVD's unit says when it appears and, sometimes,
when it goes, while a Blu-ray's display sets have to be told both. So the
composer here holds one subtitle at a time — a new one replaces what was
standing, a unit that said when to go is taken down at its own moment, and
what is still up when a range ends is taken down there. It is the same
mending the two ends of a range get [above](#the-subtitles-a-disc-draws), done
from the other side.

#### A Blu-ray's subtitles, written as a DVD's

The same journey the other way, and for the other reason: a pair beside the cut
is what a subtitle tool, a player that has never heard of a display set, or an
MP4 reads. `--subtitles beside` on a Blu-ray decodes each display set
([`pgs::read`](../../rust/crates/core/src/pgs.rs)) and writes the picture back
out as a DVD's kind of unit ([`vobsub::unit`](../../rust/crates/core/src/vobsub.rs)).

Three things have to be invented on the way, because a DVD's format has them
and a Blu-ray's does not put them where the pair needs them:

**A palette of sixteen.** A DVD's is in the disc's index; a Blu-ray's is in
every display set, a different one each time and as many as 256 colours in it.
So one of sixteen is built as the subtitles go past: each subtitle's colours
are written down under the numbers the `.idx` will call them by, and a colour
near enough to one already written *is* that one — the four colours of a
subtitle are an average of what it was drawn in, and the same white text
averages a shade differently in every line of it. Written down as they came, a
film's white would spend the whole palette on itself. Where sixteen are spoken
for, the nearest stands in for the seventeenth.

**Four colours per subtitle.** A DVD's unit is two bits a pixel and names four
entries of the sixteen: background, pattern, and two emphases — which is
exactly what a subtitle is made of, nothing and the letter and its edge and the
blend between them. A Blu-ray's antialiased text arrives in thirty shades of
that, so the colours actually drawn are clustered into three, weighted by how
much of the picture each covers. **Not the three most used**: the shades of a
letter's interior are used more than its edge is, and picking by count alone
gives three whites and no outline. So the first cluster is the most used, and
each after it is the colour whose distance from what is already chosen,
multiplied by how much it covers, is greatest — then the usual settling, four
passes, which is more than three clusters over a few dozen colours needs.

**How long each one stands.** A display set does not say; what takes a subtitle
down is a later set. A unit *can* say, in the sequence that stops it — so each
one is held back until the set that ends it arrives, and then written with the
delay filled in. Left standing instead, the pair is still right on a player
that follows a DVD's own rule, and is a subtitle of no duration at all to
everything built on libavcodec, which is most things. Waiting one set is what
turns the one into the other.

The two ends of a kept range need nothing new. What reaches this is a display
set either way — carried, put up again at a range's opening, or written to
clear one at its end — so the mending [above](#the-subtitles-a-disc-draws) is
the mending here, and the set that clears a plane at a range's end is what
fills in the last unit's stop.

One limit, and it is the format's: **a unit states its own length in two
bytes**, so 65535 is the whole of what one may be. A line of text is a few
kilobytes run-length coded and never comes near it; a full-screen picture can,
and one that will not fit is counted and said rather than written as something
a player would refuse.

#### A `.sup`: the display sets with nothing around them

The third destination, and the only one that converts a Blu-ray's subtitles
**not at all**. A `.sup` is what a display set looks like outside a container:
every segment as it travelled, each behind ten bytes saying when it is decoded
and when it is shown —
[`pgs::Sup`](../../rust/crates/core/src/pgs.rs):

```text
"PG"  pts  dts  type  length  [ the segment ]
 2     4    4    1      2
```

That is the whole format. So a Blu-ray's subtitles come out of it byte for
byte, and BDSup2Sub, Subtitle Edit and everything else that works on a disc's
subtitles read it without being taught anything. A DVD's go through the same
conversion they would for the cut itself — [above](#a-dvds-subtitles-converted-into-a-blu-rays)
— and land in a `.sup` instead of in a stream, which is what puts a DVD's
subtitles beside an `.mp4` without flattening them to four colours.

What reaches this is the same display set the pair gets and the cut carries, so
the mending at both ends of a kept range is the same mending
([above](#the-subtitles-a-disc-draws)) and nothing here has to know about it.

One difference from the pair, and it is the format's: **a `.sup` holds one
stream**, where a `.idx` names as many as the disc had. A recording with two of
them is written as two files, each taking the language the disc says it is in —
`cut_title.eng.sup` — and the number it sits on where the disc says nothing, or
says the same thing twice.

Two things about the reading are worth writing down, because both cost an
afternoon:

* **A DVD's subtitles may not be in the recording's stream list at all.** They
  share one stream with the sound and are told apart by a byte in front of each
  packet, so libavformat invents a stream the first time it meets one — which
  on the disc measured here was 400 MB in. Probing that far to open a title is
  not affordable. The disc's own index has known all along, so the list comes
  from there and the packets are matched on what they *are* rather than on an
  index settled in advance.
* **Seek before discarding, not after.** The eight seconds in front of a range
  are read with the pictures and the sound thrown away inside libavformat,
  which is what makes the look-back cheap. Ask for that first and the seek is
  then refused outright — a recording is seeked by one of its streams, and one
  that has been thrown away cannot be seeked by — leaving a context that reads
  nothing at all and a range that quietly opened with no subtitle.

And one about the writing: **a stream has to exist before the file's header
is written.** The converted stream is this program's own -- there is nothing
in the recording to copy it from -- and adding it after the header went out
left the muxer with a stream it had never initialised. What that costs is not
an error but a segmentation fault, several frames later, inside a warning the
muxer was trying to print about it.

### A disc that does not say how often pictures arrive

libavformat works the frame rate out while it probes, and stops probing at the
first program map unless it is told otherwise. On a pressed Blu-ray written in
**VC-1** that is too early: `avg_frame_rate` came back unset, and a frame rate
of "not a number" makes nonsense of every duration derived from it — the
length of the recording, the position of a mark, the number of pictures a range
holds.

`scan_all_pmts` reads every map and fixes that. It is not simply switched on
everywhere, for a reason particular to what this program reads: on a Japanese
broadcast, the thorough read also turns up the **second programme** the
transport stream carries — the phone-sized copy of the same material, on its own
pids — which is a different recording from the one that was asked for. So
`input::demux` asks the container first and reopens only where the answer came
back missing (`states_frame_rate`), which is the same rule the `+genpts`
reopening follows.

## What this does not do

| | |
|---|---|
| **Encrypted discs** | Out of scope. Nothing here decrypts AACS. A pressed disc copied by a tool that removed it reads like any other |
| **Writing a disc** | Not supported. That is authoring, and a different problem |
| **Joining clips** | Not supported. A multi-clip playlist is shown as a row per clip (above) |
| **BDMV titles** | `index.bdmv` names titles and a title is a navigation program. Which playlist "T05 Extra 01" plays is not worked out; rows are named by the disc and the clip |
| **A clip whose EP map does not read** | The map is used where there is one — see [the disc's own index](#the-discs-own-index) — and the walk over the packets is what answers where there is not |
| **Blu-ray's own streams** | PGS subtitles are carried into a transport stream, whole display set by whole display set, and mended at both ends of every kept range — [above](#the-subtitles-a-disc-draws) — or written beside the cut where that was asked for, as a VobSub pair ([above](#a-blu-rays-subtitles-written-as-a-dvds)) or as a `.sup` ([above](#a-sup-the-display-sets-with-nothing-around-them)), which is how an MP4 of a disc keeps them. IGS (a menu) and TextST (text subtitles) are dropped and said to be dropped, for the reasons [above](#a-menu-is-not-a-stream-and-the-index-is-the-only-thing-that-knows). The sound is all carried — LPCM, DTS-HD, TrueHD, E-AC-3, [above](#the-sound-a-disc-carries). VC-1 video is read, listed and cut, the partial GOPs written by [SmartCut's own encoder](rust-core.md#vc-1-the-codec-with-no-encoder) |
| **DVD subpictures** | Converted into the kind a transport stream carries and written inside the cut, which is the default; or beside it, untouched, as a VobSub pair; or converted and written beside it as a `.sup` — [above](#a-discs-subtitles-inside-the-cut-or-beside-it). Not copied into the file as they are, because a transport stream has no stream type for them |
| **DVD angles** | A chain whose cells are an angle block or an interleaved unit is not offered, [above](#a-title-is-a-run-of-cells) |
| **Writing a DVD** | A cut of a DVD title is a transport stream. A DVD's own shape is VOBUs of a bounded size, a navigation pack opening each of them and an `.IFO` describing every cell — authoring, and a different problem |
| **Encrypted DVDs** | Out of scope, the same as AACS. Nothing here decrypts CSS |

## What was checked

Real discs, and synthetic ones built out of the ordinary fixtures.

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
through `subfile` and demuxed — H.264 on 0x1011, TrueHD with its AC-3 on
0x1100 and 0x1101, PGS on 0x1200 and 0x1201. A ten second cut of one episode
came out at 91.8% copied, with both TrueHD tracks on their own PIDs and every
sample of them decoding.

**A UHD disc's subtitles, carried through a cut** — 60 seconds out of a 2160p
feature (HEVC, LPCM 5.1, PGS on 0x1200), cut into two kept ranges placed so
that three of the four boundaries fall inside a subtitle. Every display set
inside the ranges came out the other side with its composition numbers
unchanged — 44 packets — and eleven were written that the disc never sent
there: the five of the set standing when the second range opens, put up again
at its first frame, and three each for the two ranges' clears. Read back, the
output declares `hdmv_pgs_subtitle` on 0x1200 both as a `.m2ts` and as a plain
`.ts`; decoded and burnt in, the frame after the seam carries the new range's
line rather than the old one's, the frame at the head of a range that opens
mid-subtitle carries that subtitle, and nothing is left standing at the end of
the file. Written to a BDAV folder instead, the clip index lists the stream as
coding `0x90`; opened by its path on a real disc, the language comes out of
`CLIPINF` (`jpn`) where the map says nothing at all.

Then ten seconds taken straight off the disc — the 81 GB clip itself, opened
through its own entry point map — where the cut is half re-encoded and half
copied. **Every display set in the range came out in order**, including one
that opens 66 ms before the seam between the re-encoded stretch and the copied
one and finishes after it: 39 packets carried and 8 written. That set is the
reason a segment reads on past its own end while a display set is part-read,
and the reason "past the end" is judged by the earlier of the two times a
packet carries — a set is *sent* a fraction of a second before it is *shown*,
so the packet that opens one is the packet whose presentation time is
furthest ahead.

**A DVD's subtitles carried out of an image** — a 4.1 GB `.iso`, one title
set, a Japanese subpicture stream on `0x20` that libavformat does not see
until 400 MB into the recording. Two kept ranges out of the feature came out
with eleven units beside them: nine the disc sent inside the ranges, the one
that was on screen when the second range opened, written again at its first
frame, and the stop that ends the first range. Read back, `ffprobe` finds one
`dvd_subtitle` stream in the pair and both units of the short cut in it;
burnt over the cut, the frame after the seam carries the line the disc had up
at that moment, in the disc's own colours out of `VTS_02_0.IFO`. Asked to
leave them out, the cut writes the `.ts` and nothing else.

**The same subtitles converted** — the same two ranges with `--subtitles pgs`.
Nine subtitles went in as display sets, on `0x20` in a `.ts` and renumbered to
`0x1200` in a `.m2ts` as everything else is; `ffprobe` reads the stream back as
`hdmv_pgs_subtitle` from both, which for the plain `.ts` means the map written
over the muxer's own did its work. Burnt in, the frame after the seam carries
the same line as the pair beside the cut carried, in the same colours.

**A Blu-ray's subtitles written as a pair** — two twenty second ranges out of a
1080p feature (H.264, LPCM, PGS on `0x1200`), placed so that the second range
opens in the middle of a subtitle. Eleven units came out beside the cut: ten
the disc drew inside the ranges and the one standing when the second opens,
written again at its first frame. **Nothing was left over at either end** — the
display set that clears a plane at a range's end fills in the last unit's stop
rather than adding a unit of its own, and the last subtitle of the first range
stops at the instant the range does. Read back, `ffprobe` finds one
`dvd_subtitle` stream at 1920x1080 in the pair, decodes every unit, and every
one of them says how long it stands. The invented palette came to six entries
over the two ranges and thirteen over three minutes of the same feature.

Rendered on a flat ground and compared against the same subtitles carried into
a `.ts` as the disc's own display sets, the two are **identical in colour and
39.7 dB apart on luma** — which is the whole of what flattening a Blu-ray's
antialiasing into a DVD's four colours costs. No pixel moved and no subtitle
changed its moment. Asked for an `.mp4` instead, the same pair is written
beside it, which is the only way a cut of a disc in that container keeps its
subtitles at all.

**A Blu-ray's subtitles written as a `.sup`** — the same two ranges again with
`--subtitles sup`. The 85 segments came out **byte for byte the same** as the
same cut's PGS carried into a `.ts` and remuxed to a `.sup` by ffmpeg — every
segment type and every payload identical — with the times at most **6 ticks of
90 kHz apart**, 67 µs, which is the seconds-as-a-float trip through the cutter's
own clock. `ffprobe` reads the file back as `hdmv_pgs_subtitle` at 1920x1080
starting at the first subtitle of the cut. A disc with two subtitle streams on
`0x1200` and `0x1201`, both saying `eng`, was written as `cut.1201.sup` — the
language cannot tell them apart, so the number they sit on does — and the
stream that had nothing inside the ranges said so instead of leaving an empty
file. Asked of a DVD, the same option converts the units first and writes those
display sets to a `.sup` beside an `.mp4`.

**A menu mistaken for sound** — a UHD disc's clip 00004: pictures, one LPCM
track, and a menu on `0x1400`. libavformat calls that menu **MP3 audio**;
asked about the clip now, the tool lists the one real sound track and says
`not carried: a menu on pid 0x1400`. Its three menu-only clips — graphics and
no pictures — say they are menus instead of failing for want of a video
stream.

The other one was measured by looking: **748 clip indexes across eight discs**.
Twenty-nine of those clips carry a menu; **not one carries a text subtitle
stream** — no `0x92` anywhere. That is the other half of why TextST is named
and left rather than carried: there is nothing here to carry, and nothing to
check a carrier against.

**Two pressed Blu-rays written in VC-1** — the codec most discs of that age carry,
and between them most of the ways an advanced-profile stream can differ: one
1920x1080i at 29.97, coded as interlaced frames, non-uniform quantizer, every GOP
closed; the other 1920x1080 progressive at 23.976, uniform quantizer, every GOP open,
skipped pictures in the run and heavy film grain. Each was read, listed and cut, ten
seconds mid-GOP to mid-GOP: **308/308 and 246/246 pictures, with 90% of the video
byte-identical to the disc** and the partial GOPs at the ends written by SmartCut's
own encoder. Neither container stated a frame rate until every program map was read
([above](#a-disc-that-does-not-say-how-often-pictures-arrive)). What the encoder
costs, and how it is known to be right, is in
[the Rust core](rust-core.md#vc-1-the-codec-with-no-encoder).

**A synthetic disc** — [`tests/run_disc_tests.sh`](../../tests/run_disc_tests.sh)
builds one of each dialect out of the ordinary fixtures: the transport stream
is remuxed into 192 byte packets,
[`tests/disc_index.py`](../../tests/disc_index.py) writes the index files
around it, and genisoimage wraps each in a UDF 1.02 image. Then all four shapes
are asked the same questions, the cut each of them produces is compared **byte
for byte** — against each other and against the plain stream — and the tables
are checked to have gone back in. Thirty-five checks, all passing.

**A real DVD-Video disc** — an NHK documentary mastered by Sonic Scenarist in
2009: 4.1 GB, UDF 1.02 over ISO 9660, one title set in four VOB files, MPEG-2
720x480 with AC-3. The two titles and their nineteen and eighteen chapters read
out of the `.IFO` tables and agreed with an independently written Python reader;
the byte range came out at 24,727,552–4,289,400,832, which is the four VOB
files exactly; the discontinuity before the last cell was found and the rows cut
at it; `VOBU_S_PTM` came out as 0.280633 s, which is what libavformat reports
for the container's start to the microsecond. A 480 second cut of two ranges
came out **14,386 frames, 99.8% copied**, with the copied frames matching the
source's **md5 for md5, consecutively**, and video and audio within 13 ms of
each other. The same title read out of a `VIDEO_TS` folder, cut across the seam
between two VOB files, came out at 1798 frames for 60 seconds.

**A synthetic DVD** — [`tests/run_dvd_tests.sh`](../../tests/run_dvd_tests.sh) builds
two out of the ordinary fixtures: an ordinary disc, whose title set's stream is written
in two files, and one whose title was multiplexed in two halves so that its clock starts
again in the middle. [`tests/dvd_index.py`](../../tests/dvd_index.py) fills in the
navigation packs and writes the `.IFO` tables. Then the rows, the lengths, the chapters,
the tracks and the sector ranges are checked against what was written, the awkward disc
is checked to have come out as two rows on two clocks, and the cut is checked to be the
same cut **md5 for md5** whether it was taken from the folder, from the image, or from
the program stream with no disc around it at all. Twenty-three checks, all passing.

**The sound, codec by codec** —
[`tests/run_bd_audio_tests.sh`](../../tests/run_bd_audio_tests.sh) builds a
clip of each: LPCM at 16 and at 24 bits, DTS, TrueHD, E-AC-3. Each is cut into
a `.ts`, an `.m2ts` and an `.mp4`, and what is checked is the claim that codec
makes — that the stream types the disc used come back out, that LPCM into an
MP4 is the same samples with **nothing differing at all**, that the frame an
LPCM boundary lands inside comes out with its far side silenced and its near
side intact, and that the audio of a DTS or TrueHD cut is a stretch of the
recording's own bytes, found in it by search. Thirty-nine checks, all
passing.
