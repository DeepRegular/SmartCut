# gui

[English](README.md) ・ 日本語

SmartCut の GUI。Tauri v2 + バニラ JS。入力設定・出力設定・出力の 3 画面を持つ
一覧ウィンドウと、そこから開くカット編集ウィンドウの 2 つ。

設計と操作の説明は [`docs/gui.ja.md`](../docs/gui.ja.md) にある。ビルド手順は
[`docs/development.ja.md`](../docs/development.ja.md)、配布物の作り方は
[`docs/distribution.ja.md`](../docs/distribution.ja.md)。

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
cd src-tauri && cargo tauri build       # -> target/release/smartcut
./build-windows.sh                      # Windows 版のクロスビルド
```
