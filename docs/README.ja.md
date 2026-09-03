# ドキュメント

[← SmartCut](../README.ja.md) ・ [English](README.md)

## 仕組み

| | |
|---|---|
| [アルゴリズムと実装上の難所](algorithm.ja.md) | head / body / tail の切り分けと、「GOP 単位で切って繋ぐだけ」では済まない 8 つの理由 |
| [Rust コアの実装](rust-core.ja.md) | タイムスタンプの生成、SPS/PPS 混在の解決、音声境界の扱い、5.1 のダウンミックス、複数音声の全トラック出力 |
| [検証結果と既知の制限](validation.ja.md) | フレームハッシュ照合の結果、実放送録画での検証、原理的な制約 |

## 機能

| | |
|---|---|
| [GUI](gui.ja.md) | クリップ一覧とカット編集画面、フィルムストリップ、シーク用インデックス、サムネイル軌道とシーン検出、再生、プロキシ、プロジェクト、2 言語対応 |
| [CM 境界の検出](cm-detection.ja.md) | 字幕リセット・無音・ロゴの 3 つ、15 秒格子、誤検出を出さないための設計 |
| [放送録画ワークフローとの互換](broadcast-ts.ja.md) | 出力 TS の PID レイアウト、放送自身のテーブル、字幕と番組情報、シーケンスヘッダ、ADTS、L-SMASH / DGIndex |

## 作る・配る

| | |
|---|---|
| [ビルドと開発](development.ja.md) | 必要なライブラリ、ビルド手順、テストの走らせ方 |
| [配布](distribution.ja.md) | AppImage・tar.gz・deb と Windows インストーラ、同梱される依存 |

## そのほか

| | |
|---|---|
| [BDMV / BDAV への拡張](bdmv.ja.md) | 調査結果と段階的な作業量 |
| [移植方針](design.ja.md) | Rust コア + Tauri GUI を選んだ理由、ライセンスと特許 |
