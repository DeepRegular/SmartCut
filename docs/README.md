# Documentation

[← SmartCut](../README.md) ・ [日本語](README.ja.md)

The documentation is in two parts.

- **[User guide](#user-guide)** — how to use it. No background needed.
- **[Technical documentation](#technical-documentation)** — what it does
  inside. For anyone who wants to know how it works, and for anyone touching
  the code.

Every page exists in English and Japanese; the switch is at the top of each one.

---

## User guide

If this is your first time, start with [Using the GUI](user-guide/gui.md).

| | |
|---|---|
| [Using the GUI](user-guide/gui.md) | A walkthrough of every screen, with screenshots: adding recordings, cutting, output settings, writing the files |
| [Commercial detection](user-guide/cm-detection.md) | Finding the commercial breaks automatically: how accurate it is, and what to do when it gets one wrong |
| [Working through a batch](user-guide/batch.md) | Handling a whole evening of recordings at once: the background jobs, duplicating clips, and the export |
| [Projects](user-guide/projects.md) | Saving a night's work to a `.scproj` and picking it up later |
| [Using the command line](user-guide/cli.md) | Every option of the `smartcut` command |

---

## Technical documentation

To understand how it works, start with [the algorithm](technical/algorithm.md),
and especially with [the pitfalls](technical/algorithm.md#pitfalls): the eleven
reasons why "just cut on GOP boundaries and concatenate" does not work.

### How it works, and how it was checked

| | |
|---|---|
| [Algorithm](technical/algorithm.md) | How a cut is split into head, body and tail, and the eleven pitfalls that make it harder than it looks |
| [Validation](technical/validation.md) | Frame-hash verification results, testing against real broadcast recordings, and the known limits |
| [Audio](technical/audio.md) | Smart rendering applied to audio, boundary error, MPEG-2 AAC framing, downmixing, choosing the output codec, and multi-track broadcasts |
| [Broadcast TS](technical/broadcast-ts.md) | PID layout, the recording's own tables, captions and programme information, partial transport streams, ADTS, L-SMASH and DGIndex |
| [Commercial detection internals](technical/cm-detection.md) | The detector's scoring, logo detection, subtitle resets, and how the accuracy was measured |

### Discs

| | |
|---|---|
| [Reading a disc](technical/disc.md) | Blu-ray (BDAV and BDMV) and DVD-Video, from a folder or an `.iso`: UDF, IFO tables, ARIB text, one row per clip, and the chooser dialog |
| [Writing a disc](technical/bdav.md) | A night's cuts as a BDAV folder: the arrival times, the entry point map, where the programme's name comes from, and what is copied from a real disc rather than understood |

### Implementation

| | |
|---|---|
| [Rust core](technical/rust-core.md) | Timestamp generation, mixed SPS/PPS, the VC-1 encoder we had to write because libavcodec has none, and where the Rust implementation overtook the Python one |
| [Design notes](technical/design.md) | Why a Rust core with a Tauri GUI, and how the GUI is built: the filmstrip, the seek index, the proxy, playback and the two languages |
| [Building](technical/building.md) | Required libraries, how to build, how to run the tests |
| [Distribution](technical/distribution.md) | AppImage, tar.gz and deb, the Windows installer, and what each one bundles |
