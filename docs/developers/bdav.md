# Reading a BDAV disc

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](bdav.ja.md)

**A recorded Blu-ray opens as a folder or as an `.iso`, either way.** What the
list shows is the name of the programme, not `00001.m2ts`.

```
$ smartcut Anime_Test.iso
disc  : Anime_Test.iso
        3 recording(s)

  1  00:08:30.010  2026年08月17日01時00分-BS11イレブン-アズールレーン びそくぜんしんっ!にっ!! #07「神様的休息日の過ごし方。」  2 mark(s)
  2  00:08:30.010  2026年08月24日01時00分-BS11イレブン-アズールレーン びそくぜんしんっ!にっ!! #08「私の罪をお赦しください・・・」  2 mark(s)
  3  00:08:29.976  2026年08月31日01時00分-BS11イレブン-アズールレーン びそくぜんしんっ!にっ!! #09「わたしのラッキーアイテム?」  2 mark(s)

name one with --title N to open it
```

In the window, drop an `.iso` or a disc folder and its recordings arrive as one
row each. A folder holding several of them works the same way, so an evening's
worth of discs becomes a list.

## What BDAV is

Not BDMV. BDMV is the format of a film you buy: menus, a `BDMV` directory, an
`index.bdmv`. BDAV is the **recording** format -- what a set-top recorder
writes, and what an authoring tool produces when it is asked for a disc of
recordings.

```
BDAV/
  info.bdav              which playlists there are, and in what order
  PLAYLIST/00001.rpls    one recording: which clip, from when to when
  CLIPINF/00001.clpi     that clip's own index
  STREAM/00001.m2ts      the transport stream itself
```

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

A `.rpls` does not carry the name in UTF-8. It is written in the ARIB STD-B24
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

## What a row is

**One playlist, one row**, in the order `info.bdav` puts them -- which is the
order a recorder's own list would show. The name, the time it was recorded, the
IN and OUT, and the chapter marks all come out of the `.rpls`.

A playlist that plays more than one clip -- what a recorder writes when a
programme ran past the length it splits its streams at -- becomes a row per
clip, named `(1/3)`, `(2/3)`, `(3/3)`. Joining them into one timeline is a
different piece of work: each clip carries its own clock, and splicing two of
them is not the same operation as cutting one. Until that exists, showing the
pieces is the honest thing -- every second of the recording is reachable, and
the list says plainly that it is in pieces.

Chapter marks are read **only when they can be believed**. The size of a mark
is given away by the section's own length, but where the timestamp sits inside
one is not the same on every disc: BDMV's mark is fourteen bytes with the time
four in, and the marks a BDAV recorder writes are longer and carry a name and a
thumbnail reference beside the time. So each candidate offset is tried and the
one whose times **all land inside the title** is the one that is used. When
none of them does, the marks are left out: a chapter point in the wrong place
is worse than no chapter point.

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
| **Encrypted discs** | Out of scope. Nothing here decrypts AACS |
| **Writing BDAV** | Not supported. That is authoring, and a different problem |
| **Joining clips** | Not supported. A multi-clip playlist is shown as a row per clip (above) |
| **The CLPI EP map** | Not used. The index is still built by scanning packets. Adding it to `IndexSource` in [`index.rs`](../../rust/crates/core/src/index.rs) would save that pass |
| **BDMV (commercial discs)** | Not supported. This ffmpeg is built `--enable-libbluray`, so the `bluray:` protocol is reachable, but `.rpls` is outside what libbluray reads |
| **Blu-ray's own streams** | LPCM (`pcm_bluray`) survives neither MP4 nor MKV and degrades to `bin_data` in TS; PGS subtitles, TrueHD, DTS-HD and VC-1 are untested |

## What was checked

Both a real disc and a synthetic one.

**A real disc** — BDAV written by TMSR6, made into a UDF 2.60 image by ImgBurn:
3.4 GB, three clips, 1920x1080 MPEG-2 with AAC and ARIB captions, a partial
transport stream. All three programme names read correctly, the byte ranges
agreed with an independently written Python UDF reader, a 60 second cut came out
99.2% untouched, and the broadcast's own tables came through (PIDs 0x1100,
0x1101, 0x1102 and the SIT on 0x1F). **Copying the same clip out to a folder and
cutting the same range produced a file identical down to the md5.**

**A synthetic disc** — [`tests/run_bdav_tests.sh`](../../tests/run_bdav_tests.sh)
builds a small one out of the ordinary fixtures: the transport stream is
remuxed into 192 byte packets,
[`tests/bdav_disc.py`](../../tests/bdav_disc.py) writes the index files around
it, and genisoimage wraps the lot in a UDF 1.02 image. Then the folder and the
image are asked the same questions, the cut each of them produces is compared
**byte for byte**, and the tables are checked to have gone back in. Eleven
checks, all passing.
