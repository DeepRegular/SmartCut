# Documentation

[← smartcut](../README.md) ・ [日本語](README.ja.md)

## How it works

| | |
|---|---|
| [Algorithm and pitfalls](algorithm.md) | How head / body / tail are split, and the eight reasons "just cut on GOP boundaries and join" is not enough |
| [The Rust core](rust-core.md) | Generating timestamps, resolving mixed SPS/PPS, handling audio boundaries |
| [Validation and known limits](validation.md) | Frame-hash verification results, testing against real broadcast recordings, the limits inherent in the approach |

## Features

| | |
|---|---|
| [GUI](gui.md) | The cut editor, the filmstrip, proxy editing, the thumbnail track and scene detection, playback |
| [Commercial boundary detection](cm-detection.md) | Subtitle resets, silence and logo presence, the 15-second grid, and the design that keeps false positives out |
| [Broadcast workflow compatibility](broadcast-ts.md) | PID layout of the output TS, sequence headers, ADTS, L-SMASH / DGIndex |

## Building and shipping

| | |
|---|---|
| [Building and development](development.md) | Required libraries, how to build, how to run the tests |
| [Distribution](distribution.md) | AppImage, tar.gz and deb, the Windows installer, and the bundled dependencies |

## Also

| | |
|---|---|
| [Extending to BDMV / BDAV](bdmv.md) | Research notes and the work involved, stage by stage |
| [Design notes](design.md) | Why a Rust core plus a Tauri GUI, and the licence and patent situation |
