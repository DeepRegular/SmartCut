# smartcut

English ・ [日本語](README.ja.md)

A smart-rendering cut tool: **it re-encodes only the partial GOPs that the cut
points fall inside, and copies everything else bit-for-bit.** Built primarily
for removing commercials from Japanese broadcast recordings (MPEG-2 TS).

On real terrestrial recordings **over 99% of the output is a lossless copy**.
A 22-minute, 5-range export driven by the automatic commercial detector came
out **bit-identical across all 40589 frames**.

```
... I ....... I=========================I ....... I ...
      ^t_in   ^k_first                  ^k_term   ^t_out
    |<-head->|<--------- body --------->|<-tail->|
      re-encode        stream copy       re-encode
```

## What it does

- **Smart rendering** — H.264 / HEVC / MPEG-2 / MPEG-4 Part 2. Cutting exactly
  on an access point re-encodes nothing at all.
- **Automatic commercial-boundary detection** — combines runs of silence with
  the presence of the station logo, and reports the runs that land on the
  15-second grid. Boundaries snap to access points, so **cutting commercials
  stays entirely lossless**.
- **Cut-editing GUI** — film strip, scene detection, scroll search, and preview
  playback with audio. What you see is always the *edited* timeline.
- **Built for broadcast material** — interlacing is preserved, 2:3 pulldown is
  handled on a field-level timeline, and dropped frames, non-zero `start_time`,
  and ARIB ADTS layout are all accounted for.
- **Output containers** — MPEG-TS / MP4 / Matroska, defaulting to the same
  container and directory as the input.

Codec and track-layout limits apply; see
[known limitations](docs/validation.md#既知の制限) (Japanese).

## Install

Grab a build from [Releases](https://github.com/DeepRegular/SmartCut/releases).
**You do not need to install FFmpeg** — both builds bundle it.

| | |
|---|---|
| Linux | `smartcut_0.1.0_amd64.AppImage` — needs glibc 2.39+ (Ubuntu 24.04 / Debian 13 / Fedora 40 or newer) |
| Windows | `smartcut_0.1.0_x64-setup.exe` (installer) or `smartcut-portable-x64.zip` (unzip and run). x64 only; needs the WebView2 runtime |

To build it yourself, see [ビルドと開発](docs/development.md) (Japanese).

## Usage

Drop a file onto the GUI, or use the command line:

```bash
smartcut input.ts --keep 5.3-12.7 -o out.ts   # keep the given range
smartcut input.ts --cut 8.0-20.0  -o out.ts   # drop the given range
smartcut input.ts --analyze                   # show the plan, write nothing

smartcut input.ts --analyze --detect-cm --logo  # commercial candidates
smartcut input.ts --analyze --scenes            # scene changes
```

| Option | Meaning |
|---|---|
| `--keep START-END` / `--cut START-END` | Ranges; repeatable. `1:23:45.6` form also accepted |
| `--audio-mode copy\|reencode` | `copy` (default) is lossless with ±10.7ms boundary error; `reencode` is sample-accurate |
| `--index scan\|container` | How access points are indexed. `container` is faster but unavailable for TS |
| `--detect-cm` / `--logo` / `--scenes` | Commercial candidates, logo assist, scene detection |
| `--no-open-gop` | Never start a copy at an open GOP |
| `-o OUTPUT` | Output path; the extension picks the container |

## Documentation

The documentation is in Japanese. The part worth reading first is
[実装上の難所](docs/algorithm.md#実装上の難所) — the eight reasons why
"just cut on GOP boundaries and concatenate" does not work, in the order they
were hit.

| | |
|---|---|
| [Algorithm and pitfalls](docs/algorithm.md) | How the cut is split, and the eight traps |
| [Rust core](docs/rust-core.md) | Timestamps, mixed SPS/PPS, audio boundaries |
| [Validation and limits](docs/validation.md) | Frame-hash verification, real-material results |
| [GUI](docs/gui.md) | Editor, thumbnail track, scene detection, playback |
| [Commercial detection](docs/cm-detection.md) | Silence plus logo, the 15-second grid, avoiding false positives |
| [Broadcast workflow compatibility](docs/broadcast-ts.md) | PID layout, ADTS, L-SMASH / DGIndex |
| [Building](docs/development.md) ・ [Distribution](docs/distribution.md) | How to build and ship it |
| [BDMV / BDAV](docs/bdmv.md) ・ [Design notes](docs/design.md) | Research and design decisions |

## Layout

```
rust/     Rust core (smartcut_core) and CLI   <- the real implementation
gui/      Tauri v2 + vanilla JS GUI
smartcut/ Python reference implementation     <- test oracle
tests/    66 end-to-end test cases
docs/     Documentation
```

The Python implementation is kept as the **reference implementation and test
oracle** that pinned down the algorithm and its pitfalls. It shares the same
frame-hash verification with the Rust core, and `tests/run_tests.sh` and
`tests/run_rust_tests.sh` report identical lossless ratios.

## License

[GPL-3.0](LICENSE).

x264 and x265 are GPL, and linking against them makes the whole application
GPL. Re-encoding can also be switched to a hardware encoder (NVENC / QSV /
VideoToolbox / AMF). Patent licensing for H.264 / HEVC needs separate
consideration for commercial distribution.
