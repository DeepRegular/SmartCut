# 配布

[← ドキュメント](../README.ja.md) ・ [← SmartCut](../../README.ja.md) ・ [English](distribution.md)

## AppImage

```bash
cargo install tauri-cli --version ^2 --locked   # 初回のみ
cd gui/src-tauri && NO_STRIP=1 cargo tauri build --bundles appimage
# -> target/release/bundle/appimage/SmartCut_0.8.2_amd64.AppImage
```

成果物は 186.2 MB で、共有ライブラリ 716 個をすべて同梱している。WebKitGTK 4.1 も、
`libavcodec` / `libavformat` / `libavutil` / `libavfilter` / `libswscale` /
`libswresample` も入っているので、**動かす側に ffmpeg を入れる必要はない**。
SmartCut はシステムの FFmpeg 7.1 に動的リンクしているので、配布できるかどうかは
これを同梱できるかで決まった。ライブラリは linuxdeploy が `ldd` を辿って集める。

| 条件 | 値 |
|---|---|
| 必要な glibc | 2.39 以上（Ubuntu 24.04 / Debian 13 / Fedora 40 以降） |
| FUSE | 必要（または `--appimage-extract-and-run`） |
| ALSA | `libasound.so.2`（同梱しない。後述） |
| ビルド環境 | Debian 13、glibc 2.41 |

AppImage の仕組み上 glibc だけは同梱できないので、glibc のバージョンが下限になる。

### なぜ `NO_STRIP=1` を付けるのか

付けないと、以前は linuxdeploy が同梱するライブラリごとに `Strip call failed` で
落ちていた（`failed to run linuxdeploy`）。2026-08-28 の時点では再現しなくなって
おり、素のビルドでも通る。

成果物のサイズはどちらでも同じである。v0.1.1 を両方の条件でビルドしたところ、
どちらも 184,515,064 バイトだった（md5 が違うのは squashfs のタイムスタンプによる
もので、strip が削れるバイトは無い）。Debian の共有ライブラリはもとから strip 済み
である。**付けておいて損は無い**ので、`NO_STRIP=1` は残してある。

### ALSA は同梱しない

音声再生（cpal）を入れたことで、バイナリが `libasound.so.2` と `libjack.so.0` を
dlopen ではなく DT_NEEDED で直接参照するようになった。どちらも linuxdeploy が
参照する AppImage の除外リストに載っているので、`ldd` を辿る同梱の対象から外れ、
**動作中のシステム側のものが使われる**。

libasound2 はデスクトップ Linux ならまず入っているし、ALSA を抱え込むと環境を
またいだときにかえって壊れやすいので、除外のままにしてある。何が同梱されているかは
展開して確認できる。

```bash
./SmartCut_0.8.2_amd64.AppImage --appimage-extract >/dev/null
ldd squashfs-root/usr/bin/smartcut | grep -E 'asound|jack|pulse'
# libasound.so.2 / libjack.so.0 -> /lib/x86_64-linux-gnu/...   (システム側)
# libpulse.so.0                 -> squashfs-root/usr/bin/../lib/...  (同梱)
```

### 確認したこと

AppImage 自体で動作を確認している。素材を開く、走査する（サムネイル 266 枚、シーン
55 点）、`Ctrl+D` で CM 検出、カット、無劣化バッジの切り替わり、いずれも動く。

ライブラリの解決は別のマシンでも確認できる。`DISPLAY` を外して起動すると
「GTK を初期化できない」まで到達して落ちる。ライブラリが足りなければもっと手前で
止まるので、そこまで進めば依存関係は満たされている。

音声再生を入れたあとのビルドでは、AppImage 自体で `mpeg2.ts` を開き（無劣化点
41 個、720x480、29.97 fps、音声あり）、サムネイル 41 枚を走査し、`Space` で
再生した。再生は 4 秒で 117 フレーム進み、ALSA 関係のエラーも panic も出なかった。

## tar.gz と deb

```bash
./gui/build-linux.sh
# -> gui/src-tauri/target/release/bundle/linux/SmartCut-0.8.2-linux-x86_64.tar.gz
# -> gui/src-tauri/target/release/bundle/linux/smartcut_0.8.2_amd64.deb
```

同じビルドを 2 通りに詰めたものである。どちらも GUI を `smartcut`、コマンド
ライン版を `smartcut-cli` という名前でインストールする。プログラム名は SmartCut
で、コマンドとして打つのは `smartcut` である。

cargo のクレート名は `gui` なので、放っておくと Tauri はそのまま `/usr/bin/gui` に
インストールしてしまう。1 つのアプリが占有してよい名前ではない。`tauri.conf.json` の
`mainBinaryName` で `smartcut` に固定してある（0.2.0 以降。それ以前は Windows 用
だけに設定されていた）。一方 Tauri が書き出すバンドル*ファイル*の名前は
`productName` に従うので、`SmartCut_0.8.2_amd64.deb` になる。deb のパッケージ名
`smartcut` と食い違うのはこのためである。`build-linux.sh` は両方を
`tauri.conf.json` から読む。

| 成果物 | サイズ | FFmpeg | 必要条件 |
|---|---|---|---|
| `SmartCut-0.8.2-linux-x86_64.tar.gz` | 202.4 MB | 同梱 | glibc 2.39 以上。FUSE 不要 |
| `smartcut_0.8.2_amd64.deb` | 4.8 MB | システムのものを使用 | FFmpeg 7.1（Debian 13 / Ubuntu 25.04 以降） |

tar.gz の中身は、AppImage と同じ AppDir を展開したものである。linuxdeploy が
`ldd` を辿って集めた 716 個のライブラリがそのまま `app/` にある。`./smartcut` は
AppRun を呼ぶ 4 行のスクリプトで、`./smartcut-cli` は `LD_LIBRARY_PATH` を
`app/usr/lib` に向けて CLI を呼ぶ。AppImage が動く環境ならどこでも動き、FUSE は
不要である。gzip なので、squashfs+zstd の AppImage より 16 MB 大きい。

### 同じライブラリが 3 つ入っていた

linuxdeploy は `ldd` が挙げた名前をそのまま複製する。`libfoo.so` と
`libfoo.so.0` は -dev パッケージが持つ `libfoo.so.0.1.2` へのシンボリックリンク
なので、実体をたどって 3 つとも実ファイルとして入っていた。librsvg なら 6.2 MB が
3 回である。合計 19.4 MB、524 MB のうちの 3.7% になる。

ローダーが開くのは SONAME の 1 つだけで、残り 2 つは名前でしかない。そこで
`build-linux.sh` は tar に詰める前にシンボリックリンクへ戻す。**gzip 自身では
気付けない。**遡れるのは 32 KB 前までで、複製どうしは数 MB 離れているためである
（AppImage 側に同じ問題が無いのは、squashfs が同一ブロックを共有するからである）。

対象は `usr/lib` の 64 KB 超に限っている。`usr/share/doc` の copyright も重複して
いるが、あるパッケージのライセンスを別のパッケージのものへのリンクに置き換える
のは避けた。

圧縮そのものは `gzip -9` にした。既定の 6 との差は 0.9 MB で、かかる時間は 1 分
である。zstd や xz にすればさらに 40〜55 MB 小さくなるが（実測で `zstd -19` が
163 MB、`xz -9` が 146 MB）、展開に必要なものが増えるので gzip のままにしてある。

deb のほうは何も同梱していない。依存関係は両方のバイナリを `dpkg-shlibdeps` に
かけて生成しているので、libav* が列挙される。

```
Depends: libasound2t64 (>= 1.0.29), libavcodec61 (>= 7:7.1.5), libavdevice61 (>= 7:7.1.5),
 libavformat61 (>= 7:7.1.5), libavutil59 (>= 7:7.1.5), libc6 (>= 2.39), libcairo2 (>= 1.10.0),
 libdbus-1-3 (>= 1.10), libgcc-s1 (>= 4.2), libgdk-pixbuf-2.0-0 (>= 2.36.9),
 libglib2.0-0t64 (>= 2.66.0), libgtk-3-0t64 (>= 3.21.5), libjavascriptcoregtk-4.1-0,
 libsoup-3.0-0 (>= 3.0.3), libswresample5 (>= 7:7.1.5), libswscale8 (>= 7:7.1.5),
 libwebkit2gtk-4.1-0 (>= 2.41.90)
```

Tauri が生成する deb の依存関係は `libwebkit2gtk-4.1-0, libgtk-3-0` の 2 つだけで、
**FFmpeg がまったく現れない**。それだけでも自前で作り直す理由になる。ほかに
追加しているのは、`.desktop` ファイル（`Exec=smartcut %f`、
`StartupWMClass=smartcut`、MPEG-2 TS と MP4 の MimeType）、32/128/256 の hicolor
アイコン、`copyright`、`changelog.Debian.gz` である。

Tauri は詰める直前にバンドル種別をバイナリへ刻印する（`UNKNOWN` → `DEB` /
`APPIMAGE`）ので、`target/release/smartcut` には最後にビルドしたバンドルの刻印しか
残らない。そのためバイナリはバンドルごとに別の場所から取っている。deb 用は Tauri の
deb から、tar.gz 用は AppDir からである。

### 確認したこと（Debian 13 の開発 VM）

- `smartcut-cli` の出力が、**deb と tar.gz で md5 まで一致する**
  （`mpeg2.ts --cut 5-10`、無劣化コピー 99.7%）。`ldd` で別のライブラリを使っている
  ことも確認済みである。tar.gz 版は `app/usr/lib/libavcodec.so.61`、deb 版は
  `/lib/x86_64-linux-gnu/libavcodec.so.61` を使う。
- どちらの GUI も実機で起動し、`mpeg2.ts` を開ける（無劣化点 41 個、プロキシ
  852x478、サムネイル 41 枚）。deb 版はタスクバーに `gui` ではなく `smartcut` と
  出る。
- `apt-get -s install ./smartcut_0.1.1_amd64.deb` が依存関係を解決する。
  `desktop-file-validate` は警告なし、`md5sums` の 8 項目もすべて一致する。
- VM の sudo にパスワードが必要なので、実際の `dpkg -i` だけは試していない。

## Windows

Linux の開発 VM から `x86_64-pc-windows-msvc` へクロスビルドしている。

```bash
./gui/build-windows.sh
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/SmartCut_0.8.2_x64-setup.exe
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/portable/smartcut-portable-x64.zip
```

| 成果物 | サイズ | 内容 |
|---|---|---|
| NSIS インストーラ | 55.0 MB | インストール後 176.4 MB（GUI の exe、CLI の exe、FFmpeg の DLL 8 個） |
| ポータブル zip | 68.8 MB | 同じ一式。GUI は `smartcut.exe`、コマンドライン版は `smartcut-cli.exe` を実行する |

**コマンドライン版は、Linux のパッケージと同じ `smartcut-cli` という名前で
入れている。** cargo が作る CLI のバイナリ名は `smartcut` で、Windows では GUI と
同じ名前になってしまう。そこでビルドスクリプトは先に `cargo xwin` で CLI を
ビルドし、`windows-deps/smartcut-cli.exe` として置く。`tauri.windows.conf.json` は
これを DLL と同じくリソースとして指定しているので、インストール先では GUI と
同じフォルダーに入る。ポータブル zip にも同じものを入れる。0.8.0 までは、
どちらにも入っていなかった。

移植のために書き直したコードは、**音声出力の 1 か所だけ**である。ほかはすべて libav
を通るので、`Command::new` も POSIX パスも出てこない。必要だったのはリンク先の
FFmpeg と、`tauri.windows.conf.json` とビルドスクリプトの追加だけだった。ただし
サウンドカードだけは libav の外にあり、そこは Windows の流儀に合わせることになった
（「詰まった点」を参照）。

### FFmpeg をどこから持ってくるか

`ffmpeg-sys-next` は `FFMPEG_DIR` 以下の `include/` と `lib/*.lib` を見る。gyan の
shared ビルドは MSVC 形式のインポートライブラリと DLL の両方を含んでいるので、
展開してそこを指せばよい。

**7.1 系でなければならない。** VM のシステム FFmpeg は 7.1.5 で、`ffmpeg-sys-next
7.1.3` はバージョンに応じてバインディングを選ぶので、系列が違うと API が食い違う。
ところが上流は 8.1 が出た時点で 7.1 の Windows ビルドの配布をやめており、BtbN にも
gyan のサイトにも 8.1 と 9.0 しか無い。gyan の GitHub リリース
（`GyanD/codexffmpeg`）にはまだ 7.1.1 があるので、そこから取っている。

DLL は 8 個必要である。`avfilter` が `postproc` を開くので、exe のインポート
テーブルに名前が現れなくても `postproc-58.dll` が必要である。

### 動かす側に必要なもの

| 項目 | 状況 |
|---|---|
| FFmpeg | 不要（DLL を exe と一緒に配布している） |
| VC++ 再頒布可能パッケージ | 不要。exe が引く C ランタイムは UCRT（`api-ms-win-crt-*`）だけで、Windows 10 以降に標準で入っている |
| WebView2 ランタイム | 必要。Windows 11 には標準、Windows 10 でも Edge 経由でほぼ入っている。無い場合、NSIS インストーラは既定でブートストラッパを取得する（ポータブル zip は利用者任せ） |
| アーキテクチャ | x64 のみ |

### 検証

VM 上の wine 10.0 で確認している。

- **CLI の出力は Linux 版と 1 バイトも違わない。** 同じ `mpeg2.ts` に対する
  `--cut 5-10` が md5 まで一致する。索引（アクセスポイント 41 個、オープン GOP
  39 個）も同一で、境界の部分 GOP に対する再エンコード 0.3% も同じである。
  確認に使うのは、ポータブル zip を展開したフォルダーの `smartcut-cli.exe` である。
  現状、FFmpeg の DLL が解決できることを確認する方法は事実上これだけである
  （次の項目を参照）。

- GUI は wine で起動しなくなった（2026-08-27 時点）。`tao` の
  `event_loop.rs:709` で `assertion failed: subclass_result.as_bool()` に当たる。
  `SetWindowSubclass` が失敗している。これは WebView2 の初期化より前なので、
  以前ここに書いていた「1180x800 のウィンドウが出て『WebView2 ランタイムが見つかり
  ません』で止まる」という状態には、もう到達しない。

  これはビルドの退行ではない。公開済みの v0.1.0 の exe（Releases のポータブル
  zip）も、同じ wine の同じ行で panic する。原因は wine 側、comctl32 の
  サブクラス化にある。そのため「GUI を WebView2 まで到達させて DLL の解決を
  確認する」という方法は当面使えない。`DISPLAY` 無しで AppImage を起動し、GTK の
  初期化まで到達させるのと同じ発想の方法である。代わりに CLI で確認する。CLI は
  同じ DLL 一式を読み込み、Linux 版と md5 まで一致する。

wine で確認できるのはここまでである。実機の Windows で最初に見つかった不具合は、
プレビューの音が出ないことだった（後述）。wine の WASAPI はどんな形式も
受け付けるので、上の検証では捕まらない。

Tauri がバンドル種別を exe に刻印するので、インストーラのペイロードと単体の exe は
ちょうど 3 バイト違う。インストーラ側は `NSIS`、ポータブル zip 側は `UNKNOWN`
である。

### 詰まった点

- Windows でだけプレビューの音が出なかった。素材のサンプリングレートと
  チャンネル数を、そのままサウンドカードに要求していたためである。Linux でこれが
  動くのは、cpal の既定出力である ALSA の `default` が実体としては `plug` の連鎖で、
  カードにできない変換を `plug` が肩代わりするからである。**WASAPI の共有モードは
  そうした変換をしない。** 共有モードは全アプリケーションを 1 つの形式に混ぜるので、
  `IAudioClient` はその形式でしか初期化できない。違う形式を渡すと、
  `IsFormatSupported` は `S_FALSE` と「いちばん近い形式」を返す。cpal はこれを
  非対応として扱い（`is_format_supported` が `S_FALSE` を `Ok(false)` に潰す）、
  開かないまま `StreamConfigNotSupported` で終わる。

  ミキシング形式はサウンド設定で決まるので、出力を 44.1 kHz にしている PC では
  48 kHz の放送が完全に無音になっていた。5.1ch の放送をステレオ出力へ送った場合も
  同様である。デバイスにミキシング形式を問い合わせてそれに合わせ、swresample による
  レート変換とチャンネルレイアウト変換を追加して修正した
  （`playback_audio::candidates`）。素材自身のレートとサンプル形式は最初の候補として
  残してあるので、Linux ではそのまま再生される。変わるのはチャンネルの並びだけで、
  ALSA の順に並べ替える（[音声](audio.ja.md)を参照）。ALSA の
  `snd_pcm_hw_params_set_buffer_size` は割り切れないサイズを拒否し、44.1 kHz の
  882 フレームが一部のカードで失敗するので、固定期間を外した候補も末尾に追加した。

- `resampling::Context::run` の出力フレームは、サンプル数が入力と同じにしか
  ならない。レートを上げる場合（48 kHz → 96 kHz など）これでは足りず、swresample は
  溢れた分を内部に貯める。クラッシュも破綻もしないが、貯まったものは二度と
  出てこないので、再生を続けるかぎり遅延とメモリが増え続ける。現在は出力フレームを
  `入力サンプル数 × 出力レート ÷ 入力レート` で確保している。

- CRT の静的リンク（`+crt-static`）は動かなかった。`cargo xwin build` を直接
  呼べば動く。ところが `cargo tauri build --runner cargo-xwin` を経由すると、
  cargo-xwin がフラグを取り違え、静的と動的の CRT ライブラリが混ざったリンク行を
  生成する（`libucrt.lib` が落ちて `strlen` が未定義になる）。環境変数でも
  `.cargo/config.toml` でも解決しない。動的リンクでも VC++ 再頒布可能パッケージへの
  依存は生じないので、これ以上追っていない。

- cargo のバイナリ名はクレート名 `gui` になるので、既定では `gui.exe` になる。
  `mainBinaryName` で `smartcut.exe` にしている。0.2.0 で
  `tauri.windows.conf.json` から `tauri.conf.json` へ移し、そこで Linux の
  バイナリ名も兼ねるようにした。
