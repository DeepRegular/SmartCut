# Fitting a disc

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](transrate.ja.md)

**A night's recordings do not fit a Blu-ray.** Six hour-long programmes off a
Japanese broadcast come to thirty gigabytes and a single-layer disc holds
twenty-three of them, so something has to give: a recording left off, a second
disc, or the pictures written down less finely than they arrived.

This is the third of those. It is not a re-encode. Every picture keeps its own
macroblocks, its own motion vectors, its own place in its own GOP and its own
timestamps; what changes is how many bits the prediction error is written in.
Nothing here decodes a picture, which is what makes it fast enough to be worth
offering: a cut that rewrites every picture of a ten-minute recording takes
thirty seconds where a cut that copies them takes six.

The screen it is asked for on is the BDAV half of 出力設定, which draws what
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
squared; what it costs is the bits of its own code, counted exactly. Dropping
a *suffix* leaves every code before it alone, so the saving is known rather
than estimated.

Both are scale-free: the step the macroblock was already written at falls out
of both sides of the trade. That is deliberate. A recording's own encoder
wrote a fine scale where the picture was flat and a coarse one where it was
busy, and the point of this is to be the same recording written less finely,
not a different one.

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

Against whole recordings written onto a disc, that lands within about one per
cent:

| | estimate | actual |
|---|---|---|
| fifty minutes | 5,114 MB | 5,147 MB |
| forty-five minutes | 3,711 MB | 3,699 MB |
| fifty-nine minutes | 6,345 MB | 6,276 MB |

A per cent of a disc is a quarter of a gigabyte, so a hundredth of it is kept
back by default. A *short* stretch is a different matter — the rates are means
over a whole recording, and a minute taken out of the middle of one can be
half again as busy as its mean.

The share that comes out is the same for every recording in the list. Each
recording's own encoder decided how to spend its bits; giving the long one a
harder time than the short one because it happens to be longer would undo that
for nothing. A recording whose pictures cannot be rewritten takes its full
room on the disc and gives none of it back, and the share is worked out
against the ones that can.

Below 35% the pictures stop being a smaller version of themselves, and a list
that cannot fit without going under that is told so rather than written.
