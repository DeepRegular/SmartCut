<div align="center">

# SmartCut

**Cut commercials out of a TV recording without re-encoding it.**

[![Release](https://img.shields.io/github/v/release/DeepRegular/SmartCut?style=flat-square&color=1f883d)](https://github.com/DeepRegular/SmartCut/releases)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%C2%B7%20Windows-lightgrey?style=flat-square)](#download)
[![Core](https://img.shields.io/badge/core-Rust-dea584?style=flat-square)](rust/)

English ・ [日本語](README.ja.md)

<img src="docs/images/demo.gif" width="1000"
     alt="Two commercial blocks being removed from a recording in the SmartCut editor">

</div>

## What is SmartCut?

SmartCut is a desktop application for cutting Japanese TV recordings. Drop a
night's worth of `.ts` files onto it and it finds the commercial breaks for you.
You check the boundaries, cut, and export.

What matters is how it exports. A normal video editor decodes the whole file and
encodes it again, which costs both time and picture quality. SmartCut re-encodes
only the handful of frames that fall inside a partial GOP at each cut point, and
copies everything else byte for byte. On real broadcast recordings, more than 99%
of the output is an exact copy of the input — and when the cuts land on keyframes,
which commercial breaks usually do, nothing is re-encoded at all.

This technique is called **smart rendering**. SmartCut applies it to the video, to
the audio, and to the subtitle and programme-information streams that a Japanese
broadcast carries alongside them.

## Why SmartCut?

**No quality is lost unnecessarily.** Re-encoding a two-hour recording degrades
every frame in it. SmartCut touches a few dozen frames at most, and often none.

**It is fast.** Copying bytes is limited by your disk, not your CPU. A 30-minute
recording is written out in well under a minute.

**Broadcast recordings come through intact.** Captions, programme information, the
station name, both tracks of a bilingual broadcast, interlacing, 2:3 pulldown and
the original PID layout are all preserved. The output still looks like a recording,
so it opens in the tools you already use for recordings.

**A night's cuts can leave as a disc.** Not only as files: SmartCut writes a
BDAV folder — the shape a recorder writes to a BD-RE — with each programme
under the name it went out under, the channel it came off, the night it was
recorded, what the broadcaster said it was about, and a chapter point at every
place a commercial break was taken out. A recording read off a disc keeps
everything its playlist said; a broadcast recording is read for its own
programme information. Asked for one, the finished disc is wrapped in a
**UDF 2.50 or 2.60 `.iso`** a burner can take. See
[Writing a disc](docs/developers/bdav.md).

**Commercial breaks are found for you.** SmartCut combines three independent
signals: the break marks the broadcaster puts in its own subtitle stream, runs of
silence, and whether the station logo is on screen. It places the marks; you decide
what to cut.

**It handles a whole evening at once.** Drop in twenty recordings, press `Ctrl+A`
then `Ctrl+D`, and come back later. Reading, detection and editing all run at the
same time, so a batch never stops you from working.

## 30-second demo

The animation at the top of this page shows two commercial blocks being removed
from a 3 minute 45 second recording:

- **133.91 seconds copied bit-for-bit, 0.57 seconds re-encoded**
- **17 frames out of 6743 were touched at all**

The clip is a synthetic test recording built by
[`tests/make_demo_media.sh`](tests/make_demo_media.sh) from ffmpeg's own test
sources: colour cards, a fake station logo and a 15-second commercial grid. No
broadcast material is involved.

Here is the same idea as a diagram. For each range you keep, only the parts that
stick out past the keyframes have to be rebuilt:

```
... I ....... I=========================I ....... I ...
      ^t_in   ^k_first                  ^k_term   ^t_out
    |<-head->|<--------- body --------->|<-tail->|
      re-encode        stream copy       re-encode
```

Cut exactly on a keyframe and even the head and tail disappear. A 22-minute
export of 5 ranges, placed by the automatic commercial detector, came out
**bit-identical across all 40589 frames**.

## Download

Builds are on the [Releases page](https://github.com/DeepRegular/SmartCut/releases).
Every build except the `.deb` bundles FFmpeg, so there is nothing else to install.

| Platform | File | Notes |
|---|---|---|
| **Linux** | `SmartCut_0.5.6_amd64.AppImage` | Make it executable and run it |
| **Linux** | `SmartCut-0.5.6-linux-x86_64.tar.gz` | Unpack and run `./smartcut`. Use this if you would rather not deal with FUSE |
| **Linux (Debian/Ubuntu)** | `smartcut_0.5.6_amd64.deb` | `sudo apt install ./smartcut_0.5.6_amd64.deb`. Only 3.2 MB, because it links against your system FFmpeg |
| **Windows** | `SmartCut_0.5.6_x64-setup.exe` | Installer |
| **Windows** | `smartcut-portable-x64-0.5.6.zip` | Unzip and run `smartcut.exe` |

**Requirements.** The AppImage and the tar.gz need glibc 2.39 or newer, which
means Ubuntu 24.04, Debian 13, Fedora 40 or later. The `.deb` needs FFmpeg 7.1,
which means Debian 13 or Ubuntu 25.04 or later; it installs the GUI as `smartcut`
and the command-line tool as `smartcut-cli`. The Windows builds are x64 only and
need the WebView2 runtime, which ships with Windows 11 and is already present on
nearly all Windows 10 machines.

To build from source, see [Building](docs/developers/building.md).

## Quick start

### With the GUI

1. **Add your recordings.** Drag them onto the window, or use **＋ Add files**.
   Each row is filled in the moment the file lands, and the reading, the
   thumbnails and the commercial detection then run behind it at the same time.
   You can open a recording before it has finished being read.
2. **Find the commercials.** Press `Ctrl+A` to select everything, then `Ctrl+D`.
   SmartCut works through the list and marks the start of every commercial block
   and every point where the programme resumes.
3. **Cut.** Double-click a recording to open the cut editor. The marks are already
   in place: click the one at the start of a break and press `I`, click the one
   where the programme resumes, press `←` then `O`, and press **✂ Cut**. Press
   **OK** when you have finished with that recording.
4. **Check the settings.** The output settings tab applies to the whole list:
   where to write, which container, and what to do with the audio.
5. **Export.** The export tab writes the whole list, top to bottom.

You can save the list at any point with `Ctrl+S`, and it comes back next time with
your cuts intact. There is a full walkthrough with screenshots in
[the user guide](docs/user-guide/gui.md).

### From a disc

SmartCut opens a disc as it is, either as a folder or as an `.iso` that is never
mounted. It reads both halves of the Blu-ray specification — **BDAV**, what a
recorder writes, and **BDMV**, what a pressed disc is — and it reads
**DVD-Video** as well, from the `.IFO` tables in `VIDEO_TS`: the titles, how long
each one runs, the chapters the disc set, and the audio and subtitle tracks it
declares.

Drop a disc on the window and SmartCut asks which of the recordings on it you
meant, and which of their tracks to take. That question is worth asking, because a
pressed season set holds twelve episodes among fifty logos, warnings and menu
loops, and the disc calls all sixty-two of them `000NN`. The ones likely to be
programmes are ticked for you.

**Opening one is instant.** A disc records where every picture a player may
start at is, in its own `CLIPINF` entry-point map, so SmartCut reads that
instead of walking the recording: a UHD title of 81 GB and 2 hours 34 minutes
went from 8 minutes 45 seconds to under a second. The languages come off the
same index, so cutting a bilingual disc leaves an output that still says which
track is which.

Cuts are written next to the disc under the programme's name, in a folder of
their own where there is more than one of them. The chapters the
disc set are already on the timeline as keyframes, and on a Japanese recording
they are frequently the commercial breaks themselves.

```bash
smartcut Anime.iso                      # what is on it
smartcut Anime.iso --title 2 --cut 8.0-20.0 -o out.ts
```

Encrypted discs are out of scope. See [Reading a disc](docs/developers/disc.md)
for how a disc is read and what SmartCut does not do with it.

### From the command line

```bash
smartcut input.ts --keep 5.3-12.7 -o out.ts   # keep this range
smartcut input.ts --cut 8.0-20.0  -o out.ts   # drop this range
smartcut input.ts --analyze                   # show the plan, write nothing

smartcut input.ts --analyze --detect-cm --logo  # list the commercial candidates
smartcut input.ts --analyze --scenes            # list the scene changes

smartcut input.ts --cut 8.0-20.0 --bdav ~/disc  # onto a disc rather than into a file
```

`--keep` and `--cut` can be repeated, and accept `1:23:45.6` as well as plain
seconds. The full option list is in
[the command-line reference](docs/user-guide/gui.md#command-line-reference).

## Supported formats

**Input containers:** `.ts` `.m2ts` `.mts` `.m2t` `.mp4` `.mkv` `.mov` `.m4v`
`.vob` `.mpg` `.mpeg` `.m2p`, plus discs — Blu-ray (BDAV or BDMV) and DVD-Video,
as a folder or as an unencrypted `.iso`, read in place.

A DVD title is a run of sectors of a single program stream that the format
required the disc to store in pieces of about a gigabyte. SmartCut joins the
pieces and takes the title out of them without unpacking or copying anything
first. The cut is written as a transport stream: putting it back into a DVD's own
shape would mean writing VOBUs, navigation packs and a rewritten `.IFO`, and
SmartCut writes none of those.

**Output containers:** MPEG-TS, M2TS, MP4, Matroska, QuickTime — or a **BDAV
disc**, a folder of recordings with an index a player reads as a list of
programmes, optionally wrapped in a UDF 2.50 or 2.60 image. By default the output uses the same container and directory as the
input.

**Video:** H.264, HEVC, MPEG-2, MPEG-4 Part 2, VC-1. Interlaced material stays
interlaced, and 2:3 pulldown is handled on a field-level timeline. **4K HDR10**
cuts too: the pictures rewritten at a boundary carry the recording's own
mastering metadata (PQ or HLG, primaries, luminance range, content light
level), so the tone mapping does not change partway through. VP9 and AV1 are
not supported: they have no elementary-stream form that can be concatenated, so
they would need a different design.

VC-1 — the codec most Blu-rays pressed before about 2010 were written in — is a
special case, because there is no VC-1 encoder anywhere: not in FFmpeg, not on a
graphics card. SmartCut therefore carries its own, for the partial GOPs at the
ends of a range. It writes intra pictures only, which is all a spliced fragment
needs and only a small part of the format. Those pictures cost more bits than the
ones they replace, but a fragment is under a second long, so the trade is worth
making. The rest of the recording is copied byte for byte as usual. `--vc1-quant`
sets how finely the pictures are written, from 3 (finest) to 31; the default lands
around 46 dB against the pictures it replaces.

**Audio:** AAC is smart-rendered, and so is a Blu-ray's LPCM. Every track in the
file is cut independently, so a bilingual broadcast keeps both languages. 5.1 can
be folded down to stereo when you need it, and the sound can be written as a
different codec entirely — AAC, AC-3, DTS or linear PCM — although that leaves no
frame to copy, so the whole track is re-encoded. The same applies to the sample
rate, and to the bit depth for linear PCM. The output settings screen offers only
combinations that can actually be written: a rate a codec does not support, or a
bitrate below the floor its frames need, is greyed out there rather than
discovered at the end of an export. AC-3, E-AC-3 and MP2 are copied through rather
than smart-rendered, and SmartCut says so when that happens. A disc's lossless
audio — DTS-HD and TrueHD — is carried byte for byte and never re-encoded. MP4 has
no box for Blu-ray LPCM, so when writing MP4 the same samples go in as plain PCM.

**Broadcast streams (when writing a `.ts`):** ARIB STD-B24 captions are carried
across byte for byte. Programme information (EIT), the station name (SDT) and the
broadcast clock (TOT) are restored after muxing, and every stream goes back on the
PID it arrived on. Superimposed text and data broadcasting cannot be carried on a
cut timeline, and SmartCut says so rather than dropping them quietly.

One video track per file. See
[known limitations](docs/technical/validation.md#known-limitations) for the full
list.

## How safe is it?

**Your original file is never modified.** SmartCut only reads it. The output goes
to a new file, by default in the same directory with `cut_` in front of the name.

**Most of the output is provably identical to the input.** The copied regions are
byte-for-byte the same bytes. That is not an estimate; it is what stream copying
means.

**The re-encoded parts are measured, not assumed.** The test suite decodes the
output and compares it against the source frame by frame, by hash. Against real
broadcast recordings:

| Material | Result |
|---|---|
| Terrestrial NHK E-Tele (MPEG-2 1440x1080i) | 899/899 frames, 98.2% lossless, interlacing preserved |
| BS11 (MPEG-2 1920x1080i) | 899/899 frames, 98.2% lossless |
| AT-X (MPEG-2 1440x1080, 2:3 pulldown) | 719/719 frames, 99.9% lossless, pulldown pattern preserved |
| A 22-minute commercial cut, 5 ranges | 40589/40589 frames, **100% bit-identical** |
| A Blu-ray in VC-1 (1920x1080i animation), 10s mid-GOP to mid-GOP | 308/308 frames, 90% of the video byte-identical, the rest rewritten at 48 dB |
| A Blu-ray in VC-1 (1920x1080p film, heavy grain), 10s mid-GOP to mid-GOP | 246/246 frames, 90% byte-identical, the rest at 45 dB with the grain intact |

**The GUI shows you the result before you commit to it.** The status line under
the timeline is the plan the engine will actually execute: which ranges are
copied, which are re-encoded, and how many frames that is. If it says "Video
completely lossless", not one frame will be re-encoded.

**"100%" is never rounded up.** Two re-encoded frames out of 40000 rounds to
100.0% in ordinary arithmetic, and that is exactly the number a smart renderer
must never print. SmartCut shows the frame count instead, and writes 100% only
when it means it.

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
| **Developers** | [Rust core](docs/developers/rust-core.md) ・ [Design](docs/developers/design.md) ・ [Building](docs/developers/building.md) ・ [Distribution](docs/developers/distribution.md) ・ [Reading a disc](docs/developers/disc.md) |

If you only read one page, make it
[the pitfalls](docs/technical/algorithm.md#pitfalls): the eight reasons why "just
cut on GOP boundaries and concatenate the pieces" does not work, in the order they
were hit.

## Repository layout

```
rust/     Rust core (smartcut_core), the VC-1 encoder and the CLI   <- the real implementation
gui/      Tauri v2 + vanilla JS GUI
smartcut/ Python reference implementation     <- test oracle
tests/    19 end-to-end suites, 297 checks
docs/     Documentation
```

The Python implementation is kept as a reference implementation and test oracle.
It is what pinned down the algorithm and its pitfalls in the first place. It
shares the same frame-hash verification as the Rust core, and
`tests/run_tests.sh` and `tests/run_rust_tests.sh` report identical lossless
ratios.

## License

[GPL-3.0](LICENSE).

x264 and x265 are GPL, and linking against them makes the whole application GPL.
Re-encoding can also be switched to a hardware encoder (NVENC, QSV, VideoToolbox,
AMF). If you intend to distribute commercially, patent licensing for H.264 and
HEVC needs separate consideration.
