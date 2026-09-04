# ビルド

[← ドキュメント](../README.ja.md) ・ [← SmartCut](../../README.ja.md) ・ [English](building.md)

SmartCut は Linux（Debian 13 / Ubuntu 24.04 以降）でビルドできる。Windows 版も
Linux からクロスビルドしている。

## 必要なもの

**FFmpeg は 7.1 系である必要がある。** `ffmpeg-sys-next` はバージョンでバインディングを
選ぶので、系列が違うと API が食い違う。

```bash
# ビルドの基本一式（bindgen が clang / libclang を使う）
sudo apt install build-essential pkg-config cmake clang libclang-dev

# FFmpeg 7.1 の開発ヘッダ
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
                 libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev

# Tauri GUI の前提（コアだけをビルドするなら不要）
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
                 patchelf desktop-file-utils xdg-utils

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

| 項目 | バージョン |
|---|---|
| FFmpeg | 7.1 系（7.1.5 で開発） |
| Rust | 1.98 以降 |
| WebKitGTK | 4.1（Tauri v2） |
| Node.js | 24 LTS（GUI のバンドルに使用） |

## ビルド

```bash
# Rust コアと CLI
cd rust && cargo build --release
# -> rust/target/release/smartcut

# GUI
cd gui/src-tauri && cargo build --release
# -> gui/src-tauri/target/release/gui

# 配布時の名前で GUI をビルドする
cd gui/src-tauri && cargo tauri build
# -> gui/src-tauri/target/release/smartcut
```

cargo のクレート名は `gui` であり、バイナリ名を変えているのは `tauri.conf.json` の
`mainBinaryName` である。そのため `smartcut` という名前はバンドルビルドのときにだけ
現れる。

AppImage・tar.gz・deb・Windows インストーラの作り方は[配布](distribution.ja.md)に
ある。

## テスト

```bash
bash tests/run_tests.sh               # Python E2E                                      13
bash tests/run_rust_tests.sh          # Rust E2E（コンテナ索引で +9）                    13
bash tests/run_audio_tests.sh         # A/V 同期（copy と reencode で +10）               5
bash tests/run_audio_content_tests.sh # 実素材の音声が正しい位置にあるか                  6
bash tests/run_aac_tests.sh           # 出力の AAC フレームが何でできているか             8
bash tests/run_downmix_tests.sh       # 5.1ch をステレオに畳んだとき各成分がどこへ行くか  9
bash tests/run_preview_tests.sh       # スクラブで指定した時刻の絵が出るか                7
bash tests/run_index_tests.sh         # 索引が走査と同じ答えを返すか                     27
bash tests/run_proxy_tests.sh         # プロキシが録画の代役になれるか                   22
bash tests/run_scene_tests.sh         # シーン検出と CM 境界の照合                        1
bash tests/run_ts_layout_tests.sh     # TS の出自とシーケンスヘッダ                       5
bash tests/run_broadcast_tests.sh     # 字幕・番組情報・音声多重                         13
bash tests/run_cm_tests.sh            # CM 検出と人間の答えの照合                         5
bash tests/run_bdav_tests.sh          # BDAV ディスクをフォルダーと .iso から読む       12
```

### フィクスチャ

合成フィクスチャ（H.264 / HEVC / オープン GOP / 29.97 fps / MPEG-2 TS）は
`run_tests.sh` が `/tmp/smartcut-fixtures/` に生成するので、**まずこれを走らせる**。
これを再利用するスイート（`run_rust_tests.sh`、`run_index_tests.sh`、
`run_proxy_tests.sh`）は、黙って検査の半分を飛ばすのではなく
`run tests/run_tests.sh first to generate fixtures` と表示して止まる。

`run_bdav_tests.sh` は `mpeg2.ts` から BDAV ディスクを丸ごと組み立てる。ストリームを
192 バイトパケットに詰め直し、索引ファイルを書き、`genisoimage` で UDF イメージに
包む（`genisoimage` が要る）。

`run_audio_tests.sh` と `run_downmix_tests.sh` は同じディレクトリに自前の
フィクスチャを作る。インパルス列と、チャンネルごとに音の違う 5.1ch トラックであり、
どちらもほかのスイートは使わない。

各スイートは出力をフィクスチャの隣、つまり `$TMPDIR` の下に書く。プロキシのスイートは
実素材では数 GB を必要とする（放送 TS 30 分ぶんのプロキシ 1 つで 2.3 GB）。小さな
`/tmp` の tmpfs では足りない。`No space left on device` で落ちる場合は `TMPDIR` を
ディスク上のディレクトリに向ける。

```bash
TMPDIR=~/tmp bash tests/run_proxy_tests.sh
```

### 実素材を読むスイート

`run_audio_content_tests.sh`、`run_aac_tests.sh`、`run_preview_tests.sh`、
`run_index_tests.sh`、`run_proxy_tests.sh`、`run_scene_tests.sh`、`run_cm_tests.sh`、
`run_ts_layout_tests.sh`、`run_broadcast_tests.sh` である（preview・index・proxy の
3 つは合成素材でも走る）。

既定では `~/media` を見る。`SMARTCUT_MEDIA` で変更できる。音声の比較には numpy が
必要で、無い場合は SKIP になる。

### 実装を差し替えて結果を比べる

環境変数で実装を差し替え、結果が同じであることを確認できる。

```bash
SMARTCUT_INDEX=container bash tests/run_rust_tests.sh    # 索引をコンテナから取る
SMARTCUT_AUDIO=copy      bash tests/run_audio_tests.sh   # 音声に一切触らない（既定は smart）
SMARTCUT_AUDIO=reencode  bash tests/run_audio_tests.sh   # サンプル精度の音声
SMARTCUT_BYTE_SEEK=0     bash tests/run_preview_tests.sh # タイムスタンプでシークする旧経路
```

## 環境変数

[シーク用インデックス](design.ja.md#シーク用インデックスseek_indexrs)は CLI からも
作れる。同じパスを 2 回渡すと、2 回目は走査を省略する。

```bash
smartcut rec.ts --seek-index /tmp/rec.scix --scenes
```

| 変数 | 既定 | |
|---|---|---|
| `SMARTCUT_BYTE_SEEK` | on | `0` / `off` で、タイムスタンプを狙って `seek_margin` 秒手前から読み進める旧経路に戻る |

[プロキシ](design.ja.md#プロキシ編集proxyrs)は既定でオフで、同じように調整できる。

| 変数 | 既定 | |
|---|---|---|
| `SMARTCUT_PROXY` | off | `1` / `on` で作成する。プレビュー・ストリップ・再生がそちらから読むようになる |
| `SMARTCUT_PROXY_WIDTH` | `1280` | プロキシの幅（正方画素）。上げるほど見た目はよくなり、作成に時間がかかる。**上限は 1920x1080**。縦長の素材では高さが先に上限に達し、幅はそれに応じて下がる |
| `SMARTCUT_PROXY_QUALITY` | `22` | 品質（x264 の CRF 相当）。小さいほど高品質で、18〜24 が実用範囲 |
| `SMARTCUT_PROXY_ENCODER` | auto | 試すエンコーダをカンマ区切りで指定する（例: `mpeg4`） |

幅と品質は[キャッシュのハッシュに含まれる](design.ja.md#キャッシュ)ので、設定ごとに
別のプロキシが作られる。

## 実素材での検証

`tests/verify_real.py <src> <out> <ranges>` が、フレーム数・整列・ビット一致率・
タイムライン・インターレース・A/V の尺差をまとめて検査する。結果は
[検証](../technical/validation.ja.md)にある。

## 仮想マシンで動かす

WebKitGTK のコンポジタは GPU の無いマシンでは何も描画せず、最初の描画のあと更新も
しない。見た目はフリーズと区別がつかない。アプリは
`WEBKIT_DISABLE_COMPOSITING_MODE=1` を既定で設定するので、通常は意識しなくてよい。

同じフリーズがもう一口あり、こちらはテキスト欄をクリックするまで待っている。GTK の XIM
入力メソッドモジュールが入っていると、`<input>` がフォーカスを取った瞬間に WebKitGTK が
描画をやめる。プログラムは裏で動き続けていて、古い絵の裏で画面は変わっている。ウィンドウを
1px リサイズすると一気に追いつく。（`xdotool` で操作したときにスクリーンショットが古いまま
になっていたのは `xdotool` のせいではなく、これである。）`GTK_IM_MODULE` が未設定のときに
GTK が落ちてくる先が XIM で、それは IME を設定していないデスクトップすべてに当たる。そこで
アプリは既定で `GTK_IM_MODULE=gtk-im-context-simple` を設定する。このモジュールでは日本語は
打てないので、IME が使えるマシンでは `GTK_IM_MODULE` をそれに設定すればよい（`fcitx`、
`ibus` など）。明示されていればアプリは触らない。
