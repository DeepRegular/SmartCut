# gui

English ・ [日本語](README.ja.md)

The SmartCut GUI, written with Tauri v2 and vanilla JS. It has two windows: a
list window with three screens (the clips, the output settings and the export),
and a cut editor that opens in a window of its own.

For how to use the GUI, see
[`docs/user-guide/gui.md`](../docs/user-guide/gui.md); for how it is built, see
[`docs/technical/design.md`](../docs/technical/design.md). Build instructions
are in [`docs/technical/building.md`](../docs/technical/building.md), and the
release artifacts are covered in
[`docs/technical/distribution.md`](../docs/technical/distribution.md).

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
cd src-tauri && cargo tauri build       # -> target/release/smartcut
./build-windows.sh                      # cross-build for Windows
```
