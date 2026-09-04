# gui

English ・ [日本語](README.ja.md)

The SmartCut GUI. Tauri v2 plus vanilla JS. A list window carrying three
screens — the clips, the output settings, the output — and a cut editor that
opens in a window of its own.

How the GUI is used is described in
[`docs/user-guide/gui.md`](../docs/user-guide/gui.md), and how it is built in
[`docs/developers/design.md`](../docs/developers/design.md). Build instructions
are in [`docs/developers/building.md`](../docs/developers/building.md), and how
the release artifacts are produced is in
[`docs/developers/distribution.md`](../docs/developers/distribution.md).

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
cd src-tauri && cargo tauri build       # -> target/release/smartcut
./build-windows.sh                      # cross-build for Windows
```
