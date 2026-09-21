# ビルド

[← ドキュメント](../README.ja.md) ・ [← SmartCut](../../README.ja.md) ・ [English](building.md)

SmartCut は Linux（Debian 13 / Ubuntu 24.04 以降）でビルドできる。Windows 版も
Linux からクロスビルドしている。

## 必要なもの

**FFmpeg は 7.1 系でなければならない。** `ffmpeg-sys-next` はバージョンに応じて
バインディングを選ぶので、系列が違うと API が食い違う。

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

cargo のクレート名は `gui` で、`tauri.conf.json` の `mainBinaryName` でバイナリ名を
`smartcut` に変えている。そのため、`smartcut` という名前になるのはバンドルビルドの
ときだけである。

AppImage・tar.gz・deb・Windows インストーラの作り方は[配布](distribution.ja.md)に
ある。

## テスト

```bash
bash tests/run_tests.sh               # Python E2E                                      13
bash tests/run_rust_tests.sh          # Rust E2E（コンテナ索引で +11）                   16
bash tests/run_audio_tests.sh         # A/V 同期（copy と reencode で +10）               5
bash tests/run_audio_content_tests.sh # 実素材の音声が正しい位置にあるか                  6
bash tests/run_aac_tests.sh           # 出力の AAC フレームが何でできているか             8
bash tests/run_downmix_tests.sh       # 5.1ch をステレオに畳んだとき各成分がどこへ行くか  9
bash tests/run_audio_codec_tests.sh   # 音声を別のコーデックで書き出せるか               39
bash tests/run_audio_smart_tests.sh   # コーデックごとのスマートレンダリング             24
bash tests/run_audio_format_tests.sh  # 音声のサンプリングレートと量子化ビット数         23
bash tests/run_audio_head_tests.sh    # 冒頭が前の番組になっている録画                   22
bash tests/run_preview_tests.sh       # スクラブで指定した時刻の映像が出るか              7
bash tests/run_index_tests.sh         # 索引が走査と同じ答えを返すか                     33
bash tests/run_proxy_tests.sh         # プロキシが録画の代役になれるか                   22
bash tests/run_scene_tests.sh         # シーン検出と CM 境界の照合                        1
bash tests/run_ts_layout_tests.sh     # TS の出自とシーケンスヘッダ                       5
bash tests/run_broadcast_tests.sh     # 字幕・番組情報・データ放送・音声多重             17
bash tests/run_bilingual_tests.sh     # 遅れて announce される副音声                      6
bash tests/run_cm_tests.sh            # CM 検出と目視の正解との照合                       5
bash tests/run_disc_tests.sh          # BDAV と BDMV をフォルダーと .iso から読む       38
bash tests/run_bdav_tests.sh          # ディスクを書く。索引・イメージ・その中身       72
bash tests/run_udf_tests.sh           # イメージをファイルシステムとして実物と並べる  33
bash tests/run_dvd_tests.sh           # DVD-Video をフォルダーと .iso から読む          23
bash tests/run_bd_audio_tests.sh      # ディスクの音声が書き出せるか                    39
bash tests/run_vc1_tests.sh           # VC-1 エンコーダをデコーダに通す                  4
bash tests/run_vp9_av1_tests.sh       # VP9 と AV1 を継いで、録画のバイトのままか        5
bash tests/run_vfr_tests.sh           # 保持されたフレームが保持されたままか、速い差し込みが残るか  7
bash tests/run_transrate_tests.sh    # 容量に合わせる。同一性・サイズ・代償         15
```

**どのスイートも全件通るのが正常である。** 落ちるものがあれば回帰である。

### フィクスチャ

合成フィクスチャ（H.264 / HEVC / オープン GOP / 29.97 fps / MPEG-2 TS）は
`run_tests.sh` が `/tmp/smartcut-fixtures/` に生成するので、**まずこれを走らせる**。
これを再利用するスイート（`run_rust_tests.sh`、`run_index_tests.sh`、
`run_proxy_tests.sh`）は、フィクスチャが無いとそこで止まる。何も言わずに検査の
半分を飛ばすのではなく、`run tests/run_tests.sh first to generate fixtures` と
表示して止まる。

`run_disc_tests.sh` は `mpeg2.ts` から方言ごとにディスクを 1 枚ずつ丸ごと
組み立てる。ストリームを 192 バイトパケットに詰め直し、`disc_index.py` が索引
ファイルを書き、`genisoimage` でそれぞれを UDF イメージに包む（`genisoimage` が
必要）。

`run_bdav_tests.sh` は逆方向の検査である。同じフィクスチャから録画 2 本のディスクを
書き、SmartCut 自身の読み取り側で開く。そのうえで `bdav_index.py` が、索引の全数値を
対象のストリームと突き合わせる。エントリーポイントマップを 1 点ずつファイルまで
追いかけ、到着時刻を測り、クリップ索引にあるストリームの内訳を確かめる。`~/media` に
実際の放送録画があれば、番組名・チャンネル・放送日時がディスクを往復しても残ることも
確かめる。最後にイメージを検査する。UDF の版ごとに書いて SmartCut で開き直し、
7-Zip が入っていれば、別の実装の読み取り側で展開して元のフォルダーと突き合わせる。

`run_udf_tests.sh` はイメージを逆側から見る。開くディスクとしてではなく、開く前に
整っていなければならないファイルシステムとして見る。`udf_shape.py` は、読み取り側が
通る 3 つの層を報告する。アンカーとその指す記述子、区画とその中のファイル
エントリー、そして各ファイルの中身が実際に置かれている場所である。スイートはどの
イメージでも成り立つ不変条件を確かめる。両端にアンカーがあること、どのファイルも
区画の中にあること、何かの上に何かが書かれていないこと、エクステントが長さの欄を
超えないこと、ストリームが 64 KB クラスタの頭から始まること、そして木構造が元の
フォルダーとファイル単位・バイト単位で一致することである。

そのうえで、`$SMARTCUT_DISCS`（既定は `~/Documents/claude/TMPGEnc`）にある実物の
イメージを書き手ごとに 1 枚ずつ、同じ報告で横に並べる。こちらの流儀がレコーダーや
ライティングソフトとどこで違うかが、再生できないディスクを見る前に分かる。
そのため上の件数は、実行するマシンにあるイメージで決まる。ここでは書き手が 5 つ
揃っていて 33 件、3 つだったときは 28 件だった。並べて出た違いは失敗ではない。
レコーダーは区画を overwritable で書き、アンカーを 3 つ置き、削除済みのファイル
識別子をディレクトリに残す。どれも規格の範囲内である。この比較は早速役に立った。
メタデータ区画の複製を指すエントリーが本体のすぐ隣に書かれていて、クラスタが 1 つ
読めなくなれば両方とも失われる状態になっていたのを、これで見つけた。

`run_dvd_tests.sh` は同じストリームを ffmpeg の `dvd` マルチプレクサで MPEG
プログラムストリームにして DVD を 2 枚組み立てる。1 枚はふつうの DVD で、
タイトルセットのストリームが規格どおり複数ファイルに分かれている。もう 1 枚は
タイトルが 2 回に分けて多重化されていて、途中で時計が振り出しに戻る。
`dvd_index.py` がナビゲーションパック（ffmpeg は提示時刻を 0 のままにする）を
埋め、その周りに `.IFO` のテーブルを書く。

`run_bd_audio_tests.sh` は同じ `mpeg2.ts` から、ディスクで使われるコーデックごとに
クリップを 1 本ずつ組み立てる。16 ビットと 24 ビットの LPCM、DTS、TrueHD、E-AC-3 の
5 つである。これをそれぞれ `.ts`・`.m2ts`・`.mp4` に切り出す。

`run_audio_codec_tests.sh` は、出力設定画面で選べる 4 つのコーデック（AAC・AC-3・
DTS・LPCM）をそれぞれ 4 種類のコンテナへ書き出し、3 つのことを確かめる。指定した
コーデックになっているか、各チャンネルの音が入力どおりか、そしてトランスポート
ストリームの番組マップが実際に入っているコーデックを宣言しているかである。
加えて、書けない指定を 2 つ試す。下限を下回る 5.1ch DTS と、DTS に配置の無い
3 チャンネルである。前者は DTS が通常運ばれる値まで上がること、後者は理由を示して
受け付けられないことを確かめる。どちらも `writable_sound` の判定を受けて出力設定
画面で灰色になる組み合わせである。

`run_audio_format_tests.sh` は、サンプルの残る 2 つの側面、サンプリングレート
（どれだけの間隔で取るか）と量子化ビット数（どれだけの幅で書くか）を確かめる。
リサンプルは 3 か所に反映されなければならない。サンプル自体、ストリームの宣言、
そしてトランスポートストリームなら全フレームの ADTS ヘッダである。コーデックが
対応しないレートを指定されたときは、対応するうちで最も近いものに直し、そのことを
表示しなければならない。量子化ビット数はサンプルをそのまま書くコーデックにしか
意味が無いので、LPCM では指定に従い（ファイルの大きさはここで決まる）、ほかの
コーデックでは理由を示して受け付けない。

`run_audio_tests.sh` と `run_downmix_tests.sh` は同じディレクトリに自前の
フィクスチャを作る。インパルス列と、チャンネルごとに音の違う 5.1ch トラックである。
後者はコーデックのスイートとフォーマットのスイートも使うが、ほかのスイートは
どちらも使わない。

各スイートは、出力をフィクスチャと同じ場所、つまり `$TMPDIR` の下に書く。プロキシの
スイートは実素材で数 GB を使う（放送 TS 30 分分のプロキシ 1 つで 2.3 GB）ので、
小さな `/tmp` の tmpfs では足りない。`No space left on device` で落ちる場合は、
`TMPDIR` をディスク上のディレクトリに向ける。

```bash
TMPDIR=~/tmp bash tests/run_proxy_tests.sh
```

### 実素材を読むスイート

`run_audio_content_tests.sh`、`run_aac_tests.sh`、`run_preview_tests.sh`、
`run_index_tests.sh`、`run_proxy_tests.sh`、`run_scene_tests.sh`、
`run_cm_tests.sh`、`run_ts_layout_tests.sh`、`run_broadcast_tests.sh` である
（preview・index・proxy の 3 つは合成素材でも走る）。

既定では `~/media` を見る。`SMARTCUT_MEDIA` で変更できる。音声の比較には numpy が
必要で、無い場合は SKIP になる。

`run_vc1_tests.sh` も実素材を読むが、必要なものが特殊なので別に挙げる。VC-1 で
書かれた録画、つまり 2010 年頃までにプレスされた Blu-ray が必要である。
`SMARTCUT_VC1` にその `.m2ts`（あるいはその一部）を渡す。指定が無ければ、
成功扱いにはせずそこで終了する。エンコーダ側の example バイナリも必要で、これは
通常の `cargo build --release` では作られない。

```bash
cd rust && cargo build --release --examples          # -> target/release/examples/vc1enc
SMARTCUT_VC1=~/media/disc.m2ts bash tests/run_vc1_tests.sh
```

4 つのうち 3 つは、その録画のピクチャを粗い・中くらい・細かい量子化ステップで
符号化し、libavcodec で復号して戻ってきたものを測る。残る 1 つは、両端がアクセス
ポイントの間に落ちる 10 秒のカットを行い（head と tail の両方を書くことになる）、
その間のコピー部分が、録画自身のバイト列としてそのまま現れることを確かめる。
冒頭が平坦すぎて測れないディスクのために、参照ピクチャを取る位置は
`SMARTCUT_VC1_AT`（既定は 20 秒）で動かせる。

### 実装を差し替えて結果を比べる

環境変数で実装を差し替え、結果が同じであることを確認できる。

```bash
SMARTCUT_INDEX=container bash tests/run_rust_tests.sh    # 索引をコンテナから取る
SMARTCUT_AUDIO=copy      bash tests/run_audio_tests.sh   # 音声に一切触らない（既定は smart）
SMARTCUT_AUDIO=reencode  bash tests/run_audio_tests.sh   # サンプル精度の音声
SMARTCUT_BYTE_SEEK=0     bash tests/run_preview_tests.sh # タイムスタンプでシークする旧経路
```

コンテナ索引で走らせると、15 件のうち 11 件が対象になる。トランスポートストリームの
4 件は `the container has no seek table for this stream` を表示してスキップされる。
走査という経路があるのは、まさにこのためである。

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

このうち最初の 2 つは、`SMARTCUT_FFMPEG_LOG` と `SMARTCUT_CLEAN_JOINS` とともに
ウィンドウの環境設定にも項目がある。環境変数は起動時の初期値として使われ、
コマンドラインで渡した録画はその値で開かれる。フロントエンドが保存された値を
送った時点で、そちらが優先される。[GUI の使い方](../user-guide/gui.ja.md#環境設定)を参照。

## 実素材での検証

`tests/verify_real.py <src> <out> <ranges>` で、フレーム数・整列・ビット一致率・
タイムライン・インターレース・音声と映像の長さの差をまとめて検査できる。結果は
[検証](validation.ja.md)にある。

## 仮想マシンで動かす

WebKitGTK のコンポジタは GPU の無いマシンでは何も描画せず、最初の描画のあと更新も
しない。見た目はフリーズと区別がつかない。アプリは
`WEBKIT_DISABLE_COMPOSITING_MODE=1` を既定で設定するので、通常は意識しなくてよい。

同じようなフリーズがもう 1 つあり、こちらはテキスト欄をクリックした瞬間に起きる。
GTK の XIM 入力メソッドモジュールが入っていると、`<input>` がフォーカスを取った
時点で WebKitGTK が描画をやめてしまう。プログラム自体は裏で動き続けていて、古い
表示のまま状態だけが変わっていく。ウィンドウを 1px リサイズすると一気に追いつく。
（`xdotool` で操作したときにスクリーンショットが古いままになっていたのは、
`xdotool` のせいではなくこれが原因だった。）`GTK_IM_MODULE` が未設定のとき GTK が
選ぶのが XIM なので、IME を設定していないデスクトップはすべてこれに当たる。そこで
アプリは既定で `GTK_IM_MODULE=gtk-im-context-simple` を設定している。この
モジュールでは日本語を入力できないので、IME が使えるマシンでは `GTK_IM_MODULE` を
使いたい IME（`fcitx`、`ibus` など）に設定すればよい。明示的に設定されていれば、
アプリはこの変数を変更しない。
