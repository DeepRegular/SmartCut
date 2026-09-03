# ビルドと開発

[← ドキュメント一覧](README.ja.md) ・ [← SmartCut](../README.ja.md) ・ [English](development.md)

Linux（Debian 13 / Ubuntu 24.04 以降）でビルドする。Windows 版も Linux から
クロスビルドする。

## 必要なもの

**FFmpeg は 7.1 系であること。** `ffmpeg-sys-next` はバージョンごとに
バインディングを出し分けるので、別系列を掴ませると API がずれる。

```bash
# ビルド基盤（clang / libclang は bindgen が使う）
sudo apt install build-essential pkg-config cmake clang libclang-dev

# FFmpeg 7.1 の開発ヘッダ
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
                 libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev

# Tauri GUI の前提（GUI をビルドしないなら不要）
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
                 patchelf desktop-file-utils xdg-utils

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

| 項目 | 版 |
|---|---|
| FFmpeg | 7.1 系（開発は 7.1.5） |
| Rust | 1.98 以上 |
| WebKitGTK | 4.1（Tauri v2） |
| Node.js | 24 LTS（GUI のバンドルに使う） |

## ビルド

```bash
# Rust コア + CLI
cd rust && cargo build --release
# -> rust/target/release/smartcut

# GUI
cd gui/src-tauri && cargo build --release
# -> gui/src-tauri/target/release/smartcut
```

### 配布物

AppImage・tar.gz・deb / Windows インストーラの作り方は [配布](distribution.ja.md) を参照。

## テスト

```bash
bash tests/run_tests.sh               # Python E2E                     13
bash tests/run_rust_tests.sh          # Rust E2E（+ container 索引で 9） 13
bash tests/run_audio_tests.sh         # A/V 同期（+ copy/reencode で 10）   5
bash tests/run_audio_content_tests.sh # 実素材の音声が正しい位置にあるか   6
bash tests/run_aac_tests.sh           # 出力の AAC フレームそのものを見る   8
bash tests/run_downmix_tests.sh       # 5.1ch を畳んだ音がどこへ行ったか    8
bash tests/run_preview_tests.sh       # スクラブの絵が頼んだ時刻か         7
bash tests/run_index_tests.sh         # 索引が走査と同じ答えを返すか       27
bash tests/run_proxy_tests.sh         # プロキシが録画の代わりになるか     22
bash tests/run_scene_tests.sh         # シーン検出 vs CM 境界             1
bash tests/run_ts_layout_tests.sh     # TS の素性とシーケンスヘッダ         5
bash tests/run_broadcast_tests.sh    # 字幕・番組情報・音声多重                   10
bash tests/run_cm_tests.sh            # CM 検出 vs 目視の正解              5
```

合成フィクスチャ（H.264 / HEVC / オープン GOP / 29.97fps / MPEG-2 TS）は
`run_tests.sh` が `/tmp/smartcut-fixtures/` に生成するので、まずこれを走らせる。
フィクスチャを使い回す側（`run_rust_tests.sh` / `run_index_tests.sh` /
`run_proxy_tests.sh`）は、無ければ検査を黙って飛ばさずに
`run tests/run_tests.sh first to generate fixtures` と言って止まる。
`run_audio_tests.sh` と `run_downmix_tests.sh` は自前のフィクスチャを同じ
ディレクトリに作る——インパルス列と、チャンネルごとに違う純音を入れた 5.1ch。
どちらも他のテストが要らないものである。

各テストの出力もフィクスチャと同じ `$TMPDIR` の下に書かれる。プロキシのテストは
実素材だと数 GB 要る（1 時間の放送 TS のプロキシ 1 本で 2.3 GB）ので、小さな
`/tmp` の tmpfs では足りない。`No space left on device` で落ちるときは `TMPDIR`
をディスク上のディレクトリに向ける:

```bash
TMPDIR=~/tmp bash tests/run_proxy_tests.sh
```

**実素材を読むテスト**は `run_audio_content_tests.sh` / `run_aac_tests.sh` /
`run_preview_tests.sh` /
`run_index_tests.sh`・`run_proxy_tests.sh`（どちらも合成素材でも走る）/
`run_scene_tests.sh` /
`run_cm_tests.sh` / `run_ts_layout_tests.sh`。既定の
置き場は `~/media`、`SMARTCUT_MEDIA` で変えられる。音声の照合には numpy が要る
（無ければ SKIP する）。

環境変数で実装を切り替えて同じ結果になるかを確かめられる:

```bash
SMARTCUT_INDEX=container bash tests/run_rust_tests.sh   # 索引をコンテナ由来に
SMARTCUT_AUDIO=copy      bash tests/run_audio_tests.sh  # 音声を一切触らない（既定は smart）
SMARTCUT_AUDIO=reencode  bash tests/run_audio_tests.sh  # 音声をサンプル精度に
SMARTCUT_BYTE_SEEK=0     bash tests/run_preview_tests.sh # シークを時刻指定に戻す
```

[シーク用インデックス](gui.ja.md#シーク用インデックスseek_indexrs)は
CLI からも作れる。同じパスを二度渡せば、二度目は走査を飛ばす:

```bash
smartcut rec.ts --seek-index /tmp/rec.scix --scenes
```

| 変数 | 既定 | |
|---|---|---|
| `SMARTCUT_BYTE_SEEK` | 有効 | `0` / `off` でバイト位置シークを使わず、時刻を狙って `seek_margin` だけ手前から読み直す旧来の経路に戻す |

[プロキシ](gui.ja.md#プロキシ編集proxyrs)は既定でオフ。環境変数で振れる:

| 変数 | 既定 | |
|---|---|---|
| `SMARTCUT_PROXY` | 無効 | `1` / `on` で作る（プレビュー・ストリップ・再生がそちらから読む） |
| `SMARTCUT_PROXY_WIDTH` | `1280` | プロキシの幅（正方画素）。上げるほど絵は良くなり作成は長くなる。**上限は 1920x1080**（縦が先に当たる縦長素材は幅がそのぶん下がる） |
| `SMARTCUT_PROXY_QUALITY` | `22` | 画質。x264 の CRF で言う（小さいほど良い、18〜24 が実用域） |
| `SMARTCUT_PROXY_ENCODER` | 自動 | 試すエンコーダをカンマ区切りで指定（`mpeg4` など） |

幅と品質は[キャッシュのハッシュに入っている](gui.ja.md#キャッシュ)ので、
振ったぶんだけ別のプロキシが建つ。

## 実素材での検証

`tests/verify_real.py <src> <out> <ranges>` — フレーム数・整列・ビット完全一致率・
タイムライン・インターレース・A/V 長差をまとめて確認する。結果は
[検証](validation.ja.md) を参照。

## 仮想マシンで動かす場合

WebKitGTK のコンポジタが GPU の無い環境では何も描かず、初回描画のあと一切
更新されない——フリーズにしか見えない。アプリ側で
`WEBKIT_DISABLE_COMPOSITING_MODE=1` を既定にしてあるので、通常は意識しなくてよい。

`xdotool` で GUI を機械的に叩く場合は、DOM の更新が X に伝わらずスクリーン
ショットが古くなることがある。ウィンドウを 1px リサイズすれば再描画が起きる。
