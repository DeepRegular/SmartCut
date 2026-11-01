# Container contracts at a smart-render seam

Smart-rendered pictures must agree with both the source timeline and the
description carried by their output container. Successful decoding alone does
not check either contract.

## Source clock and aspect ratio

For progressive, non-transport inputs, the writer refines its existing frame
grid to represent the source container's ticks. A rounded average frame rate
does not describe a 40/40/80 ms cadence or the millisecond-rounded timestamps
in Matroska. Repeated holds are detected against the median measured gap as
well as the average. The seek-index version changes because older caches can
misclassify that cadence.

The refinement keeps the existing overflow bounds: an unrepresentable clock
or a duration that would exceed the signed 32-bit encoder grid retains the
previous grid. It is not a promise of exact timing for every possible clock.
Transport streams and interlaced material retain their previous clock rules.

Selection slack is separate from display coverage. The coarse-clock planner
deliberately moves CFR selection bounds before a picture; that must not make
each output range a few milliseconds short. A re-encoded range uses the next
measured picture for its end, where that is within a frame, or the real file
end. VFR ranges can still end inside a hold.

`av_guess_sample_aspect_ratio` supplies the container's effective SAR. It is
propagated to codec parameters, the boundary encoder and the output stream.
This matters particularly for AV1, which has no bitstream SAR to recover when
only `AVStream.sample_aspect_ratio` carries the MP4/Matroska display aspect.

## AV1 configuration

FFmpeg's `extract_extradata` bitstream filter extracts the Sequence Header from
each output packet. A small AV1 bit reader compares the public `av1C` fields;
it does not parse frame references or rewrite the coded pictures. The initial
track configuration comes from the first sequence actually written, with the
source extradata as a fallback when that sequence is carried only there.

- MP4: every sequence must agree with the track's `av1C` profile, first
  operating-point level/tier and bit-depth/chroma fields. Different sequence
  bytes with those same fields can remain in the stream.
- Matroska/WebM: require the same Sequence Header payload throughout. This is
  deliberately conservative about the `operating_parameters_info` exception
  permitted by the mapping.

The FFmpeg 7.1 MOV muxer writes one sample description. Updating its extradata
does not create another `stsd` entry. Therefore an incompatible seam cannot
be fixed merely by attaching the encoder's header. When the guard detects
one, the failed output context is closed and the selection is retried with
all its video ranges re-encoded. The log explicitly reports zero copied video
and the fallback. A second incompatible configuration remains an error.
Audio and subtitle modes still use the existing pipeline.

This fallback costs an extra attempt and loses the original copied video.
The analysis/GUI plan can still show the initial copy estimate; the rendering
log is authoritative after a fallback. Multiple MP4 sample descriptions or a
provably identical Matroska sequence could avoid that cost in future work.

## M2TS arrivals

After muxing and any SI graft, M2TS arrival timestamps are rebuilt from the
PCR clock in the final packet layout. PCR wrap is unwrapped, arrival times
are interpolated between PCR anchors, and packets before/after the anchors
share the nearest arrival (a burst). A one-PCR, one-picture cut is supported.
Only the four-byte prefix changes; every 188-byte transport packet and the
two copy-permission bits are preserved.

This avoids the invalid arrival clock produced by FFmpeg 7.1's variable-rate
M2TS muxing without selecting an arbitrary high fixed mux rate or adding null
packets. It adds a sequential read pass and an in-place read/write pass, with
memory proportional to PCR anchors. No PCR, malformed packet alignment or a
backward PCR is an error. This is not T-STD or Blu-ray compliance certification
and does not establish a maximum arrival/decoder-buffer rate.

## Regression suite

```bash
cd rust && cargo build --release --locked && cd ..
bash tests/run_container_contract_tests.sh
# Or select this suite through the normal runner:
bash tests/ci.sh container_contract
```

The standard-library Python suite needs FFmpeg/ffprobe with libx264, libx265,
libaom-av1 and librav1e. It generates its own 240-picture fixtures, checks 28
exports, and retains their logs and `results.json` under the printed temporary
directory. `python3 tests/container_contract_tests.py --help` allows explicit
tool paths and a parent work directory, including a native Windows build.
Checks cover strict decoding, frame count, source display PTS, video duration,
SAR, AV1 descriptions and all three M2TS table modes. MP4 duration uses the
video track's duration: a B picture's `stts` decode duration is not necessarily
its display duration.

Specifications and implementation references:

- [AV1 in ISO BMFF](https://aomediacodec.github.io/av1-isobmff/v1.3.0.html)
- [Matroska AV1 mapping](https://github.com/ietf-wg-cellar/matroska-specification/blob/master/codec/av1.md)
- [FFmpeg 7.1.1 MOV muxer](https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavformat/movenc.c)
- [FFmpeg 7.1.1 extradata filter](https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavcodec/bsf/extract_extradata.c)
