# gui

English ・ [日本語](README.ja.md)

The SmartCut GUI. Tauri v2 plus vanilla JS. A list window carrying three
screens — the clips, the output settings, the output — and a cut editor that
opens in a window of its own.

The design and the controls are described in [`docs/gui.md`](../docs/gui.md).
Build instructions are in [`docs/development.md`](../docs/development.md), and
how the release artifacts are produced is in
[`docs/distribution.md`](../docs/distribution.md).

```bash
cd src-tauri && cargo build --release   # -> target/release/smartcut
./build-windows.sh                      # cross-build for Windows
```
