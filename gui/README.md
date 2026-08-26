# gui

smartcut のカット編集 GUI。Tauri v2 + バニラ JS。

設計と操作の説明は [`docs/gui.md`](../docs/gui.md) にある。ビルド手順は
[`docs/development.md`](../docs/development.md)、配布物の作り方は
[`docs/distribution.md`](../docs/distribution.md)。

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
./build-windows.sh                      # Windows 版のクロスビルド
```
