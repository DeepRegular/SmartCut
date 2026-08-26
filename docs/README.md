# ドキュメント

[← smartcut](../README.ja.md)

## 仕組み

| | |
|---|---|
| [アルゴリズムと実装上の難所](algorithm.md) | head / body / tail の切り分けと、「GOP 単位で切って繋ぐだけ」では済まない 8 つの理由 |
| [Rust コアの実装](rust-core.md) | タイムスタンプの生成、SPS/PPS 混在の解決、音声境界の扱い |
| [検証結果と既知の制限](validation.md) | フレームハッシュ照合の結果、実放送録画での検証、原理的な制約 |

## 機能

| | |
|---|---|
| [GUI](gui.md) | カット編集画面、フィルムストリップ、サムネイル軌道とシーン検出、再生 |
| [CM 境界の検出](cm-detection.md) | 字幕リセット・無音・ロゴの 3 つ、15 秒格子、誤検出を出さないための設計 |
| [放送録画ワークフローとの互換](broadcast-ts.md) | 出力 TS の PID レイアウト、シーケンスヘッダ、ADTS、L-SMASH / DGIndex |

## 作る・配る

| | |
|---|---|
| [ビルドと開発](development.md) | 必要なライブラリ、ビルド手順、テストの走らせ方 |
| [配布](distribution.md) | AppImage と Windows インストーラ、同梱される依存 |

## そのほか

| | |
|---|---|
| [BDMV / BDAV への拡張](bdmv.md) | 調査結果と段階的な作業量 |
| [移植方針](design.md) | Rust コア + Tauri GUI を選んだ理由、ライセンスと特許 |
