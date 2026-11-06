# Source clocks and variable-rate range ends

The timestamp clock and the nominal picture rate serve different purposes.
The boundary encoder still uses the existing rate and quality settings. The
output timeline must also represent the source's display timestamps.

## When the clock changes

`Grid::of` refines a progressive, non-TS source only when it is classified
as variable-rate. Constant-rate sources retain their nominal grid and
timescale even when a frame period is not a whole number of source ticks.
For example, 29.97 fps in Matroska has 33/34 ms timestamp steps because its
clock rounds to milliseconds. Written to MP4, that CFR source still has
exact 1001/30000 second steps, not the millisecond rounding of the input.
An already suitable grid is returned unchanged, without fraction reduction.

`on_clock` chooses a subdivision that represents both the field grid and
the source tick. Only a newly aligned grid reduces its output time base.
Checked arithmetic and the existing 32-bit MP4 step/timescale limits bound
the result; if no such refinement fits, the existing bounded grid remains.
This does not promise exact ticks for every possible clock and file length.

The walk measures repeated holds against the typical positive display gap
as well as the average. A 40/40/80 ms pattern must not be called constant
merely because its average frame lasts about 53 ms. Millisecond-rounded
NTSC and a 2:3 cadence still do not count as variable by that test.

An unclassified progressive stream with B pictures and an average over 1%
below its declared rate needs its display times read: the public container
index contains decode timestamps. `ContainerIndex` declines that case,
so the default `auto` mode falls back to a full packet scan before cutting.
Opening a large file can therefore take noticeably longer, since the whole
file must be read instead of just its seek table. This is a packet scan,
not a full-video decode. Ordinary CFR keeps the fast table path.
An explicitly required container index reports the reason it cannot answer.
Seek-cache version 19 invalidates the former classification.

## A seam is not the end of a keep-range

At a seam inside one kept range, the following segment resumes on a measured
picture. Extending the preceding segment to a nominal frame length would
shift every picture after the seam. `SegmentCtx::last` distinguishes this
from the end of a keep-range.

At a range end, a normal picture retains the coded-length floor used before
this change. The review reproducer has a picture at 37.996 s and OUT at
38 s. Refining the clock must not turn that picture into a 4 ms flash; its
normal 23.976 frame lasts about 41.708 ms. The existing
`coded_length_unbounded` cases retain their time-based hold behavior,
including a range entirely inside a long held picture.

On a refined clock, the final displayed picture also stays in the write
queue until a later displayed picture arrives or the final hold is known.
Decoder reorder depth alone is insufficient: a copied H.264 P picture can
precede three B pictures while `has_b_frames` is two. Releasing that P
picture too early left a 40/40/80 ms selection about 27 ms short. The queue
uses the existing 256 MiB seam-buffer limit. If that limit is exceeded,
`emit_one()` releases the picture as before so the cut can continue; its
final hold can then be shorter because that packet is no longer writable.
Other clocks retain the existing queue behavior.

## Reproducing the checks

Build the actual CLI with the project's FFmpeg 7.1 SDK, then run:

```sh
bash tests/run_source_clock_tests.sh
# Or select the executable and tools explicitly:
python3 tests/source_clock_tests.py --cut rust/target/release/smartcut \
  --ffmpeg ffmpeg --ffprobe ffprobe --work-dir /tmp
bash tests/run_vfr_tests.sh
```

The 95-case suite generates AVC, HEVC, AV1 and VP9 inputs, includes the reviewer's
unchanged 90-second 720p command, and checks single/multiple selections,
copy-only and single-picture cuts, long holds and CFR controls in MP4/MKV.
The millisecond CFR controls also export to MP4 and require exact nominal
frame steps and average rates, including 29.97 fps VP9 from Matroska.
It checks decoded counts, all display PTS, decode ordering, movie and video
track ends, compressed copied pictures and their decoded pixels. AVC/HEVC
VCL bytes are checked separately from repeated parameter-set NAL units.
Matroska stores no DTS; equal inferred DTS on its millisecond clock are
recorded, while backward DTS are rejected. MP4 DTS must strictly increase.

Each run writes complete commands, hashes and results to a new directory.
`--fixtures <previous-run>/fixtures` reuses the exact input bytes. FFmpeg
7.1's synthetic B-frame VFR MP4 edit list can hide its last generated
picture: the reference pixel decoder alone uses `ignore_editlist=1` to
read all generated packets. Outputs always undergo normal demuxing and
strict decoding, with their end times checked independently.

This suite tests timing and copying, not AV1 sample-description compliance,
SAR propagation, M2TS arrival clocks, GUI playback or hardware players.

The review assertions use the original default indexing. Forcing `--index scan`
on that duplicate-PTS synthetic file retains a pre-existing unbounded-hold
case (796 pictures rather than 794, with an approximately 4 ms range bound)
in both v0.8.39 and this change. That separate selection/hold behavior is not
claimed fixed here.
