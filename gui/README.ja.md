# gui

[English](README.md) ・ 日本語

SmartCut の GUI です。Tauri v2 + バニラ JS で書かれています。ウィンドウは
2 つあります。入力設定・出力設定・出力の 3 画面を持つ一覧ウィンドウと、そこから
開くカット編集ウィンドウです。

操作方法は [`docs/user-guide/gui.ja.md`](../docs/user-guide/gui.ja.md)、実装の
解説は [`docs/technical/design.ja.md`](../docs/technical/design.ja.md) に
あります。ビルド手順は
[`docs/technical/building.ja.md`](../docs/technical/building.ja.md)、配布物の
作り方は
[`docs/technical/distribution.ja.md`](../docs/technical/distribution.ja.md) に
あります。

```bash
cd src-tauri && cargo build --release   # -> target/release/gui
cd src-tauri && cargo tauri build       # -> target/release/smartcut
./build-windows.sh                      # Windows 版のクロスビルド
```
