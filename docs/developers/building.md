# Building

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](building.ja.md)

SmartCut builds on Linux (Debian 13 / Ubuntu 24.04 or newer). The Windows build is
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

# Prerequisites for the Tauri GUI (skip if you are only building the core)
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

# The GUI under the name it ships as
cd gui/src-tauri && cargo tauri build
# -> gui/src-tauri/target/release/smartcut
```

The cargo crate is called `gui`, and it is `mainBinaryName` in `tauri.conf.json` that
renames the binary, so the name `smartcut` only appears on a bundling build.

For how the AppImage, tar.gz, deb and the Windows installer are produced, see
[Distribution](distribution.md).

## Tests

```bash
bash tests/run_tests.sh               # Python E2E                                     13
bash tests/run_rust_tests.sh          # Rust E2E (+9 with the container index)          13
bash tests/run_audio_tests.sh         # A/V sync (+10 with copy and reencode)            5
bash tests/run_audio_content_tests.sh # is real material's audio in the right place      6
bash tests/run_aac_tests.sh           # what the output's AAC frames are made of         8
bash tests/run_downmix_tests.sh       # where 5.1 goes when it is folded to stereo       9
bash tests/run_audio_codec_tests.sh   # writing the sound as another codec entirely     36
bash tests/run_audio_format_tests.sh  # the rate and the width the samples are written  23
bash tests/run_preview_tests.sh       # does a scrub show the time you asked for         7
bash tests/run_index_tests.sh         # does the index answer as the walk did           27
bash tests/run_proxy_tests.sh         # can the proxy stand in for the recording        22
bash tests/run_scene_tests.sh         # scene detection vs commercial boundaries         1
bash tests/run_ts_layout_tests.sh     # TS provenance and sequence headers               5
bash tests/run_broadcast_tests.sh     # captions, programme information, multi-audio    13
bash tests/run_cm_tests.sh            # commercial detection vs a human's answer         5
bash tests/run_disc_tests.sh          # a BDAV and a BDMV disc, as folders and as .isos 35
bash tests/run_bd_audio_tests.sh      # the sound a disc carries, written out             39
```

### Fixtures

The synthetic fixtures (H.264 / HEVC / open GOP / 29.97 fps / MPEG-2 TS) are generated
by `run_tests.sh` into `/tmp/smartcut-fixtures/`, so **run that first**. The suites that
reuse them (`run_rust_tests.sh`, `run_index_tests.sh`, `run_proxy_tests.sh`) stop with
`run tests/run_tests.sh first to generate fixtures` rather than quietly skipping half
their checks.

`run_disc_tests.sh` builds a whole disc of each dialect out of `mpeg2.ts` -- the stream
remuxed into 192 byte packets, index files written around it by `disc_index.py`, and a UDF
image wrapped over each by `genisoimage`, which it needs installed.

`run_bd_audio_tests.sh` builds one clip per codec a disc carries -- LPCM at 16 and at
24 bits, DTS, TrueHD, E-AC-3 -- out of the same `mpeg2.ts`, and cuts each into a `.ts`,
an `.m2ts` and an `.mp4`.

`run_audio_codec_tests.sh` asks for each of the four codecs the window offers -- AAC,
AC-3, DTS, LPCM -- into each of four containers, and checks three things of each: the
track is the codec that was asked for, every channel still carries the tone it went in
with, and a transport stream's own programme map declares the codec that is actually
in it.

`run_audio_format_tests.sh` asks for the other two things a sample is -- the rate it is
taken at and the width it is written with. A resample has to reach the samples, the
stream's declaration and, for AAC in a transport stream, the ADTS header on every frame;
a rate the codec does not have has to come back as the nearest it does, out loud. A width
only means anything where samples are written down, so it is honoured for LPCM -- where
it decides the file's size outright -- and declined out loud for everything else.

`run_audio_tests.sh` and `run_downmix_tests.sh` build their own fixtures into the same
directory: an impulse train, and a 5.1 track with a tone per channel. The codec and
format suites reuse the second of those; nothing else wants either.

Every suite writes its output next to the fixtures, under `$TMPDIR`. The proxy suite
needs several GB of that on real material — one proxy of half an hour of broadcast TS is
2.3 GB — which is more than a small `/tmp` tmpfs holds. If it fails with
`No space left on device`, point `TMPDIR` at a directory on disk:

```bash
TMPDIR=~/tmp bash tests/run_proxy_tests.sh
```

### Suites that read real material

`run_audio_content_tests.sh`, `run_aac_tests.sh`, `run_preview_tests.sh`,
`run_index_tests.sh`, `run_proxy_tests.sh`, `run_scene_tests.sh`, `run_cm_tests.sh`,
`run_ts_layout_tests.sh` and `run_broadcast_tests.sh`. (The preview, index and proxy
suites also run on synthetic material.)

They look in `~/media` by default, which `SMARTCUT_MEDIA` overrides. The audio comparison
needs numpy, and SKIPs without it.

### Swapping implementations

Environment variables let you swap implementations and check that the result is the same:

```bash
SMARTCUT_INDEX=container bash tests/run_rust_tests.sh    # take the index from the container
SMARTCUT_AUDIO=copy      bash tests/run_audio_tests.sh   # never touch the audio (smart is the default)
SMARTCUT_AUDIO=reencode  bash tests/run_audio_tests.sh   # sample-accurate audio
SMARTCUT_BYTE_SEEK=0     bash tests/run_preview_tests.sh # seek by timestamp again
```

## Environment variables

The [seek index](design.md#the-seek-index-seek_indexrs) can be built from the CLI too.
Pass the same path twice and the second run skips the walk:

```bash
smartcut rec.ts --seek-index /tmp/rec.scix --scenes
```

| Variable | Default | |
|---|---|---|
| `SMARTCUT_BYTE_SEEK` | on | `0` or `off` goes back to aiming at a timestamp and reading forward from `seek_margin` seconds early |

The [proxy](design.md#proxy-editing-proxyrs) is off by default, and tunable the same way:

| Variable | Default | |
|---|---|---|
| `SMARTCUT_PROXY` | off | `1` or `on` to build one. The preview, the strip and playback then read from it |
| `SMARTCUT_PROXY_WIDTH` | `1280` | Proxy width in square pixels. Higher looks better and takes longer to build. **The cap is 1920x1080**; for tall material, where the height hits the cap first, the width comes down accordingly |
| `SMARTCUT_PROXY_QUALITY` | `22` | Quality, in x264 CRF terms. Lower is better; 18–24 is the useful range |
| `SMARTCUT_PROXY_ENCODER` | auto | Comma-separated list of encoders to try (`mpeg4`, for instance) |

Width and quality are [part of the cache hash](design.md#the-cache), so each setting
builds its own proxy.

## Validating against real material

`tests/verify_real.py <src> <out> <ranges>` checks frame count, alignment, bit-exact
ratio, timeline, interlacing and A/V length difference in one go. For the results, see
[Validation](../technical/validation.md).

## Running in a virtual machine

WebKitGTK's compositor draws nothing on a machine without a GPU, and never updates after
the first paint — which looks exactly like a freeze. The app defaults
`WEBKIT_DISABLE_COMPOSITING_MODE=1`, so normally you do not have to think about it.

The same freeze arrives a second way, and this one waits for a click into a text field:
with GTK's XIM input-method module in the window, WebKitGTK stops painting the moment an
`<input>` takes focus. The program keeps running underneath — the screen behind the stale
pixels goes on changing, and a 1px resize brings it all back at once. (Screenshots that
went stale while driving the GUI with `xdotool` were this, not `xdotool`.) XIM is what GTK
falls back to when `GTK_IM_MODULE` is unset, which is every desktop where an IME was never
set up, so the app defaults `GTK_IM_MODULE=gtk-im-context-simple`. That module cannot
compose Japanese; on a machine with a working IME, set `GTK_IM_MODULE` to it (`fcitx`,
`ibus`) and the app leaves the choice alone.
