# gui

[English](README.md) ・ 日本語

SmartCut の GUI。Tauri v2 + バニラ JS。入力設定・出力設定・出力の 3 画面を持つ
一覧ウィンドウと、そこから開くカット編集ウィンドウの 2 つ。

操作の説明は [`docs/user-guide/gui.ja.md`](../docs/user-guide/gui.ja.md)、
実装の説明は [`docs/developers/design.ja.md`](../docs/developers/design.ja.md) に
ある。ビルド手順は
[`docs/developers/building.ja.md`](../docs/developers/building.ja.md)、配布物の
作り方は
[`docs/developers/distribution.ja.md`](../docs/developers/distribution.ja.md)。

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
cd src-tauri && cargo tauri build       # -> target/release/smartcut
./build-windows.sh                      # Windows 版のクロスビルド
```
