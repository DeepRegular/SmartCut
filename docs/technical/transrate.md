# Fitting a disc

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](transrate.ja.md)

**A night's recordings do not fit a Blu-ray.** Six hour-long programmes off a
Japanese broadcast come to thirty gigabytes and a single-layer disc holds
twenty-five of them, so something has to give: a recording left off, a second
disc, or the pictures written down less finely than they arrived.

This is the third of those. It is not a re-encode. Every picture keeps its own
macroblocks, its own motion vectors, its own place in its own GOP and its own
timestamps; what changes is how many bits the prediction error is written in.
Nothing here decodes a picture, which is what makes it fast enough to be worth
offering: a cut that rewrites every picture of a ten-minute recording takes
thirty seconds where a cut that copies them takes six.

The screen it is asked for on is the BDAV half of Output settings, which draws what
the list comes to against the disc and says what has to come off. The engine
is [`smartcut_mpeg2`](../../rust/crates/mpeg2/); the arithmetic that turns a
disc into a share is [`fit.rs`](../../rust/crates/core/src/fit.rs).

## What a picture is made of

An MPEG-2 picture is a few dozen slices, each a run of macroblocks, each of
those an address, a type, its motion vectors, and the coefficients of up to
six blocks. The coefficients are the picture: on a broadcast recording they
are most of the bytes. Everything else says where they go.

So the picture is read apart down to the coefficients and written back with
the rest of it copied bit for bit — the same address increments, the same
types, the same vectors, the same intra DC that the blocks after it are coded
as differences from. Two things are done to the coefficients on the way
through, and a run of pictures is written at whatever mixture of the two it
takes to reach a size.

**The scale is lifted.** Every level is divided down to a coarser quantiser
step, which is what the quantiser scale code in each slice and each
macroblock is for. A code is coarse — on the non-linear scale a broadcast
uses, the step from the first code to the second is a doubling — so a code
becomes *two* codes and a share: at a lift of one code and a half, half the
macroblocks take one code coarser and half take two, spread evenly over the
picture by the golden ratio rather than in a patch. The average step across
the picture is then the fractional one that was asked for.

**The tail of a block is dropped.** A block's last few coefficients are its
dearest — each is a code of its own, and they are what keeps the end-of-block
from coming sooner — and they are also its smallest. Where what they are
worth is less than what they cost, they go. What a coefficient is worth is its
level times what the quantisation matrix says about its place in the block,
squared, and scaled by the step it is written at against the recording's
usual step; what it costs is the bits of its own code, counted exactly.
Dropping a *suffix* leaves every code before it alone, so the saving is known
rather than estimated.

The lift is scale-free -- every macroblock moves by the same ratio, so the
recording's own encoder's choices of where to be fine and where to be coarse
stand. The tail is not, and was until 2026-10: a level of one in a coarsely
quantised macroblock stands for several times the picture that one in a fine
macroblock does, and weighed the same, the large coarse residual of a dissolve's
B picture went first -- the old scene's blocks were left in the new one, and a
picture came out at 19 dB in a run averaging 42. Each coefficient is now priced
at its macroblock's step squared against a running mean of the pictures' own
steps (about three seconds' worth), so the pressure means what it did before on
any recording, and the macroblocks coarser than their neighbours keep more.

### Which of the two does the work

The second, by a long way.

| | share of the pictures' own bytes | mean Y-PSNR |
|---|---|---|
| by lifting the scale alone | 0.739 | 42.0 dB |
| the two together, as it is done | 0.759 | 48.3 dB |

Six decibels, measured against the source on a minute of a 1440x1080
broadcast — and the lower row is the *smaller* file of the two. It is also
what the tool this was measured against does: at three quarters of the size it
has lifted the scale by two thirds of one code on average and thrown away two
coefficients in five, and a quarter of its blocks with them.

## The rate control

**The share is not applied to each picture.** It is applied to the recording.

A picture forced to a fixed share of *itself* is a picture whose place in its
own GOP has been taken away from it. The I picture that opens a GOP is large
because every other picture in that GOP is predicted from it; squeezing it by
the same fraction as the B pictures hanging off it spends the most where it is
worth least and takes the most from where everything else depends. It was
written that way first, and the pictures that came out were worse than the
ones a single scale produces at the same total size by about as much again as
the choice of lever above is worth.

So the run settles at one pressure and leaves the recording's own sharing out
of bits alone. What moves the pressure is a standing count of what the
pictures so far have cost against what they were meant to, turned into a lift
slowly: five pictures' worth of overspend to move it by one, and a slow hand
back to the settled scale so that the run does not have to stay in debt to
stay where it is. The first picture of a run is searched properly — half a
dozen rewrites of it, each measured and thrown away — so that the opening
second is not written at full size and paid for later.

Landing on a size, over a minute of broadcast asked for 0.7557 of its
pictures: **0.7594**. Over a whole disc it is the estimate that decides
whether the disc closes, not this; see below.

## What it costs

**Drift.** The encoder wrote each predicted picture as the difference from a
reference it had in front of it. The decoder now rebuilds that reference
slightly differently, because the coefficients that built it were rounded
here, and the difference is added to rather than corrected. It accumulates
along a GOP and is cleared at the next intra picture.

Every fast transrater pays it, this one and the reference tool alike. Measured
frame by frame against the source, both draw the same sawtooth: best on the
picture that opens a GOP, worst on the picture before the next one, about five
decibels between the two ends at a mild reduction.

The alternative is to carry the error forward and subtract it from the next
picture, which means keeping decoded references — an inverse transform and
motion compensation, i.e. most of a decoder, and most of the time a decode
would have cost.

### Against the tool it was measured against

The same minute of the same recording, written by both to the same size:

| | share of the pictures' own bytes | mean Y-PSNR | worst frame |
|---|---|---|---|
| the reference tool | 0.7557 | 46.4 dB | 40.4 dB |
| this | 0.7594 | 48.3 dB | 42.7 dB |

## What it will not do

**Anything but MPEG-2.** It is the format a Japanese broadcast and a
recorder's own disc are in, and it is the one whose macroblock layer can be
rewritten without decoding it. H.264 and HEVC cannot: their coefficients are
coded against neighbours that would have to be decoded to be read, and their
entropy coding is adaptive. A recording in either is written at its full size
and the run says so.

That includes the few pictures re-encoded at the ends of each range. Those are
encoded afresh whatever the format, and where the pictures around them are being
written smaller they are given the same share of their bit rate, so that they are
not the only fine pictures on the disc. Where the pictures around them are copied
at their full size, they are encoded at the full rate too (`open_encoder_as` in
[`cut.rs`](../../rust/crates/core/src/cut.rs)): the share is applied only where
what is written is MPEG-2. Up to 0.8.35 it was applied to every seam, so an H.264
cut given `--video-share`, or written onto a disc with **Transcode** on, had its
seams re-encoded at the share: 39 dB beside copied pictures at 48, the only
pictures in the cut that were made worse.

**4:2:2 and 4:4:4**, which no broadcast or Blu-ray recording is; **MPEG-1**,
which has no picture coding extension and a stuffing code in the macroblock
layer; and the **scalable profiles**, which put fields in a slice that are not
read here. Each is declined by name rather than guessed at, and a picture
declined is written through exactly as it arrived.

**A picture whose bytes are not in its coefficients.** On synthetic material —
a test pattern, a caption card — a P or B picture can be almost entirely
macroblock addresses, types and motion vectors, and none of that is touched
here. Such a recording comes out at ninety-five per cent of its size whatever
is asked of it. Broadcast material is not like that, but a run that cannot
reach the size it was asked for says what it reached instead, and the output
screen says what the finished disc actually came to.

## The identity

This is the whole of the evidence that the walk is right.

A picture read apart and written back **at no pressure at all** has to be the
picture that arrived, byte for byte. Every field of every macroblock is
written again from what was read — the address increments and their escapes,
the type, the motion vectors and their residuals, the DC size and difference,
every run and level and escape, the stuffing bytes some encoders leave after
a slice — so a table with one row wrong, a length counted wrong, a run
misplaced or a first-coefficient rule missed comes out as bytes that differ.

| | pictures | differ | declined |
|---|---|---|---|
| eight broadcasters, half an hour each | 425,452 | 0 | 7 |
| a fifty-minute recording off a disc | 89,899 | 0 | 0 |
| the same, as the reference tool rewrote it | 106,082 | 0 | 0 |
| the same, as this program cut it | 88,101 | 0 | 0 |
| **total** | **709,534** | **0** | **7** |

The declines are one picture each in seven of the broadcast recordings, and it
is the same picture every time: a recording that starts in the middle of one
has a first picture that is not a whole picture. It is written through as it
arrived and the run says so.

**The stuffing after a slice is written back only at no pressure.** A picture
being made smaller leaves it out: those zeroes are there to hold a stream at a
constant rate and are no part of the picture. Some broadcasters pad that way —
a quarter of one recording's bytes, and two fifths in its first twenty
seconds — and a recording off a DVD recorder a few per cent. Up to 0.8.36 the
padding was kept at every pressure, so a padded recording could not come down
to the share asked: a minute asked for 35% came out at 55%, and a disc asked
to fit 130 MB came to 144.5 MB (the run said so). The same disc now comes to
125.8 MB, and a recording that pads nothing is written as before.

`tests/run_transrate_tests.sh` checks the identity on whatever material is in
`~/media`, that a cut asked to fit a size fits it, that what it cost is still
above 32 dB, and that a recording that cannot be rewritten says so.

## Where the size comes from

[`fit.rs`](../../rust/crates/core/src/fit.rs) answers the question the screen
asks: given a list of cuts and a disc, what share do the pictures have to come
to?

It is arithmetic on rates rather than a pass over anything. The pictures'
rate was measured while the recording was indexed and the sound's is declared;
both are multiplied by how much of the recording is being kept, and the
framing a disc puts round them is added — 192 bytes of transport packet per
184 of payload, a PES header per picture and a part-empty packet after it,
the tables written every frame and the clock in a packet of its own, the clip
index, and the 196,608 bytes a stream is padded out to.

A recording read off a disc has no such measurement behind it. The entry-point
map in `CLIPINF` says where every picture a player may start at is, and nothing
about how big any of them is. So the rate is sampled: two dozen windows of two
seconds each, tens of megabytes against the tens of gigabytes the walk reads.

What the windows are read for is the *share* the pictures take of the bytes,
not the rate itself. A broadcast recording's rate over two seconds has nothing
to do with its rate over the hour — on the recording measured here the windows
ran from 5 to 14 Mbit/s against an average of 11, and their mean landed five
per cent out, which is no better than the guess it replaces. The share is the
steady figure: 0.875 to 0.937 across the same windows, byte-weighted 0.920
against a counted 0.9195. The rate it is applied to comes off the map itself —
the bytes between its first entry point and its last, over the time between
them — which is exact and costs nothing. On six broadcast recordings the
sampled rate lands within 0.7% of the counted one on every clip, and 81.67%
against 81.41% for the share of the disc they came to.

The sound's rate is declared, except where it was taken away: a recording whose
opening belongs to the programme before it has that programme's sound described
in its header, and what the container said is dropped rather than believed.
What a codec is worth at that many channels stands in. Sizing a compressed
track with the arithmetic that sizes linear PCM instead — samples by channels
by bits — charges an AAC track carried at 250 kbit/s a megabit and a half,
which on four of those six recordings put the estimate 14% over.

**An AAC rate read off a transport stream is not believed below half the
usual.** ADTS and LATM frames carry no rate, so what libav states for one in
a program or transport stream is the average over the few frames its probe
read, and a recording that opens quiet makes that a fraction of the track's:
a recorder title's stereo track stated 29 kbit/s where it was carried at 190,
and a 300-second cut fitted to 400 MB came out 8.5 MB past its price on the
sound alone. So where the track is AAC on a 90 kHz clock, or LATM, a stated
rate below half of what the codec is worth at that many channels gives way to
that figure (`audio_rate`). A track really carried that low is then priced
high, and its pictures come out a little smaller than they needed to; none
of 35 broadcast services carries one that low. An MP4 or Matroska file
states what the track holds, and AAC-LC at 64 kbit/s or HE-AAC at 48 is
common and true there: distrusted everywhere, an MPEG-2 cut with tracks
like those fitted to 30 MB was written at 51% and came to 26.0 MB, where
57% had made 28.9. Those are told apart by the clock, which is the sound's
own samples in an MP4 and milliseconds in Matroska.

**A disc's tables are priced by the second, not as a share.** The list of
programmes and the map go out every frame, the clock in a packet of its own,
and a partial stream's SIT and SDT besides -- at the same rate whatever the
pictures are doing. Up to 0.8.37 they were priced as 0.7% of the rest, so the
smaller `--fit` made the pictures, the further short the price fell: a
recorder title cut to 300 seconds at 75% carried 5.5 MB of them where 3.7
had been priced, and a run fitted to 400 MB came out over it. Counted on three
cuts written to a disc, a broadcast recorder title sends 99 packets a second
of them, a broadcast `.ts` 104, and a DVD title with linear PCM 103 (no clock
packet there, but the map fifty times a second). So a disc is now charged 104
packets of 192 bytes a second for them (`DISC_TABLES` in
[`fit.rs`](../../rust/crates/core/src/fit.rs)). A cut into a file keeps the
share: a `.ts` carries the broadcast's own tables as the broadcast sent them,
and a file is never priced below its own rate besides.

Against whole recordings written onto a disc, that lands within about one per
cent:

| | estimate | actual |
|---|---|---|
| fifty minutes | 5,114 MB | 5,147 MB |
| forty-five minutes | 3,711 MB | 3,699 MB |
| fifty-nine minutes | 6,345 MB | 6,276 MB |

A per cent of a disc is a quarter of a gigabyte, so a hundredth of it is kept
back by default.

**A stretch that is kept is priced from what it holds.** The rates are means
over a whole recording, and a minute taken out of the middle of one can be
half again as busy as its mean. Priced at the mean, two quarter-minute pieces
of one broadcast came to 37 MB where the cut was 39, and fitted to a 20 MB disc
they were written at 21 MB; three minutes of another, fitted to 90% of its
plain cut, came out 2.4% larger than the room it was given. The index already
knows the byte each entry point starts at, and the bytes between two
neighbouring points are what a copy carries for that stretch. So each range
kept is priced from the pairs of points it covers: what they hold beyond the
average for that much clock -- or short of it, where the stretch is quieter --
goes onto the pictures, which are what moves; the sound and the tables go out
at the same rate whatever the pictures are doing (`fit::Profile`). The
positions are whatever the index has: the walk's, a disc's entry-point map,
a container's seek table, or a saved index. Clock no pair covers is priced at
the average, as before: before the first point and after the last, a pair whose
bytes go backwards (a title that runs on into its next file and counts from
nought again), an index that holds no positions; and an index whose pairs cover
less than ten seconds is not used at all. A whole recording kept is priced as
it was. `--fit` and the window's gauge and **Transcode** price the same
stretches this way, so the same cuts are given the same share.

Against the plain cut of twelve broadcast recordings, three minutes of each
now price 0.35% to 1.8% above what is written, where the mean put them from 3.6%
under to 2.0% over; two quarter-minute pieces of each, 1.5% under to 6.5% over,
where it had them 5% under to 30% over. What can still come out larger than
the room it was fitted to:

- **The rewrite does not always reach the share it is asked for.** On short
  pieces at a hard share it can land a few points above it -- 62% of the
  pictures where 55% was asked, on the worst of the twelve -- so short pieces
  fitted to 60% of their size came out up to 5% over.
- **The pictures re-encoded at each range's ends are not priced.** Each range
  opens and closes on a few pictures written afresh, which on one recording
  cost about 0.8 MB a range more than the stretch held. Five ranges of three
  seconds cut to 14% more than their price; ten to twenty ranges over an hour's
  programme come to a tenth or two of a per cent, inside the hundredth kept back.
- **Only MPEG-2 can be written smaller.** For anything else the share only
  says whether it fits as it is, and into a `.ts` the file's own rate stands in
  for what it carries: a phone's H.264 recording came out 4% above that.

**And where it does, the run says so.** The first of those is the one that can
go furthest: pictures that are mostly macroblock headers and motion vectors
have a floor of their own, and thirty seconds of one broadcast asked for 48.8%
went no smaller than 60.1%, so the disc came out 15% over the size `--fit`
named -- and the run ended with `wrote` and nothing else. The rewrite now says
when it lands more than two points above the share asked for, and how many
megabytes more than planned the pictures it rewrote came to (`Shrink::say` in
[`cut.rs`](../../rust/crates/core/src/cut.rs)). That figure is of those
pictures alone, so the note adds that the cut as a whole may still fit; up to
0.8.33 it called the figure how much larger the cut was than planned, and a
cut that still fitted was told so. And once the run is over, the CLI weighs
what it wrote against the size `--fit` priced against, and says by how much
it came out over, as the window's check after a run already did. With
`--bdav` that is the whole disc folder; with an overwritable image, the folder
counted in the whole clusters the image lays its files out in; with a
read-only image, the image itself, once it is written -- each file takes
whole extents in it and the file system has structures of its own, so a
folder under the size can make an image over it, and up to 0.8.33 such an
image was weighed by its folder and said nothing. The note also says why:
the recording weighed more than it was priced at; or the run was
priced to fit as it was, so the pictures were left as they were (with
`--video-share` to write them smaller, or, for anything but MPEG-2, the
reminder that only MPEG-2 can be -- up to 0.8.33 an H.264 cut was told to
try `--video-share` too); or, for an image whose files alone come to less
than the size, that the rest is the image's own file system, which `--fit`
does not price. Where the pictures were written smaller, the note says what
the run was priced at with them at that share and how far past that it came,
and puts that down to the recording weighing more than its index measured --
the pictures, or what goes round them -- with the note above it where the
pictures would not shrink as far as asked. Up to 0.8.37 it blamed the
pictures alone, and a disc whose pictures had come to exactly their share,
over because its tables had been priced short, was told the pictures would
not shrink. Nothing written is changed by any of this: they are
messages. The window's check after a run weighs the same things and, with
**Transcode** on, gives the same reasons -- where none of the recordings written is MPEG-2, that only MPEG-2 can be
transcoded. Up to 0.8.34 it weighed a read-only image by its folder too, and
chose its sentence by whether the pictures had been transcoded, so a list
priced to fit with **Transcode** on, or one with no MPEG-2 in it, was told to
turn **Transcode** on (`startExport` in
[`app.js`](../../gui/src/app.js)).

**A recorder's title is measured over clock that no cut writes.** At a seam,
from where the pictures before it stop to the first entry point after it,
nothing is written
([the stretches no cut writes](disc.md#what-the-playlist-plays-and-what-it-steps-over)),
and the seconds kept are counted around those stretches. The rates are spread
over the whole clock, so their product came out short by the share of the
clock the stretches take: 0.7% on one recorder title with 19.4 seconds of them,
most of the hundredth kept back. The pictures' rate, and the file's where that
is not known, are now scaled by the clock over what is written
(`fit::written_scale`), which is 1 for every recording without seams.
Dividing a row by size uses the same scale.

**A disc already in the folder is measured, not estimated.** Writing into a
folder that holds a disc adds to it, and what the list has is the room the disc
has left. Every file under `BDAV` is counted, by the rule the check after
writing counts the finished disc by (`fit::already_on`), and taken off the
capacity before the share is worked out, so the hundredth kept back is a
hundredth of what is left. The window leaves out the streams its own run is
writing, since those are the list it is pricing. Until this, every run was
weighed against the whole disc: three runs into one folder for a 30 MB disc
were each told they fitted and came to 34,216,478 bytes, where they now come to
28,711,454.

**An overwritable image is weighed against what a recorder's layout holds**, not
against the disc's size (`fit::holds`): 24.12 GB of files on a single layer and
48.34 GB on a dual ([the image](bdav.md#the-image)), and the dual layer's room on
an XL disc too, since no recorder lays one out larger and an image past the dual
layer is refused when it is written. Priced at the "25GB" of 25,025,314,816 bytes
less the hundredth kept back, a list of 24.12 to 24.78 GB was said to fit and came
out a dual-layer image no 25 GB disc takes. In the window's **Disc used**, a single
layer reads 22.5 GiB rather than 23.3; the check after writing counts each file in
whole 64 KB units; and a size given in bytes takes the dual layer's room from
48,440,016,896 bytes up, where the dual-layer image fits. A read-only image, and a
disc written with no image, are weighed as before.

The share that comes out is the same for every recording in the list. Each
recording's own encoder decided how to spend its bits; giving the long one a
harder time than the short one because it happens to be longer would undo that
for nothing. A recording whose pictures cannot be rewritten takes its full
room on the disc and gives none of it back, and the share is worked out
against the ones that can.

Below 35% the pictures stop being a smaller version of themselves, and a list
that cannot fit without going under that is told so: the command line stops
before writing anything. The window says so under **Disc used** -- the share the
pictures would have had to come to, and that 35% is as far as they go -- and
asked to write the list anyway, writes its pictures at 35%. Up to 0.8.35 it named
the floor in both places ("would have to come to 35.0%, and 35% is as far as
this goes"), whatever the list needed.

A list with no MPEG-2 in it has nothing to write at a share. Where it does not
fit, the window says by how much and that what is not MPEG-2 cannot be
transcoded, and the run writes it as it is. Up to 0.8.35 such a list was told it
would not fit even at 35%, the run began by saying it would write the pictures
at 35% of their size, and that share reached its seams (above).
