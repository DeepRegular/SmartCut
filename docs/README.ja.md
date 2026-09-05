# 技術ドキュメント

[← SmartCut](../README.ja.md) ・ [English](README.md)

以下の各ページには英語版と日本語版があり、切り替えは各ページの冒頭にあります。

使い方を知りたい場合は [GUI ガイド](user-guide/gui.ja.md)から読んでください。
仕組みを知りたい場合は[アルゴリズム](technical/algorithm.ja.md)から、とくに
「GOP 単位で切って繋ぐだけ」では済まない 8 つの理由を挙げた
[実装上の難所](technical/algorithm.ja.md#実装上の難所)から読むのがおすすめです。

## ユーザーガイド

| | |
|---|---|
| [GUI](user-guide/gui.ja.md) | 全画面の操作手順（スクリーンショット付き）。録画の追加、カット、出力設定、書き出しまで |
| [CM 検出](user-guide/cm-detection.ja.md) | CM の切れ目をどう見つけているか、精度はどのくらいか、外したときにどうするか |
| [プロジェクト](user-guide/projects.ja.md) | 一晩ぶんの作業を `.scproj` に保存し、あとから再開する |
| [バッチ処理](user-guide/batch.ja.md) | 一晩ぶんの録画をまとめて扱う。クリップ一覧、裏で動くキュー、書き出し |

## 技術解説

| | |
|---|---|
| [アルゴリズム](technical/algorithm.ja.md) | カットを head / body / tail に切り分ける原理と、見た目より難しくしている 8 つの落とし穴 |
| [検証](technical/validation.ja.md) | フレームハッシュ照合の結果、実際の放送録画での検証、既知の制限 |
| [放送 TS](technical/broadcast-ts.ja.md) | PID 配置、録画自身のテーブル、字幕と番組情報、部分 TS、ADTS、L-SMASH と DGIndex |
| [音声](technical/audio.ja.md) | 音声へのスマートレンダリング適用、境界誤差、MPEG-2 AAC のフレーミング、ダウンミックス、出力コーデックの選択、音声多重放送 |

## 開発者向け

| | |
|---|---|
| [Rust コア](developers/rust-core.ja.md) | タイムスタンプの生成、SPS/PPS 混在の解決、Rust 実装が Python を追い越した点 |
| [設計](developers/design.ja.md) | なぜ Rust コア + Tauri GUI なのか。GUI の作り: フィルムストリップ、シーク用インデックス、プロキシ、再生、多言語対応 |
| [ビルド](developers/building.ja.md) | 必要なライブラリ、ビルド方法、テストの走らせ方 |
| [配布](developers/distribution.ja.md) | AppImage・tar.gz・deb、Windows インストーラ、それぞれが何を同梱しているか |
| [Blu-ray を読む](developers/disc.ja.md) | BDAV も BDMV も、フォルダーからも `.iso` からも読む。UDF、ARIB のテキスト、クリップごとの一覧、選択ダイアログ |
