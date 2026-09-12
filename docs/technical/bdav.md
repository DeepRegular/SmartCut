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
[Broadcast TS](broadcast-ts.md)). So a recording written onto a
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

### The arrival times are written again, at a rate

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

So [`bdav::stamp`](../../rust/crates/core/src/bdav.rs) writes them again. **A
recording is delivered at a constant rate**, and where there was nothing to
deliver the recorder wrote nothing and the times jump. That is exactly the
shape a real disc's arrival times have: on both reference discs every step is
either the rate's own step — 1571 ticks on one, 1593 on the other — or a
jump, and never anything between. Both discs declare a `TS_recording_rate`
that is that step read back as a rate; 27 MHz over 1571 ticks, times a packet,
is 3,231,064 bytes a second, which is the number on the first of them.

**A recorder need not declare the step it used.** The BD-REs a recorder wrote
in 2026, measured here later, step every 2031 ticks and declare 3,525,000
bytes a second -- which is the step 1440, a rate two fifths faster than
anything on the disc actually arrives at. Nothing on them arrives faster than
the declared rate allows, and that is the only thing the field can be read as
promising: a ceiling. The two discs above declare theirs exactly at the step
because the tool that wrote them chose to. Working backwards from
`TS_recording_rate` to the step is right on those two and wrong on a
recorder's own.

The clock references say when the packets carrying them arrive, and they are
what the schedule is pinned to. Working backwards from the end, a packet
arrives one step before the packet after it, and a packet carrying a clock
reference arrives at the moment its clock says — unless the burst that
follows it will not fit at the rate, in which case it has to begin earlier and
the reference moves earlier with it. The gaps then fall where the stream had
nothing to send.

**Where in a run the gaps fall is a choice, and the recording does not say.**
All that is known is the clock references; between two of them there is room
the packets in between do not need. Packing each of them as late as it will go
puts all of that room in one lump at the front of the run, which measured on a
recording written that way is a stream that runs for 22 milliseconds and then
stops for 18. A recorder stops far more often and for far less: 38 packets and
then 72 steps of nothing on one reference disc, 76 and 71 on the other, which
is a stop every 6.5 and 8.6 milliseconds. So the room is dealt out through the
run instead, in stretches of about four milliseconds — which on the same
recording comes to a stop every 7.3, of 36 steps, with every gap still a whole
number of steps and no packet arriving sooner than one.

**A moved reference is rewritten.** A clock reference *is* the arrival time of
the byte carrying it; a packet delivered at a different moment from the one
its reference claims would have a player's clock stepping about by the
difference every time one arrived. So the value goes back into the stream as
the schedule's own, and the two agree by construction.

What that costs is that the data sits in the decoder's buffer for as long as
the reference moved, so the rate is chosen to keep the move small: the
slowest rate that moves no reference by more than a fifth of a second, found
by searching between 25.8 and 48 Mbit/s, and the fastest where no rate does.

**A fifth of a second, because a tenth does not reach the rate a disc is
written at.** At a tenth, six recordings off one broadcast came out between 32
and 35 Mbit/s — no reference disc's rate, and a different one on each
recording, since each is searched for on its own. Solving two of those same
streams again at 25.8 Mbit/s costs 0.079 and 0.070 seconds of further
movement, all of it inside a fifth; and at a fifth every one of them settles
on 25.8 Mbit/s, which is one rate for the whole disc and the same
3,231,064 the reference disc carries. The lead it is paid for with is 0.66
seconds, against the 1.12 the reference disc asks of the same buffer.

Interpolating between the clock references instead — which is what this did
before, and looks reasonable — puts the *muxer's* bursts into the arrival
times: libavformat writes a clock reference every twenty to thirty
milliseconds and whatever the picture needed in between, so a 258 kB frame
came out claiming to arrive at 137 Mbit/s. Measured on a 24 minute recording,
**59% of the packets arrived faster than the rate the index declared**, which
is the one mistake in this file that matters. On the same recording written
the way above, none of them do and the decoder's lead sits between 0.42 and
0.66 seconds from one end to the other.

### The stream is padded to whole aligned units

A Blu-ray reads and writes a stream 32 source packets — 6144 bytes — at a
time, and every stream file on both reference discs is a whole number of them,
each one ending in the null packets that made it so. What libavformat leaves
is whatever its last flush came to: of six streams measured, none were a whole
number and the shortfall ran from 1920 to 3456 bytes. So the tail is padded
with null packets, arriving at the rate everything else did.

The image is rounded out the same way, to a whole 64 kB cluster: both
reference images come to one exactly.

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
| **ClipInfo** | that this is a transport stream of a recording, the rate it is written at — read off the schedule that wrote it, so the index and the stream cannot disagree — and how many source packets it holds |
| **SequenceInfo** | which PID carries the clock, the packet its first reference is in, and the first and last moment a picture is shown |
| **ProgramInfo** | which PID the map is on, and what each stream is: the coding, and a shape and rate for pictures, a channel arrangement and rate and language for sound, a language for [the subtitles a disc draws](disc.md#the-subtitles-a-disc-draws) |
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
whichever comes first. On a half-hour recording that is about 3,100 fine
entries and 200 coarse ones.

**Twelve seconds and not six.** The coarse time is fourteen bits above bit 19,
and a player puts the two halves back together with the lowest of those
fourteen *dropped* — the fine entry's eleven bits cover bit 19 as well. So a
coarse entry is only worth writing when bit 20 changes. Writing one when bit
19 did, as this used to, wrote twice as many as a disc needs: 369 against the
reference disc's 186 over the same length. Nothing read them wrongly; the map
was a kilobyte fatter than it had to be.

**Where the picture ends.** The three bits between the angle-change flag and
the time say how far past an entry the picture it names ends, which is what a
player reads to fetch one picture and no more — how a fast forward is done.
Both reference discs fill it in on every entry; this used to write zero, which
says the picture is shorter than the field can express. What the units are is
not written down anywhere this project can reach, so they were measured — and
the steps turn out **not to be even**. The first three are 682 source packets
apart, which is close enough to 128 kB of stream that an even 128 kB reads
right all the way to the third; above that they stretch, to four and a half of
that step, to seven, to ten. Measured against the whole of one disc: each of
its 1019 entries was read back out of the stream — from the entry to the first
packet of the picture after it, which is where the picture it names ends — and
every one of the 1019 falls in the bucket the disc's own field names. An even
128 kB carried upwards, which is what this wrote before 0.5.13, puts 75 of
them a bucket too high, all of them pictures over 393 kB, which on an
interlaced broadcast is an ordinary size for one. The two largest steps are
the only ones no disc here exercises; they are
[`sorshi/bdav`](https://github.com/sorshi/bdav)'s, which arrived at the same
table from the other direction, against a different tool's output.

**The times here are not the playlist's tick.** An entry point map carries
`PTS_EP_start`, which is the picture's own presentation time stamp —
thirty-three bits at 90 kHz, the number the stream itself carries — and it is
the one place in a disc's index that counts in that clock rather than in the
playlist's half of it. Written in the playlist's 45 kHz, as this did before
0.5.4, every entry sat at half the time it belonged to: a player seeking into
one of these discs landed twice as far in as it was asked, and this program
reading its own disc back planned a cut against times half the truth, which on
a short recording put a boundary between two pictures and left the segment with
none. `tests/bdav_index.py` had checked the entries against the same mistake
and so agreed with it; it reads the stamp as the stamp now, and follows every
one of them into the stream to check that the packet it names begins a picture
at the time it claims.

The positions come from the byte offset the packet scan recorded for each
access point, divided by 192.

## What the playlist says

`00001.rpls` is one PlayItem — one clip, from its first picture to its last —
with everything a recorder knows about the recording in front of it and its
chapter points behind it.

**What a recorder writes down, and where.** The description a playlist opens
with is a fixed 1502 bytes on both of the discs this was written against, and
the four things in it are at the same offsets on both:

```text
 50  when it was recorded    7 bytes, binary coded decimal, century first
 57  how long it ran         3 bytes, binary coded decimal, hours first
 64  the channel's number    2 bytes  -- the three digits a viewer knows
 67  the channel's name      1 + 20   -- ARIB text
 88  the programme's name    1 + 255  -- ARIB text
344  what it was about       2 + …    -- ARIB text, lines and all
```

The fields fit each other exactly: 68 + 20 is 88, and 89 + 255 is 344, which
is how the layout can be read off two discs rather than guessed at.

**How long it ran is the slot and not the cut.** One reference disc says half
an hour against twenty-four minute recordings, the other a quarter of an hour
against twelve and a half minute ones — and thirty minutes against the two
half-hour programmes at the end of it. That is the listing's own length, which
is the field a broadcast's event carries beside the moment it began, in the
same three bytes of binary coded decimal; a disc that wrote one and not the
other would be throwing half a descriptor away. Nothing types it: it comes
from the recording, or from the playlist of the disc the recording came off. The
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

Which is a thing somebody typing a name should be able to see coming, so the
screen counts the room left in each of the three text fields as they are typed
-- in bytes of this code, since that is what the field is measured in, and not
in characters. The channel's twenty are the tightest of them: a name like
`NHK総合・東京` is seventeen.

**The chapter points are the cuts.** One at the start of every kept range,
which is where the commercial breaks were, plus whatever marks were put down
in the editor. That is the one thing on a recorder's disc a viewer uses every
time.

## The lead a disc gives a decoder

A stream cannot show its first picture the moment its clock starts: the
buffer that picture comes out of has to be filled first, and the lead is how
long the stream waits. Both reference discs wait 0.44 and 0.46 seconds. Left
to itself, the mpegts muxer waits for the reorder delay and nothing more,
which on one recording was 0.05 seconds — not time enough to fill anything.

The muxer adds its `max_delay` to every presentation time and takes it off
again for the clock reference it writes, so that setting *is* the lead and
nothing else. It is set to 0.4 seconds, which comes out at 0.48. Only on a
disc's own stream: a `.ts` cut is meant to be the recording it came from, and
this would be a shift the recording does not have.

## Where all of that comes from

This is the reason the feature exists. A stream says nothing about what
programme is in it; the disc's index is where that lives, and it has to be
*filled in* from somewhere. Three somewheres, in this order:

| | |
|---|---|
| **What was typed** | the output settings screen offers the disc's name once and, per clip, all four of the things a playlist says about a recording: the programme's name, the channel and its number, the moment it went out and what it was about. A field nobody can correct is a field that is wrong forever, and a recording that has been through tools that kept none of this has nowhere else to get it from. Each field shows what will be written, whichever of the three answers below it came from; emptied, it is written empty -- which is what the authoring tool's disc does with the channel and the description -- except the name, which fills itself back in |
| **The disc it came off** | a recording read off a BDAV disc arrives with everything its playlist said: the name, the moment it was recorded, the channel and its number, and what the broadcaster said the programme was — [`disc::Entry`](../../rust/crates/core/src/disc.rs) carries all of it |
| **The broadcast itself** | [`si::programme`](../../rust/crates/core/src/si.rs) reads the recording's own tables: the short event descriptor for the name and the sentence under it, the extended event descriptors for the cast and the staff, the event's start time for the moment, and the service description for the channel |

Each field is taken on its own rather than as a set, so a disc that named the
programme and not the channel still takes the channel from the stream.

**The disc's own name is counted, like every other name.** A length byte in
`info.bdav` at offset 64 and then that many bytes of ARIB — the same shape the
playlist gives the programme's name. Written without the byte, as this did
before, a recorder reads the first byte of the text as the length, and the
first byte of a name that begins in kanji is a shift — so a disc called
`星降る夜の郵便局` goes out as eighteen bytes behind a shift of 0x0F, is read as
fifteen bytes of name, and comes up in a list as `星降る夜の郵便`.

The reader had the same byte off in the other direction: a recorder writes the
name straight in kanji, so the length in front of it was decoded as the first
half of a character and every pair after it split across two. The same name
read back as `〓厩澆詭襪陵絞惷`, and so did every real disc's.

Both halves are fixed, and the two discs below are what say so: the length
byte is exactly the number of bytes that follow it on each.

**The disc's own name is not one of the fields.** Nothing carries it: a
recording knows what programme it is, and a disc of six of them is a thing
nobody has named yet. It used to be filled in with the channel the first
recording came off, which is right for an evening scraped off one channel and
wrong for the disc most people make -- six weeks of one series, labelled in a
recorder's list with the name of a transponder. So it is now read out of the
programmes instead: [`series::of`](../../rust/crates/core/src/series.rs) takes
the episode number, the episode's own title and the broadcast's marks off one
name, and `series::shared` answers where a run of them all say the same thing --
`星降る夜の郵便局`, out of six recordings that each called themselves that and
then said which episode they were -- and answers with nothing where they do
not. Several programmes have no shared name to find, and neither do two
seasons of one, because the season is part of the name; the screen then falls
back on the moment the disc is being made, `2026-09-11 00:15`, which is a true
thing to say about a disc when there is nothing else true to say. Cutting a
mixture back to the letters its titles happen to share was tried first and
taken out again: a disc of six programmes labelled with a name most of them do
not have reads, in a recorder's list, exactly like a disc of six episodes of
that one. Either way it is a filled-in field like the others, and typing over
it is what settles it.

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

What a run adds it also takes back when it does not finish. A slot is reserved
before the cut that fills it, and the index is written once at the end over
what the run finished; stop the run and that pass does not happen, so what is
left is a stream no playlist names, one the next run's numbering walks past
and any image made of the disc afterwards carries. Those streams -- and the
stream of a recording whose cut failed while the rest of the list went on --
are removed at the end of the run. Only the numbers the run itself was given
are ever named, so nothing another run put on the disc is touched.

## The image

A folder is what a recorder writes to a disc and what an authoring tool will
burn, but it is not what most people hand to a burner. So the disc can be
wrapped in an image as it is finished: `disc/` becomes `disc.iso`, and the
folder stays -- the image is made *of* it, and deleting somebody's disc
because they asked for an image of it is not this program's decision to make
on its own.

Asked for by the run, though, it is: the checkbox under the image setting, or
`--iso-only` on the command line, takes the folder away once the image holds
all of it. Always in that order -- the folder is written, the image is made of
it, and only then does the folder go, so an image that failed leaves the disc
exactly where it is. What goes is `BDAV`, and the folder above it only where
that leaves it empty (`bdav::remove_disc`): a disc written straight into a
folder of somebody's own is one thing in it, and the rest is not ours to take.

The image is a **UDF** filesystem, at revision 2.50 or 2.60 as the output
settings screen asks. [`udfw.rs`](../../rust/crates/core/src/udfw.rs) writes
it, and there is no specification in this project to write it from: every
offset, every constant and every field was read off the two images the disc
work was done against, and what comes out is read back by
[`udf.rs`](../../rust/crates/core/src/udf.rs), which is the reader those two
images are already opened with.

```text
  0..15   nothing: the system area an ISO9660 volume would use
 16..18   BEA01, NSR03, TEA01 -- "there is a UDF volume in here"
 32..47   the volume descriptors: primary, implementation use, partition,
          logical volume, unallocated space, terminating
 64..65   the logical volume integrity descriptor, and a terminator
    256   the anchor, which is the one descriptor at a fixed place
    288   the partition, and inside it the metadata partition and the files
   then   a reserve copy of the volume descriptors, and a second anchor in
          the last sector
```

**The file entries live in a metadata partition**, which is what UDF 2.50
added and what both reference images use: a partition whose blocks are the
contents of an ordinary file on the partition beside it, so that the
directory tree is in one place on the disc and the file data stays where the
burner laid it down. It is also what decides how an entry names its data. A
short allocation descriptor means "the partition this descriptor is recorded
on", so a directory -- whose contents are in the metadata partition too --
uses short ones, and a file uses long ones and names the partition its bytes
are actually on.

Three details are worth writing down, because getting any of them wrong
produces an image this program reads and everything else refuses -- or one
that reads until the day a cluster of it goes bad:

* **Every descriptor records the block it is written at.** A reader that
  finds one claiming to be somewhere else is right to stop, and other readers
  do. The reader here never looked, which is exactly why the first images it
  happily opened were rejected by everything else.
* **A file identifier carries the number of the file it names**, in the six
  bytes a long allocation descriptor keeps for the implementation, and the
  file's own entry has to agree. Both reference images do this. The check in
  `udfw.rs` is that the identifier this writes for a directory called `BDAV`
  comes out **byte for byte identical** to the one on the reference image,
  checksum and cyclic redundancy check included.
* **The metadata partition's second copy is described from beside itself.**
  The copy goes at the end of the partition, and the entry that describes it
  used to be written at block 1, beside the original's at block 0 -- so one
  unreadable 64 KB cluster took the directory tree and the spare copy of it
  together, which is the single thing the spare exists to survive. It now
  goes a cluster in front of the copy it describes. Every reference image
  does the same, a recorder's and a burner's alike; `run_udf_tests.sh` is
  what noticed that ours did not.

A file longer than 1,073,739,776 bytes is written as several extents, because
that is what a 30 bit length field with a 2 KB block comes to -- the same
split the reader sees on discs written by burners. **A file so large that its
extents no longer fit in one block is refused**, rather than written as though
it were whole: an extent is a gigabyte and a long allocation descriptor is
sixteen bytes, so a block holds the description of about 114 GB, and the
padding step used to `resize` a file entry past that back down to a block and
finish the image without a word. An image that says it is done and is not is
the worst answer available here.

**A file a UDF volume cannot name is named rather than dropped.** The names
written here are plain ASCII of 200 characters or fewer, which is what a disc
of recordings has — the files on one are `00001.m2ts` and `info.bdav` — but the
folder handed over is whatever the caller named. Anything else is left out of
the image, and now said so, with up to four of the names printed the way the
folder spells them. A dot-file is left out silently, being nobody's recording.

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
picture at the time it claims** and says where that picture ends; that the
arrival times never step backwards, span the recording, and **never come
closer together than the rate the index declares**; that the stream is a whole
number of aligned units and the image a whole number of clusters; and that the programme's name, the channel and its
number, the moment it went out and what the broadcaster said it was about all
survive being written into a disc, read back out of it, and written into a
second one — the last two compared as the bytes that went in.

**The image, three ways.** Each revision is written, and then: this program's
own reader opens it and finds the disc; 7-Zip, whose UDF reader was written by
somebody else, agrees it is a UDF volume of that revision and unpacks it to a
tree `diff -r` finds identical to the folder it was made of; and a cut taken
out of the image comes out the same md5 as the same cut taken out of the
folder. Separately, an image of a 1.2 GB file was written and unpacked byte
for byte, which is the extent split above.

Sixty-five checks, all passing.
