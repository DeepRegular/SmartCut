# gui

English ・ [日本語](README.ja.md)

The smartcut cut editor. Tauri v2 plus vanilla JS.

The design and the controls are described in [`docs/gui.md`](../docs/gui.md).
Build instructions are in [`docs/development.md`](../docs/development.md), and
how the release artifacts are produced is in
[`docs/distribution.md`](../docs/distribution.md).

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
./build-windows.sh                      # cross-build for Windows
```
