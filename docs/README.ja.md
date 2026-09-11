# ドキュメント

[← SmartCut](../README.ja.md) ・ [English](README.md)

ドキュメントは 2 つに分かれています。

- **[ユーザーガイド](#ユーザーガイド)** — 使い方。専門知識は要りません。
- **[技術資料](#技術資料)** — 中で何をしているか。仕組みを知りたい人と、
  コードを触る人のためのものです。

各ページに日本語版と英語版があります。切り替えは各ページの冒頭にあります。

---

## ユーザーガイド

はじめて使うなら [GUI の使い方](user-guide/gui.ja.md)から読んでください。

| | |
|---|---|
| [GUI の使い方](user-guide/gui.ja.md) | 全画面の操作手順（スクリーンショット付き）。録画の追加からカット、出力設定、書き出しまで |
| [CM 検出](user-guide/cm-detection.ja.md) | CM の切れ目を自動で探す。どのくらい当たるか、外れたときはどうするか |
| [まとめて処理する](user-guide/batch.ja.md) | 一晩ぶんの録画をまとめて片付ける。裏で動く処理、複製、まとめて書き出し |
| [プロジェクト](user-guide/projects.ja.md) | 作業を `.scproj` に保存して、あとから再開する |
| [コマンドラインで使う](user-guide/cli.ja.md) | `smartcut` コマンドの全オプション |

---

## 技術資料

仕組みを知りたい人は[アルゴリズム](technical/algorithm.ja.md)から、
とくに[実装上の難所](technical/algorithm.ja.md#実装上の難所)がおすすめです。
「GOP 単位で切って繋ぐだけ」では済まない理由を 10 個挙げてあります。

### 仕組みと検証

| | |
|---|---|
| [アルゴリズム](technical/algorithm.ja.md) | カットを head / body / tail に切り分ける原理と、見た目より難しくしている 10 個の落とし穴 |
| [検証](technical/validation.ja.md) | フレームハッシュ照合の結果、実際の放送録画での検証、既知の制限 |
| [音声](technical/audio.ja.md) | 音声へのスマートレンダリングの適用、境界の誤差、MPEG-2 AAC のフレーミング、ダウンミックス、出力コーデックの選択、音声多重放送 |
| [放送 TS](technical/broadcast-ts.ja.md) | PID 配置、録画自身のテーブル、字幕と番組情報、部分 TS、ADTS、L-SMASH と DGIndex |
| [CM 検出の実装](technical/cm-detection.ja.md) | 検出器のスコアリング、ロゴ検出、字幕リセット、精度をどう測ったか |

### ディスク

| | |
|---|---|
| [ディスクを読む](technical/disc.ja.md) | Blu-ray（BDAV / BDMV）と DVD-Video を、フォルダーからも `.iso` からも読む。UDF、IFO のテーブル、ARIB のテキスト、選択ダイアログ |
| [ディスクを書く](technical/bdav.ja.md) | 一晩ぶんのカットを BDAV フォルダーとして書き出す。到着時刻、エントリーポイントマップ、番組名の出どころ、意味が分からないので実物から写した欄 |

### 実装

| | |
|---|---|
| [Rust コア](technical/rust-core.ja.md) | タイムスタンプの生成、SPS/PPS 混在の解決、libavcodec に無いので自作した VC-1 エンコーダ、Rust 実装が Python 実装を追い越した点 |
| [設計ノート](technical/design.ja.md) | なぜ Rust コア + Tauri GUI なのか。GUI の作り（フィルムストリップ、シーク用インデックス、プロキシ、再生、多言語対応） |
| [ビルド](technical/building.ja.md) | 必要なライブラリ、ビルド方法、テストの走らせ方 |
| [配布](technical/distribution.ja.md) | AppImage・tar.gz・deb と Windows インストーラ、それぞれが何を同梱しているか |
