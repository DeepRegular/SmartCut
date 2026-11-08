# AV1 configuration in MP4

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](av1-mp4.ja.md)

An AV1 boundary encoder can write different Sequence Header OBUs from the
recording's. Decoding that seam successfully does not prove that MP4's
`av1C` describes every sample which references it.

The writer applies this check **only when the selected output muxer is
`mp4`**. FFmpeg's `extract_extradata` filter supplies the Sequence Headers;
the guard reads only the fields copied into `av1C`: profile, the first
operating point's level and tier, bit depth, monochrome, chroma subsampling
and chroma sample position. All headers in an extracted packet are checked.
Different Sequence Header bytes can share a description when their required
fields agree.

The first sequence actually written supplies the track configuration. Where
that sequence is present only in the input extradata, it is attached to the
first output packet. This also fixes a selection wholly re-encoded with a
configuration different from the source's, which needs no second attempt.

When the plan includes copied pictures, a boundary encoder may declare a
lower level than the source. The writer uses the active output fields, or
the source sequence / fixed av1C fields when the first picture is encoded.
If only the level differs and the target permits the encoder's level, it
rewrites `seq_level_idx[0]` in the boundary Sequence Header and in its
extracted packet configuration. A regular header includes a tier bit above
index 7. A reduced still-picture header always infers tier zero. The writer
keeps the rest of the syntax, regenerates the trailing one bit and zero
padding, and updates the OBU size if needed. Copied packets are read only.
For entirely encoded selections the encoder's actual configuration applies.

Index 31 has no level-based constraints. For a finite target, the coded and
target indices must be defined, the target must be higher, and the other
av1C fields (including tier) must agree. Every Table A.3 limit is the same
or larger at a higher index, and so is the minimum compression ratio's
allowance (MinCompBasis × rate ÷ MaxDisplayRate: 2 ÷ 31,950,720 at 3.1,
4 ÷ 70,778,880 at 4.0), so any higher defined index may be written. The
guard declines finite promotions with explicit decoder model information,
whose buffers would need separate verification. A lower finite target, a
change of bit depth/chroma/profile, or inconsistent copied sequences takes
the existing retry path. See [AV1 Annex A.3](https://aomediacodec.github.io/av1-spec/av1-spec.pdf#page=651).

The boundary encoder is SVT-AV1 where it can write the recording, which
chooses a level from the pictures. SVT-AV1 has no 4:4:4, 4:2:2 or 12-bit,
so those go to rav1e, which writes index 31 unless told otherwise; 31 over
a finite source cannot be promoted and would force the retry. rav1e is
therefore given the source's first operating-point level (`level=X.Y`)
when that level is defined and main tier. Told, it writes exactly that
index and the coded pictures are unchanged. If rav1e refuses the level
because the pictures exceed it (a source that understates its level), it
is opened again without one. This applies to every output container, so
in Matroska and WebM these seams now state the source's level as well.

The boundary encoder is also told where the recording's chroma samples sit.
AV1 states that position in the sequence header itself, so a recording
written by libaom with a chroma sample position of 1 or 2 used to meet
seams that said 0, and the selection went to the retry below. SVT-AV1 and
rav1e write the position they are given, and the copied middle is kept.
libaom ignores it; a libaom seam that differs is still caught by the check
and retried.

FFmpeg 7.1's MOV/MP4 writer emits one `stsd` entry. A `NEW_EXTRADATA` side
datum updates its configuration, but cannot assign a second sample entry to
the changed samples. If the fields cannot be aligned during an MP4 export, the failed
context closes and the selection is retried with all video ranges
re-encoded. Audio and subtitle modes retain their existing settings. The
log identifies the initial copy figures as a discarded plan and states that
the retry copies no video. There is at most one retry; a further change is
an error.

This fallback adds an attempt, encoding time and generation loss. It does
not implement multiple MP4 sample descriptions. The analysis/GUI plan is
still the initial estimate; progress can restart on the retry. Source-clock,
VFR and SAR changes belong to separate contributions.

**Matroska and WebM keep their existing logic.** They do not enter this
guard or its full-video retry. Whether a changed AV1 sequence in Matroska
should cause a refusal, a warning, an option or a full re-encode is a policy
decision left to the maintainer, outside this change.

## Reproduce

```bash
cd rust && cargo build --release --locked -p smartcut-cli && cd ..
bash tests/run_av1_mp4_tests.sh
# Or: bash tests/ci.sh av1_mp4
```

Python 3.11+, FFmpeg/ffprobe with libaom-av1, librav1e and libsvtav1 are
required. The suite checks 18 MP4 exports and three Matroska controls.
The rav1e level-31 and finite Level 4.0 cases require mixed output with
the copied key packets and pixels intact, and so does an input with
colocated chroma, whose seams must state the same position. MP4 checks include strict decoding, frame counts, display PTS,
DTS, video duration, `av1C` against the independent CBS `trace_headers`
reader, and packet/pixel equality inside the declared copied intervals.
The Matroska controls assert that no configuration retry was added; they
do not certify Matroska sequence-header conformance.

Use `--cut`, `--ffmpeg`, `--ffprobe`, `--work-dir` and `--fixtures` for
explicit paths and paired runs on identical inputs. Commands, hashes,
results and output logs remain under the printed work directory. Media
and built binaries are not committed.

References:

- [AV1 in ISO BMFF v1.3.0, sections 2.2-2.4](https://aomediacodec.github.io/av1-isobmff/v1.3.0.html)
- [FFmpeg 7.1.1 single-entry stsd writer and packet extradata handling](https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavformat/movenc.c)
- [FFmpeg 7.1.1 extract_extradata filter](https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavcodec/bsf/extract_extradata.c)
