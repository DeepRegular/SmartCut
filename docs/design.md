# Design notes

[← Documentation](README.md) ・ [← SmartCut](../README.md) ・ [日本語](design.ja.md)

Decision: **a Rust core with a Tauri GUI**.

- Core: demux, packet selection and mux are all driven directly through
  `rsmpeg` / `ffmpeg-next` (the libavformat / libavcodec bindings).
  - Timestamps are assigned by us, so the seam problem disappears by construction
  - `nal_ref_idc` can be read straight off the packet, so the sampling of #3 is
    not needed
  - No intermediate files, and no multiple ffprobe passes
- GUI: Tauri (Windows / macOS / Linux, small distribution size). The timeline UI
  can be built with web technology.

The Python prototype is kept as a **reference implementation and test oracle**.
`tests/run_tests.sh` (Python) and `tests/run_rust_tests.sh` (Rust) share the same
frame-hash comparison.

## Licence and patents

**It ships under GPL-3.0**: of the two ways out below, the software encoders
are the side that was taken.

- **x264 / x265 are GPL.** Linking them makes the whole application GPL.
- The way around that is a **hardware encoder** (NVENC / QSV / VideoToolbox /
  AMF). Only partial GOPs get re-encoded, so the quality compromise is small and
  the licence situation is clean. The prototype can switch between them with
  `--video-encoder`.
- The **patent licences** for H.264 / HEVC (MPEG LA / Access Advance) need
  separate consideration for commercial distribution.
