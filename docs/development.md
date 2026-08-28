# Building and development

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](development.ja.md)

Builds on Linux (Debian 13 / Ubuntu 24.04 or newer). The Windows build is
cross-built from Linux too.

## Requirements

**FFmpeg must be from the 7.1 series.** `ffmpeg-sys-next` selects its bindings by
version, so a different series gives you a mismatched API.

```bash
# Build essentials (bindgen uses clang / libclang)
sudo apt install build-essential pkg-config cmake clang libclang-dev

# FFmpeg 7.1 development headers
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
                 libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev

# Prerequisites for the Tauri GUI (not needed if you are not building the GUI)
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
                 patchelf desktop-file-utils xdg-utils

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

| Item | Version |
|---|---|
| FFmpeg | 7.1 series (developed against 7.1.5) |
| Rust | 1.98 or newer |
| WebKitGTK | 4.1 (Tauri v2) |
| Node.js | 24 LTS (used to bundle the GUI) |

## Building

```bash
# Rust core and CLI
cd rust && cargo build --release
# -> rust/target/release/smartcut

# GUI
cd gui/src-tauri && cargo build --release
# -> gui/src-tauri/target/release/gui
```

### Release artifacts

For how the AppImage, tar.gz and deb, and the Windows installer are produced, see
[Distribution](distribution.md).

## Tests

```bash
bash tests/run_tests.sh               # Python E2E                                13
bash tests/run_rust_tests.sh          # Rust E2E (+9 with the container index)    13
bash tests/run_audio_tests.sh         # A/V sync (+5 with reencode)                5
bash tests/run_audio_content_tests.sh # is real material's audio in the right place 4
bash tests/run_preview_tests.sh       # does a scrub show the time you asked for    7
bash tests/run_proxy_tests.sh         # can the proxy stand in for the recording   22
bash tests/run_scene_tests.sh         # scene detection vs commercial boundaries    1
bash tests/run_ts_layout_tests.sh     # TS provenance and sequence headers          5
bash tests/run_cm_tests.sh            # commercial detection vs a human's answer    4
```

The synthetic fixtures (H.264 / HEVC / open GOP / 29.97 fps / MPEG-2 TS) are
generated automatically into `/tmp/smartcut-fixtures/`.

The tests that **read real material** are `run_audio_content_tests.sh`,
`run_preview_tests.sh`, `run_proxy_tests.sh` (which also runs on synthetic
material), `run_scene_tests.sh`, `run_cm_tests.sh` and
`run_ts_layout_tests.sh`. They look in `~/media` by default, which
`SMARTCUT_MEDIA` overrides. The audio comparison needs numpy (it SKIPs without
it).

Environment variables let you swap implementations and check that the result is
the same:

```bash
SMARTCUT_INDEX=container bash tests/run_rust_tests.sh   # take the index from the container
SMARTCUT_AUDIO=reencode  bash tests/run_audio_tests.sh  # sample-accurate audio
```

The [proxy](gui.md#proxy-editing-proxyrs) side is tunable the same way:

| Variable | Default | |
|---|---|---|
| `SMARTCUT_PROXY` | on | `0` / `off` to skip it (read from the recording directly) |
| `SMARTCUT_PROXY_WIDTH` | `960` | Proxy width (square pixels). Higher looks better and takes longer to build. **The cap is 1920x1080** (for tall material, where the height hits the cap first, the width comes down accordingly) |
| `SMARTCUT_PROXY_QUALITY` | `22` | Quality, in x264 CRF terms (lower is better; 18–24 is the useful range) |
| `SMARTCUT_PROXY_ENCODER` | auto | Comma-separated list of encoders to try (`mpeg4`, for instance) |

Width and quality are [part of the cache hash](gui.md#the-cache), so each setting
builds its own proxy.

## Validating against real material

`tests/verify_real.py <src> <out> <ranges>` — checks frame count, alignment,
bit-exact ratio, timeline, interlacing and A/V length difference in one go. For
the results, see [Validation](validation.md).

## Running in a virtual machine

WebKitGTK's compositor draws nothing on a machine without a GPU, and never
updates after the first paint — which looks exactly like a freeze. The app
defaults `WEBKIT_DISABLE_COMPOSITING_MODE=1`, so normally you do not have to
think about it.

If you drive the GUI mechanically with `xdotool`, DOM updates sometimes fail to
reach X and the screenshot goes stale. Resizing the window by 1px forces a
repaint.
