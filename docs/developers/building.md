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
bash tests/run_rust_tests.sh          # Rust E2E (+9 with the container index)          15
bash tests/run_audio_tests.sh         # A/V sync (+10 with copy and reencode)            5
bash tests/run_audio_content_tests.sh # is real material's audio in the right place      6
bash tests/run_aac_tests.sh           # what the output's AAC frames are made of         8
bash tests/run_downmix_tests.sh       # where 5.1 goes when it is folded to stereo       9
bash tests/run_audio_codec_tests.sh   # writing the sound as another codec entirely     39
bash tests/run_audio_format_tests.sh  # the rate and the width the samples are written  23
bash tests/run_preview_tests.sh       # does a scrub show the time you asked for         7
bash tests/run_index_tests.sh         # does the index answer as the walk did           27
bash tests/run_proxy_tests.sh         # can the proxy stand in for the recording        22
bash tests/run_scene_tests.sh         # scene detection vs commercial boundaries         1
bash tests/run_ts_layout_tests.sh     # TS provenance and sequence headers               5
bash tests/run_broadcast_tests.sh     # captions, programme information, multi-audio    11
bash tests/run_cm_tests.sh            # commercial detection vs a human's answer         5
bash tests/run_disc_tests.sh          # a BDAV and a BDMV disc, as folders and as .isos 37
bash tests/run_bdav_tests.sh          # writing a disc: the index, the image, and both   53
bash tests/run_dvd_tests.sh           # a DVD-Video disc, as a folder and as an .iso    23
bash tests/run_bd_audio_tests.sh      # the sound a disc carries, written out             39
bash tests/run_vc1_tests.sh           # the VC-1 encoder, put through a decoder          4
```

### Fixtures

The synthetic fixtures (H.264 / HEVC / open GOP / 29.97 fps / MPEG-2 TS) are generated
by `run_tests.sh` into `/tmp/smartcut-fixtures/`, so **run that first**. The suites that
reuse them (`run_rust_tests.sh`, `run_index_tests.sh`, `run_proxy_tests.sh`) stop with
`run tests/run_tests.sh first to generate fixtures` rather than quietly skipping half
their checks.

`run_disc_tests.sh` builds a whole disc of each dialect out of `mpeg2.ts` — the stream
remuxed into 192 byte packets, index files written around it by `disc_index.py`, and a UDF
image wrapped over each by `genisoimage`, which it needs installed.

`run_bdav_tests.sh` goes the other way: it writes a disc of two recordings out of the
same fixture, opens it with SmartCut's own reader, and then has `bdav_index.py` check
every number in the index against the stream it is about — the entry point map
followed into the file point by point, the arrival times, and what the clip index says
the streams are. With a real broadcast recording in `~/media` it also checks that the
programme's name, the channel and the moment it went out survive a round trip through a
disc. The last of it is the image: each UDF revision is written, opened again by this
program, and -- where 7-Zip is installed -- unpacked by a reader written by somebody
else and compared against the folder it was made of.

`run_dvd_tests.sh` builds two DVDs out of the same stream, muxed to MPEG program
streams by ffmpeg's `dvd` muxer: an ordinary one, whose title set is written in two
files the way the format made discs write it, and one whose title was multiplexed in
two halves, so that its clock starts again in the middle. `dvd_index.py` fills in the
navigation packs — ffmpeg leaves their presentation times at zero — and writes the
`.IFO` tables around them.

`run_bd_audio_tests.sh` builds one clip per codec a disc carries — LPCM at 16 and at
24 bits, DTS, TrueHD, E-AC-3 — out of the same `mpeg2.ts`, and cuts each into a `.ts`,
an `.m2ts` and an `.mp4`.

`run_audio_codec_tests.sh` asks for each of the four codecs the window offers — AAC,
AC-3, DTS, LPCM — into each of four containers, and checks three things of each: the
track is the codec that was asked for, every channel still carries the tone it went in
with, and a transport stream's own programme map declares the codec that is actually
in it. It also asks for two things that cannot be written — 5.1 DTS under its bitrate
floor, and three channels of DTS, which is a count it has no arrangement for — and
checks that the first is raised to what DTS is ordinarily carried at and the second is
refused in a sentence that says why. Those are the answers the output settings screen
greys out, told to it by `writable_sound`.

`run_audio_format_tests.sh` covers the other two properties of a sample: the rate it is
taken at and the width it is written with. A resample has to reach the samples, the
stream's declaration and, for AAC in a transport stream, the ADTS header on every frame;
a rate the codec does not have must come back as the nearest one it does, with a message
saying so. A width only means anything where samples are written down, so it is honoured
for LPCM — where it decides the file's size outright — and declined with a message
everywhere else.

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

`run_vc1_tests.sh` reads real material too, and is named separately because what it
needs is particular: a recording written in **VC-1**, which in practice means a Blu-ray
pressed before about 2010. It is named by `SMARTCUT_VC1` — a whole `.m2ts` or a piece of
one — and without it the suite exits rather than pretending to pass. It also wants the
encoder's own example binary, which an ordinary `cargo build --release` does not
produce:

```bash
cd rust && cargo build --release --examples          # -> target/release/examples/vc1enc
SMARTCUT_VC1=~/media/disc.m2ts bash tests/run_vc1_tests.sh
```

Three of the four checks encode pictures out of that recording at a coarse, a middle
and a fine quantizer, decode each with libavcodec and measure what comes back; the
fourth takes a ten second cut whose ends fall between access points, so that a head and
a tail both have to be written, and checks that the copied stretch between them is a
verbatim run of the recording's own bytes. `SMARTCUT_VC1_AT` moves where the reference
pictures are taken from (20 s in by default), for a disc that opens on something too
flat to measure.

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

The same freeze arrives a second way, this time on a click into a text field. With GTK's
XIM input-method module in the window, WebKitGTK stops painting the moment an `<input>`
takes focus. The program keeps running underneath: the state behind the stale pixels goes
on changing, and a 1px resize brings it all back at once. (Screenshots that went stale
while driving the GUI with `xdotool` were this, not `xdotool`.) XIM is what GTK falls back
to when `GTK_IM_MODULE` is unset, which is every desktop where an IME was never set up, so
the app defaults `GTK_IM_MODULE=gtk-im-context-simple`. That module cannot compose
Japanese, so on a machine with a working IME, set `GTK_IM_MODULE` to it (`fcitx`, `ibus`);
where it is set explicitly, the app leaves it alone.
