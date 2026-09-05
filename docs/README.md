# Technical documentation

[← SmartCut](../README.md) ・ [日本語](README.ja.md)

Every page below exists in English and Japanese, and the switch is at the top of
each one.

If you are here to use the program, start with [the GUI guide](user-guide/gui.md).
If you are here to understand how it works, start with
[the algorithm](technical/algorithm.md) — and in particular with
[the pitfalls](technical/algorithm.md#pitfalls), the eight reasons why "just cut
on GOP boundaries and concatenate" does not work.

## User Guide

| | |
|---|---|
| [GUI](user-guide/gui.md) | A walkthrough of every screen, with screenshots: adding recordings, cutting, output settings, writing the files |
| [Commercial detection](user-guide/cm-detection.md) | How the commercial breaks are found, how accurate it is, and what to do when it gets one wrong |
| [Projects](user-guide/projects.md) | Saving a night's work to a `.scproj` and picking it up later |
| [Batch processing](user-guide/batch.md) | Handling a whole evening of recordings at once: the clip list, the background queues, and the export |

## Technical

| | |
|---|---|
| [Algorithm](technical/algorithm.md) | How a cut is split into head, body and tail — and the eight pitfalls that make it harder than it looks |
| [Validation](technical/validation.md) | Frame-hash verification results, testing against real broadcast recordings, and the known limits |
| [Broadcast TS](technical/broadcast-ts.md) | PID layout, the recording's own tables, captions and programme information, partial transport streams, ADTS, L-SMASH and DGIndex |
| [Audio](technical/audio.md) | Smart rendering applied to audio, boundary error, MPEG-2 AAC framing, downmixing, choosing the output codec, and multi-track broadcasts |

## Developers

| | |
|---|---|
| [Rust core](developers/rust-core.md) | Timestamp generation, mixed SPS/PPS, and where the Rust implementation overtook the Python one |
| [Design](developers/design.md) | Why a Rust core with a Tauri GUI, and how the GUI is built: the filmstrip, the seek index, the proxy, playback, and the two languages |
| [Building](developers/building.md) | Required libraries, how to build, how to run the tests |
| [Distribution](developers/distribution.md) | AppImage, tar.gz and deb, the Windows installer, and what each one bundles |
| [Reading a Blu-ray](developers/disc.md) | BDAV and BDMV, from a folder or an `.iso`: UDF, ARIB text, one row per clip, and the chooser |
