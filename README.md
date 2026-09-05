<div align="center">

# SmartCut

**Cut commercials out of a TV recording without re-encoding it.**

[![Release](https://img.shields.io/github/v/release/DeepRegular/SmartCut?style=flat-square&color=1f883d)](https://github.com/DeepRegular/SmartCut/releases)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%C2%B7%20Windows-lightgrey?style=flat-square)](#download)
[![Core](https://img.shields.io/badge/core-Rust-dea584?style=flat-square)](rust/)

English ・ [日本語](README.ja.md)

<img src="docs/images/demo.gif" width="880"
     alt="Two commercial blocks being removed from a recording in the SmartCut editor">

</div>

## What is SmartCut?

SmartCut is a desktop application for cutting Japanese TV recordings. You drop a
night's worth of `.ts` files onto it, it finds the commercial breaks
automatically, you check the boundaries and cut, and it writes the results out.

The important part is *how* it writes them out. A normal video editor decodes the
whole file and encodes it again, which costs time and quality. SmartCut re-encodes
only the few frames that sit inside a partial GOP at each cut point, and copies
everything else byte for byte. On real broadcast recordings, more than 99% of the
output is an exact copy of the input — and when the cuts land on keyframes, which
commercial breaks usually do, nothing is re-encoded at all.

This technique is called **smart rendering**. SmartCut applies it to the video, to
the audio, and to the subtitle and programme-information streams that a Japanese
broadcast carries alongside them.

## Why SmartCut?

**Nothing is lost that does not have to be.** Re-encoding a two-hour recording
degrades every frame of it. SmartCut touches at most a couple of dozen frames, and
often none.

**It is fast.** Copying bytes is limited by your disk, not your CPU. A 30-minute
recording is written out in well under a minute.

**Broadcast recordings survive intact.** Captions, programme information, the
station name, both tracks of a bilingual broadcast, interlacing, 2:3 pulldown, and
the original PID layout all come through. The output opens in the tools you
already use for recordings, because it still looks like a recording.

**Commercial breaks are found for you.** Three independent signals — the marks the
broadcaster puts in its own subtitle stream, runs of silence, and the presence of
the station logo — are combined to locate the breaks. SmartCut places the marks;
you decide what to cut.

**It handles a whole evening at once.** Drop twenty recordings in, press
`Ctrl+A` then `Ctrl+D`, and come back later. Reading, detecting and editing all
run at the same time, so the batch never blocks you from working.

## 30-second demo

The animation at the top of this page shows two commercial blocks being removed
from a 3 minute 45 second recording:

- **133.91 seconds copied bit-for-bit, 0.57 seconds re-encoded**
- **17 frames out of 6743 were touched at all**

That clip is a synthetic test recording, built by
[`tests/make_demo_media.sh`](tests/make_demo_media.sh) out of nothing but ffmpeg's
own test sources: colour cards, a fake station logo, and a 15-second commercial
grid. No broadcast material is involved.

Here is the same idea as a diagram. For each range you keep, only the parts either
side of the keyframes have to be rebuilt:

```
... I ....... I=========================I ....... I ...
      ^t_in   ^k_first                  ^k_term   ^t_out
    |<-head->|<--------- body --------->|<-tail->|
      re-encode        stream copy       re-encode
```

Cut exactly on a keyframe and even the head and tail disappear. A 22-minute,
5-range export driven by the automatic commercial detector came out
**bit-identical across all 40589 frames**.

## Download

Builds are on the [Releases page](https://github.com/DeepRegular/SmartCut/releases).
Every build except the `.deb` includes FFmpeg, so there is nothing else to install.

| Platform | File | Notes |
|---|---|---|
| **Linux** | `SmartCut_0.3.4_amd64.AppImage` | Make it executable and run it |
| **Linux** | `SmartCut-0.3.4-linux-x86_64.tar.gz` | Unpack and run `./smartcut`. Use this if you would rather not deal with FUSE |
| **Linux (Debian/Ubuntu)** | `smartcut_0.3.4_amd64.deb` | `sudo apt install ./smartcut_0.3.4_amd64.deb`. Only 3.0 MB, because it links against your system FFmpeg |
| **Windows** | `SmartCut_0.3.4_x64-setup.exe` | Installer |
| **Windows** | `smartcut-portable-x64-0.3.4.zip` | Unzip and run `smartcut.exe` |

**Requirements.** The AppImage and tar.gz need glibc 2.39 or newer, which means
Ubuntu 24.04, Debian 13, Fedora 40 or later. The `.deb` needs FFmpeg 7.1, which
means Debian 13 or Ubuntu 25.04 or later; it installs the GUI as `smartcut` and
the command-line tool as `smartcut-cli`. The Windows builds are x64 only and need
the WebView2 runtime, which is already present on Windows 11 and on nearly all
Windows 10 machines.

To build from source, see [Building](docs/developers/building.md).

## Quick Start

### With the GUI

1. **Add your recordings.** Drag them onto the window, or use **＋ Add files**.
   Each one is read in the background and indexed, so it will open instantly
   later.
2. **Find the commercials.** Press `Ctrl+A` to select everything, then `Ctrl+D`.
   SmartCut works through the list and marks the start of every commercial block
   and every return to the programme.
3. **Cut.** Double-click a recording to open the cut editor. The marks are already
   there: click the one at the start of a break, press `I`, click the one where
   the programme returns, press `←` then `O`, and press **✂ Cut**. Press **OK**
   when you are done with that recording.
4. **Check the settings.** The output settings tab covers the whole list: where to
   write, which container, what to do with the audio.
5. **Write it out.** The export tab writes the whole list, top to bottom.

Save the list at any point with `Ctrl+S` and it comes back next time, cuts and
all. There is a full walkthrough with screenshots in
[the user guide](docs/user-guide/gui.md).

### From a Blu-ray

A disc opens as it is -- as a folder, or as an `.iso` that is never mounted --
and both halves of the specification are read: **BDAV**, what a recorder
writes, and **BDMV**, what a pressed disc is.

Drop one on the window and it asks which of the recordings on it you meant, and
which of their tracks to take. That question is worth asking: a pressed season
set holds twelve episodes among fifty logos, warnings and menu loops, and the
disc calls all sixty-two of them `000NN`. The ones worth a look come already
ticked.

Cuts are written beside the disc under the programme's name, and the chapters
the disc set are already on the timeline as keyframes -- on a Japanese recording
those are frequently the commercial breaks themselves.

```bash
smartcut Anime.iso                      # what is on it
smartcut Anime.iso --title 2 --cut 8.0-20.0 -o out.ts
```

Encrypted discs are out of scope. See [Reading a Blu-ray](docs/developers/disc.md)
for how it is read and what it does not do.

### From the command line

```bash
smartcut input.ts --keep 5.3-12.7 -o out.ts   # keep this range
smartcut input.ts --cut 8.0-20.0  -o out.ts   # drop this range
smartcut input.ts --analyze                   # show the plan, write nothing

smartcut input.ts --analyze --detect-cm --logo  # list the commercial candidates
smartcut input.ts --analyze --scenes            # list the scene changes
```

`--keep` and `--cut` can be repeated, and accept `1:23:45.6` as well as plain
seconds. The full option list is in
[the command-line reference](docs/user-guide/gui.md#command-line-reference).

## Supported formats

**Input containers:** `.ts` `.m2ts` `.mts` `.m2t` `.mp4` `.mkv` `.mov` `.m4v`,
and Blu-rays -- BDAV or BDMV, a folder or an unencrypted `.iso`, read in place

**Output containers:** MPEG-TS, M2TS, MP4, Matroska, QuickTime. The default is the
same container and directory as the input.

**Video:** H.264, HEVC, MPEG-2, MPEG-4 Part 2. Interlaced material keeps its
interlacing, and 2:3 pulldown is handled on a field-level timeline. VP9 and AV1
are not supported — they have no elementary-stream form that can be concatenated,
so they would need a different design.

**Audio:** AAC is smart-rendered, and so is a Blu-ray's LPCM. Every track in the file
is cut independently, so a bilingual broadcast keeps both languages. 5.1 can be folded
down to stereo when you need it, and the sound can be written as another codec
entirely — AAC, AC-3, DTS or linear PCM — which leaves no frame to copy and so
re-encodes the whole track. The sample rate goes the same way, and for linear PCM
so does the bit depth. The output settings screen offers only what can actually be
written: a rate a codec does not have, or a bitrate below the floor its frames need,
is greyed out there rather than found out at the end of an export. AC-3, E-AC-3 and MP2 are copied through rather than
smart-rendered, and SmartCut tells you when that happens. A disc's lossless sound —
DTS-HD and TrueHD — is carried byte for byte and never re-encoded. Writing an MP4, where
there is no box for Blu-ray LPCM, the same samples go in as plain PCM.

**Broadcast streams (when writing a `.ts`):** ARIB STD-B24 captions are carried
across byte for byte. Programme information (EIT), station name (SDT) and
broadcast clock (TOT) are restored after muxing, and every stream goes back on the
PID it arrived on. Superimposed text and data broadcasting cannot be carried on a
cut timeline; SmartCut says so rather than dropping them quietly.

One video track per file. See [known limitations](docs/technical/validation.md#known-limitations)
for the full list.

## How safe is it?

**Your original file is never modified.** SmartCut only ever reads it. Output goes
to a new file, by default in the same directory with `cut_` in front of the name.

**Most of the output is provably identical to the input.** The copied regions are
byte-for-byte the same bytes. That is not an estimate; it is what stream copying
means.

**The parts that are re-encoded are measured, not assumed.** The test suite decodes
the output and compares it against the source frame by frame, by hash. Against real
broadcast recordings:

| Material | Result |
|---|---|
| Terrestrial NHK E-Tele (MPEG-2 1440x1080i) | 899/899 frames, 98.2% lossless, interlacing preserved |
| BS11 (MPEG-2 1920x1080i) | 899/899 frames, 98.2% lossless |
| AT-X (MPEG-2 1440x1080, 2:3 pulldown) | 719/719 frames, 99.9% lossless, pulldown pattern preserved |
| A 22-minute commercial cut, 5 ranges | 40589/40589 frames, **100% bit-identical** |

**The GUI tells you before you commit.** The status line under the timeline is the
plan the engine will actually execute: which ranges get copied, which get
re-encoded, and how many frames that is. If the badge says "Video completely
lossless", not one frame will be re-encoded.

**"100%" is never rounded up.** Two re-encoded frames out of 40000 rounds to 100.0%
in ordinary arithmetic, and that is exactly the number a smart renderer must never
print. SmartCut shows the frame count instead, and refuses to write 100% unless it
means it.

Full results, including the bugs found along the way and the limits inherent in
the approach, are in [Validation](docs/technical/validation.md).

## Technical documentation

Every page is available in English and Japanese; the switch is at the top of each
one.

**[→ Technical documentation](docs/README.md)**

| | |
|---|---|
| **User Guide** | [GUI](docs/user-guide/gui.md) ・ [Commercial detection](docs/user-guide/cm-detection.md) ・ [Projects](docs/user-guide/projects.md) ・ [Batch processing](docs/user-guide/batch.md) |
| **Technical** | [Algorithm](docs/technical/algorithm.md) ・ [Validation](docs/technical/validation.md) ・ [Broadcast TS](docs/technical/broadcast-ts.md) ・ [Audio](docs/technical/audio.md) |
| **Developers** | [Rust core](docs/developers/rust-core.md) ・ [Design](docs/developers/design.md) ・ [Building](docs/developers/building.md) ・ [Distribution](docs/developers/distribution.md) ・ [Reading a Blu-ray](docs/developers/disc.md) |

If you only read one page, make it
[the pitfalls](docs/technical/algorithm.md#pitfalls): the eight reasons why "just
cut on GOP boundaries and concatenate the pieces" does not work, in the order they
were hit.

## Repository layout

```
rust/     Rust core (smartcut_core) and CLI   <- the real implementation
gui/      Tauri v2 + vanilla JS GUI
smartcut/ Python reference implementation     <- test oracle
tests/    17 end-to-end suites, 267 checks
docs/     Documentation
```

The Python implementation is kept as a reference implementation and test oracle.
It is what pinned down the algorithm and its pitfalls in the first place. It shares
the same frame-hash verification as the Rust core, and `tests/run_tests.sh` and
`tests/run_rust_tests.sh` report identical lossless ratios.

## License

[GPL-3.0](LICENSE).

x264 and x265 are GPL, and linking against them makes the whole application GPL.
Re-encoding can also be switched to a hardware encoder (NVENC, QSV, VideoToolbox,
AMF). Patent licensing for H.264 and HEVC needs separate consideration if you
intend to distribute commercially.
