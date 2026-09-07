# Writing a disc

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](bdav.ja.md)

**The other direction from [reading a disc](disc.md).** A night's cuts can
leave here as a folder of `cut_*.ts`, or as a disc: a `BDAV` directory a
recorder or a player opens as a list of programmes, each with the name it went
out under, the channel it came off, the night it was recorded, what the
broadcaster said it was about, and a chapter point at every place a commercial
break was taken out.

```
BDAV/
  info.bdav              which playlists there are, and what the disc is called
  PLAYLIST/00001.rpls    one recording: which clip, from when to when, and its name
  CLIPINF/00001.clpi     that clip's own index: what it carries, and every entry point in it
  STREAM/00001.m2ts      the transport stream itself
```

What comes out is a folder, not an image. Burning it, or wrapping it in a UDF
image, is what ImgBurn and `genisoimage` are for, and a disc that has to be
built before it can be looked at is a disc nobody checks before they burn it.

## The stream is written by the cut

Nothing here writes video. Asked for a `.m2ts`, libavformat writes Blu-ray's
own framing — the same 188 byte packets with four bytes of arrival time in
front of each — and the recording's own tables go back into it exactly as they
go into a `.ts` ([`si.rs`](../../rust/crates/core/src/si.rs), and
[Broadcast TS](../technical/broadcast-ts.md)). So a recording written onto a
disc is the same smart-rendered cut as one written into a file: over 99% of it
copied byte for byte, with the same handful of re-encoded frames at each seam.

Three things are different, and each of them is a thing a *disc* needs.

### The PIDs are Blu-ray's

A cut into a `.ts` puts every stream back on the PID it arrived on. The tools
downstream of a broadcast recording look for the sound and the captions where
the broadcast put them, and a fresh numbering makes the output look like
something else entirely.

A `.m2ts` cannot do that. Its programme map is on PID 0x0100 whatever else is
on it, and a Japanese broadcast puts its pictures there often enough for the
two to meet — at which point the muxer stops the cut outright:

```
[mpegts] PID 256 cannot be both elementary and PMT PID
```

So the streams of a `.m2ts` are numbered the way a Blu-ray numbers them:
pictures on 0x1011, sound from 0x1100, everything else from 0x1200. The
recording's own tables still describe each of them — the map that goes into
the file is the broadcast's, with each stream named by where it went rather
than by where it came from — so a Japanese player still finds AAC declared as
AAC, the captions declared as a data stream with the component tag they
arrived with, and the programme information on PID 0x1F where a partial
transport stream keeps it.

### The arrival times are written again

Every packet of a Blu-ray carries the moment it arrived, in the 27 MHz the
clock counts in, and a player uses those to feed its decoder at the rate the
recording was made at.

libavformat writes that field, and asked for a stream whose rate it was not
told it writes **nonsense** in it — a counter that steps *backwards* by
194,189,274 ticks every packet:

```
228516354, 34327080, 913879629, 719690355, 525501081, …
```

Told a mux rate it writes a clean one, at the cost of padding the stream with
null packets up to that rate, which on a 12 Mbit/s recording is half the disc
spent on nothing.

So [`bdav::stamp`](../../rust/crates/core/src/bdav.rs) writes them again from
the stream's own clock, which is where the answer already is. A packet between
two programme clock references arrived between them, in proportion to how far
through it sits; the packets in front of the first are spaced at the rate a
disc is written at, and the ones after the last at the rate the file has
averaged so far. That is the same model a recorder uses, and it is why a real
disc's arrival times step evenly for a burst and then jump: the jump is where
the recorder had nothing to write. Measured on a disc a Japanese authoring
tool wrote, the step is 1571 ticks for 94% of the packets and a larger number
for the rest.

The last interval is the one worth being careful about. It is often a handful
of packets the muxer flushed together, whose spacing is nothing like the
recording's — extrapolating the tail from it put four seconds of arrival time
onto a 167 second recording before the average took over.

### The index is read back off the file

Every number in `CLIPINF` and `PLAYLIST` is read off the stream that was
written rather than carried forward from the recording it was cut from. A cut
is not its source: it is shorter, its timestamps start elsewhere, and where
its pictures sit in the file is the muxer's business. So the pass opens the
finished `.m2ts` and asks it — which is the same walk over every packet that
the editor makes when a recording is opened, and costs about a second a
gigabyte.

## What a clip index says

`00001.clpi` is five sections behind a header of addresses. Three carry
something:

| | |
|---|---|
| **ClipInfo** | that this is a transport stream of a recording, the rate it is written at, and how many source packets it holds |
| **SequenceInfo** | which PID carries the clock, and the first and last moment a picture is shown |
| **ProgramInfo** | which PID the map is on, and what each stream is: the coding, and a shape and rate for pictures, a channel arrangement and rate and language for sound |
| ClipMark | empty: on a disc of recordings the chapter points belong to the playlist |
| MakersPrivateData | empty by definition |

The coding of each stream is taken from the map the file itself carries rather
than from the codec libavformat named, so the index and the map cannot
disagree — which is the failure that would have a player hand AAC frames to a
decoder that was told they were private data.

### The entry point map

This is what makes a disc seekable. Without it a player has to read forward
looking for a picture it can decode, which on a two hour recording is the
difference between a chapter skip landing at once and landing eventually.

Each entry point is written twice over, coarsely and finely, because
thirty-two bits per entry is not enough for both a time and a position:

```
coarse:  18 bits  which fine entry this is about
         14 bits  the time, above bit 19
         32 bits  the source packet
fine:     1 bit   an angle change point, which a recording has none of
          3 bits  how far past the entry its picture ends
         11 bits  the time, bits 9 to 20
         17 bits  the source packet, low bits
```

A player reconstructs one by taking the coarse entry's high bits and the fine
entry's low bits, so a coarse entry is written afresh whenever the high bits
change — every twelve seconds of time or every 131,072 packets of position,
whichever comes first. On a half-hour recording that is about 3,600 fine
entries and 300 coarse ones.

The times are the 45 kHz a playlist counts in, which is the presentation clock
halved, and the positions come from the byte offset the packet scan recorded
for each access point, divided by 192. `tests/bdav_index.py` follows every one
of them into the stream and checks that the packet it names begins a picture
at the time it claims.

## What the playlist says

`00001.rpls` is one PlayItem — one clip, from its first picture to its last —
with everything a recorder knows about the recording in front of it and its
chapter points behind it.

**What a recorder writes down, and where.** The description a playlist opens
with is a fixed 1502 bytes on both of the discs this was written against, and
the four things in it are at the same offsets on both:

```text
 50  when it was recorded    7 bytes, binary coded decimal, century first
 64  the channel's number    2 bytes  -- the three digits a viewer knows
 67  the channel's name      1 + 20   -- ARIB text
 88  the programme's name    1 + 255  -- ARIB text
344  what it was about       2 + …    -- ARIB text, lines and all
```

The fields fit each other exactly: 68 + 20 is 88, and 89 + 255 is 344, which
is how the layout can be read off two discs rather than guessed at. The
description is the only one counted in two bytes, because it runs to hundreds
of them — the recorder's disc read here carries between 471 and 596 bytes of
it per recording, and there is room to 1546 for more.

A recorder fills all four in. The authoring tool's disc writes the name and
the date and leaves the channel and the description empty, which is what it
knows: it was handed a file, not a broadcast.

**The name is ARIB text.** Not UTF-8: a recorder writes the eight-unit code of
ARIB STD-B24, and a name written there as UTF-8 is a name a recorder draws as
mojibake. [`arib.rs`](../../rust/crates/core/src/arib.rs) reads that code for
the disc reader and now writes it as well — which is a smaller thing than
reading it, because a writer only has to be read correctly. Two graphic sets
are enough for a name, the alphanumerics and the kanji set that holds the kana
too, and both are already invoked when the text begins; so the whole of the
state is a locking shift between them, and no escape sequence appears in the
output at all. The half width of the alphanumerics and the normal width of the
kanji go in as a recorder writes them, because that is what makes a name read
on a television the way it read on air.

The field is a length byte and 255 bytes at most, which a Japanese title
reaches at about 85 characters. What does not fit is cut at a character, never
inside one: half a JIS pair is a different character rather than a shorter
name.

**The chapter points are the cuts.** One at the start of every kept range,
which is where the commercial breaks were, plus whatever marks were put down
in the editor. That is the one thing on a recorder's disc a viewer uses every
time.

## Where all of that comes from

This is the reason the feature exists. A stream says nothing about what
programme is in it; the disc's index is where that lives, and it has to be
*filled in* from somewhere. Three somewheres, in this order:

| | |
|---|---|
| **What was typed** | the output settings screen offers the programme's name per clip and the disc's name once, because a name nobody can correct is a name that is wrong forever |
| **The disc it came off** | a recording read off a BDAV disc arrives with everything its playlist said: the name, the moment it was recorded, the channel and its number, and what the broadcaster said the programme was — [`disc::Entry`](../../rust/crates/core/src/disc.rs) carries all of it |
| **The broadcast itself** | [`si::programme`](../../rust/crates/core/src/si.rs) reads the recording's own tables: the short event descriptor for the name and the sentence under it, the extended event descriptors for the cast and the staff, the event's start time for the moment, and the service description for the channel |

Each field is taken on its own rather than as a set, so a disc that named the
programme and not the channel still takes the channel from the stream.

The third source has two shapes to read, because this program writes one of
them. A broadcast recording carries an event information table and a service
description on their own PIDs; a cut this program has already made carries a
partial transport stream's single table instead, with the same facts in it —
and a cut of a cut should not lose what it was carrying.

**The description is assembled the way a recorder assembles it.** The short
event descriptor holds one sentence; the extended event descriptors hold a
list of named items — the cast, the staff, the episode's own summary — spread
over as many descriptors as they need, with an item continued into the next
one carrying an empty name. What goes onto the disc is the sentence, and then
`【item】text` a line at a time, which is what the recorder's own disc has in
that field.

**The channel's number is the three digits a viewer knows it by**, and it is
the service's own identifier on satellite: a satellite service numbered 161 is
channel 161, and every BS and CS service is numbered in that range. A terrestrial
service is not — its identifier is 1024 and up, and the three digits are built
from a key number that only the network information table carries — so a
terrestrial recording writes 0, which is the field saying it does not know
rather than a number that would be wrong. The name goes in either way.

## What is copied rather than understood

Three places still carry fields whose meaning is not written down anywhere
this program can reach: the six bytes in front of the date in a playlist, four
in `info.bdav`, and the four bytes each chapter mark opens with.

**Both discs have the same values in all of them** — a Japanese recorder's,
written in 2010, and an authoring tool's, written in 2026 — so what is written
here is those values, and it is marked as such in the code. Copying a constant
that two unrelated tools agree on and a player has demonstrably accepted is
worth more than writing a zero into a field whose name is unknown. Everything
else — the addresses, the lengths, the times, the counts, the PIDs, the names,
the description — is this program's own and is written from what the stream
actually contains.

The same principle put the offsets in the reader: they are the ones read off
discs real tools wrote, rather than a specification this project does not
have. Two discs are what turned the middle of a playlist from a run of
unknown bytes into the table above: the fields the authoring tool left empty
are the ones the recorder fills in.

## Adding to a disc

A run writes onto the disc rather than over it. The numbering continues from
the highest `00NNN.m2ts` already in `STREAM`, the table in `info.bdav` is
written from every playlist in `PLAYLIST` rather than only from the ones just
made, and nothing this writes ever removes a recording somebody else put
there. An evening's second batch belongs beside the first, which is how a
recorder behaves and what makes a disc worth filling over a week.

The disc's own name is the one exception: it is one name for the disc, so the
one on the screen wins.

## What was checked

**Against two real discs.** The first is one a Japanese authoring tool wrote
from broadcast recordings in 2026: three programmes, 1.2 GB each, `info.bdav`
at 356 bytes and each `.clpi` at about 5 kB. Every structure here was laid
beside its bytes — the addresses, the section lengths, the 22 byte sequence,
the 26 byte program info, the 90 coarse and 1019 fine entry points of an eight
and a half minute recording, the 46 byte chapter marks, the ARIB text of the
name and of the disc's own title — and the arrival times were measured off its
stream, which is where the 1571 tick step and the `TS_recording_rate` of
3,231,064 come from.

The second is a Japanese recorder's own, written in 2010: six half-hour
recordings off one satellite channel, 3.6 GB each, with a 14 kB `.clpi` apiece
and six chapter marks per playlist. It is what settled the middle of the
playlist. Where the authoring tool wrote zeros it writes the channel's number
and name and several hundred bytes of what the programme was about, and the
two agree byte for byte on every field either of them fills in — which is what
makes the table above a layout rather than a guess. Both discs open in this
program's own reader, and the marks, the tracks and the names come back off
both.

**Round trip.** [`tests/run_bdav_tests.sh`](../../tests/run_bdav_tests.sh)
writes a disc of two recordings, opens it with this program's own reader —
the same one a recorder's disc goes through — and then checks every number in
the index against the stream it is about with
[`tests/bdav_index.py`](../../tests/bdav_index.py), which stands in for the
player: that the clip index agrees with the file on the packet count, the
times, the clock PID and the streams; that **every entry point lands on a
picture at the time it claims**; that the arrival times never step backwards
and span the recording; and that the programme's name, the channel and its
number, the moment it went out and what the broadcaster said it was about all
survive being written into a disc, read back out of it, and written into a
second one — the last two compared as the bytes that went in. Forty checks, all passing.
