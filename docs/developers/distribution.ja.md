# 配布

[← ドキュメント](../README.ja.md) ・ [← SmartCut](../../README.ja.md) ・ [English](distribution.md)

## AppImage

```bash
cargo install tauri-cli --version ^2 --locked   # 初回のみ
cd gui/src-tauri && NO_STRIP=1 cargo tauri build --bundles appimage
# -> target/release/bundle/appimage/SmartCut_0.3.2_amd64.AppImage
```

成果物は **185 MB で、共有ライブラリ 745 個をすべて抱えている**。WebKitGTK 4.1 も、
`libavcodec` / `libavformat` / `libavutil` / `libavfilter` / `libswscale` /
`libswresample` も入っているので、**動かす側に ffmpeg は不要**である。SmartCut は
システムの FFmpeg 7.1 に動的リンクしているので、それを同梱できるかどうかが、そもそも
配布できるかどうかの全問題だった。linuxdeploy は `ldd` を辿って拾ってくれる。

| 条件 | 値 |
|---|---|
| 必要な glibc | **2.39 以上**（Ubuntu 24.04 / Debian 13 / Fedora 40 以降） |
| FUSE | 必要（または `--appimage-extract-and-run`） |
| ALSA | `libasound.so.2`（同梱しない。後述） |
| ビルド環境 | Debian 13、glibc 2.41 |

同梱できない唯一のものが glibc であり（AppImage に内在する制約）、それが下限を
決めている。

### なぜ `NO_STRIP=1` を付けるのか

付けないと、以前は linuxdeploy が同梱するライブラリごとに `Strip call failed` で落ちて
いた（`failed to run linuxdeploy`）。**2026-08-28 の時点で再現しなくなっており**、
素のビルドでも通る。

**そして成果物のサイズはどちらでも同じである。** v0.1.1 を両方の条件でビルドしたところ、
どちらも 184,515,064 バイトだった（md5 が違うのは squashfs のタイムスタンプによるもので、
strip が削れるバイトは無い）。Debian の共有ライブラリはもとから strip 済みである。
払う代償が無いので `NO_STRIP=1` は残してある。

### ALSA は同梱しない

音声再生（cpal）を入れたことで、バイナリが `libasound.so.2` と `libjack.so.0` を
dlopen ではなく **DT_NEEDED で直接**必要とするようになった。どちらも linuxdeploy が
参照する AppImage の除外リストに載っているので、`ldd` を辿る同梱の対象から外れ、
**動作中のシステム側のものが使われる**。

libasound2 はデスクトップ Linux ならほぼ確実に入っているし、ALSA を抱え込むと環境間で
かえって壊れやすくなる傾向があるので、除外のままにしてある。何が同梱されているかは
展開して確認できる。

```bash
./SmartCut_0.3.2_amd64.AppImage --appimage-extract >/dev/null
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

音声再生を入れたあとのビルドでは、AppImage 自体で `mpeg2.ts` を開き（無劣化点 41 個、
720x480、29.97 fps、音声あり）、サムネイル 41 枚を走査し、`Space` で再生した。再生は
4 秒で 117 フレーム進み、ALSA 関係のエラーも panic も出なかった。

## tar.gz と deb

```bash
./gui/build-linux.sh
# -> gui/src-tauri/target/release/bundle/linux/SmartCut-0.3.2-linux-x86_64.tar.gz
# -> gui/src-tauri/target/release/bundle/linux/smartcut_0.3.2_amd64.deb
```

同じビルドの詰め方が 2 通りある。どちらも **GUI を `smartcut`、コマンドライン版を
`smartcut-cli`** としてインストールする。プログラム名は SmartCut、打つのは
`smartcut` である。

cargo のクレート名は `gui` なので、放っておくと Tauri はそのまま `/usr/bin/gui` に
インストールする。誰かが占有してよい名前ではない。`tauri.conf.json` の
`mainBinaryName` で `smartcut` に固定してある（0.2.0 以降。それ以前は Windows 用
だけに設定されていた）。一方 Tauri が書き出すバンドル*ファイル*の名前は `productName`
に従うので `SmartCut_0.3.2_amd64.deb` のようになる。deb のパッケージ名 `smartcut` と
食い違うのはそのためである。`build-linux.sh` は両方を `tauri.conf.json` から読む。

| 成果物 | サイズ | FFmpeg | 必要条件 |
|---|---|---|---|
| `SmartCut-0.3.2-linux-x86_64.tar.gz` | 208.7 MB | 同梱 | glibc 2.39 以上。FUSE 不要 |
| `smartcut_0.3.2_amd64.deb` | 3.0 MB | システムのものを使用 | FFmpeg 7.1（Debian 13 / Ubuntu 25.04 以降） |

**tar.gz の中身は AppImage と同じ AppDir を展開したものである。** linuxdeploy が
`ldd` を辿って集めた 745 個のライブラリがそのまま `app/` にあり、`./smartcut` は
AppRun を呼ぶ 4 行のスクリプト、`./smartcut-cli` は `LD_LIBRARY_PATH` を
`app/usr/lib` に向けて CLI を呼ぶ。AppImage が動く環境ならどこでも動き、FUSE を
気にする必要が無くなる。gzip なので、squashfs+zstd の AppImage より 24 MB 大きい。

**逆に deb は何も抱えていない。** 依存関係は両方のバイナリを `dpkg-shlibdeps` に
かけて生成しているので、libav* が列挙される。

```
Depends: libasound2t64 (>= 1.0.29), libavcodec61 (>= 7:7.1.5), libavdevice61 (>= 7:7.1.5),
 libavformat61 (>= 7:7.1.5), libavutil59 (>= 7:7.1.5), libc6 (>= 2.39), libcairo2 (>= 1.10.0),
 libdbus-1-3 (>= 1.10), libgcc-s1 (>= 4.2), libgdk-pixbuf-2.0-0 (>= 2.36.9),
 libglib2.0-0t64 (>= 2.66.0), libgtk-3-0t64 (>= 3.21.5), libjavascriptcoregtk-4.1-0,
 libsoup-3.0-0 (>= 3.0.3), libswresample5 (>= 7:7.1.5), libswscale8 (>= 7:7.1.5),
 libwebkit2gtk-4.1-0 (>= 2.41.90)
```

Tauri 自身の deb は `libwebkit2gtk-4.1-0, libgtk-3-0` の 2 つで終わっており、
**FFmpeg が依存関係に一切現れない**。それだけでも作り直す理由になる。ほかに
`.desktop` ファイル（`Exec=smartcut %f`、`StartupWMClass=smartcut`、MPEG-2 TS と MP4 の
MimeType）、32/128/256 の hicolor アイコン、`copyright`、`changelog.Debian.gz` を
追加している。

**バンドルごとにバイナリの取得元が違う。** Tauri は詰める直前にバンドル種別を
バイナリへ刻印する（`UNKNOWN` → `DEB` / `APPIMAGE`）ので、
`target/release/smartcut` には最後にビルドしたバンドルの刻印しか残らない。deb 用の
バイナリは Tauri の deb から、tar.gz 用は AppDir から取っている。

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
- 唯一試していないのは実際の `dpkg -i` である。VM の sudo にパスワードが要るため。

## Windows

Linux の開発 VM から `x86_64-pc-windows-msvc` へクロスビルドしている。

```bash
./gui/build-windows.sh
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/SmartCut_0.3.2_x64-setup.exe
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/portable/smartcut-portable-x64.zip
```

| 成果物 | サイズ | 内容 |
|---|---|---|
| NSIS インストーラ | 53.1 MB | インストール後 169.5 MB（exe 11.2 MB ＋ FFmpeg の DLL 8 個） |
| ポータブル zip | 66.3 MB | 同じ一式。展開して `smartcut.exe` を実行する |

**移植のために書き直す必要があったコードは 1 か所だけ、音声出力である。** ほかは
すべて libav を通るので `Command::new` も POSIX パスも無い。必要だったのは
*リンク先の FFmpeg* と、`tauri.windows.conf.json` およびビルドスクリプトの追加だけで
ある。ただしサウンドカードは libav の向こう側にあり、そこでは現地の作法に従う必要が
あった（「詰まった点」を参照）。

### FFmpeg をどこから持ってくるか

`ffmpeg-sys-next` は `FFMPEG_DIR` 以下の `include/` と `lib/*.lib` を見る。gyan の
shared ビルドは MSVC 形式のインポートライブラリと DLL の両方を含んでいるので、展開して
そこを指せばよい。

**7.1 系である必要がある。** VM のシステム FFmpeg は 7.1.5 で、`ffmpeg-sys-next
7.1.3` はバージョンでバインディングを選ぶので、系列が違うと API が食い違う。ところが
上流は 8.1 が出た時点で 7.1 の Windows ビルドの配布をやめており、BtbN にも gyan の
サイトにも 8.1 と 9.0 しか無い。gyan の GitHub リリース（`GyanD/codexffmpeg`）には
まだ 7.1.1 があるので、そこから取っている。

DLL は 8 個必要である。`avfilter` が `postproc` を開くので、exe のインポートテーブルに
名前が現れなくても `postproc-58.dll` が要る。

### 動かす側に必要なもの

| 項目 | 状況 |
|---|---|
| FFmpeg | **不要**（DLL を exe と一緒に配布している） |
| VC++ 再頒布可能パッケージ | **不要。** exe が引く C ランタイムは UCRT（`api-ms-win-crt-*`）だけで、Windows 10 以降に標準で入っている |
| WebView2 ランタイム | 必要。Windows 11 には標準、Windows 10 でも Edge 経由でほぼ入っている。無い場合、NSIS インストーラは既定でブートストラッパを取得する（ポータブル zip は利用者任せ） |
| アーキテクチャ | x64 のみ |

### 検証

VM 上の wine 10.0 で確認している。

- **CLI の出力は Linux 版と 1 バイトも違わない。** 同じ `mpeg2.ts` に対する
  `--cut 5-10` が md5 まで一致する。索引（アクセスポイント 41 個、オープン GOP
  39 個）も同一で、境界の部分 GOP に対する再エンコード 0.3% も同じである。なお
  **ポータブル zip の `smartcut.exe` は GUI** なので、CLI は別途ビルドする必要が
  ある。

  ```bash
  cd rust && FFMPEG_DIR=~/win-deps/ffmpeg-7.1.1-full_build-shared \
    XWIN_ACCEPT_LICENSE=1 cargo xwin build --release \
    --target x86_64-pc-windows-msvc -p smartcut-cli
  ```

  GUI と同じ DLL を exe の隣に置く。現状、FFmpeg の DLL が解決できることを確認する
  経路は事実上これだけである（次の項目を参照）。

- **GUI が wine で起動しなくなった**（2026-08-27 時点）。`tao` の
  `event_loop.rs:709` で `assertion failed: subclass_result.as_bool()` に当たる。
  `SetWindowSubclass` が失敗している。これは WebView2 の初期化より前なので、以前
  ここに書いていた「1180x800 のウィンドウが出て『WebView2 ランタイムが見つかりません』で
  止まる」という状態には到達しない。

  **これはビルドの退行ではない。** 公開済みの v0.1.0 の exe（Releases のポータブル
  zip）も、同じ wine で同じ行で panic する。原因は wine 側（comctl32 のサブクラス化）に
  ある。したがって「GUI を WebView2 まで到達させることで DLL の解決を確認する」という
  論法（`DISPLAY` 無しで AppImage を起動して GTK の初期化まで到達させるのと同じ論法）は
  当面使えず、その代役が CLI である。CLI は同じ DLL 一式を読み込み、Linux と md5 まで
  一致する。

wine で確認できるのはここまでである。**実機の Windows で動かして最初に出た不具合は、
プレビューの音が出ないことだった**（後述）。wine の WASAPI はどんな形式も受け付けるので、
上の検証で捕まえられる種類の問題ではない。

インストーラのペイロードと単体の exe はちょうど 3 バイト違う。Tauri がバンドル種別を
exe に刻印するためで、インストーラ側は `NSIS`、ポータブル zip 側は `UNKNOWN` である。

### 詰まった点

- **Windows でだけプレビューの音が出なかった。** 素材のサンプリングレートと
  チャンネル数を、そのままサウンドカードに要求していた。Linux ではそれで動く。cpal の
  既定出力は ALSA の `default` であり、その実体は `plug` の連鎖で、`plug` はカードが
  できない変換を引き受けるからである。**WASAPI の共有モードは引き受けない。** 共有
  モードは全アプリケーションを 1 つの形式に混ぜるので、`IAudioClient` はその形式でしか
  初期化できず、`IsFormatSupported` は違う形式に対して `S_FALSE` と「いちばん近い形式」を
  返す。cpal はこれを非対応として扱い（`is_format_supported` が `S_FALSE` を
  `Ok(false)` に潰す）、開かないまま `StreamConfigNotSupported` で終わる。

  ミキシング形式はサウンド設定で決まるので、**出力を 44.1kHz にしている PC では
  48kHz の放送が完全に無音**になっていた。5.1ch の放送をステレオ出力へ送った場合も
  同様である。デバイスにミキシング形式を尋ねてそれに合わせ、swresample によるレート
  変換とチャンネルレイアウト変換を追加して修正した（`playback_audio::candidates`）。
  **素材自身の形式は最初の候補として残してある**ので、Linux では従来どおり無変換で
  再生される。固定期間を外した候補も末尾に追加した。ALSA の
  `snd_pcm_hw_params_set_buffer_size` は割り切れないサイズを拒否するので、44.1kHz の
  882 フレームが一部のカードで失敗するためである。

- **`resampling::Context::run` の出力フレームは、入力と同じサンプル数しか持たない。**
  レートを上げる場合（48kHz → 96kHz など）これでは足りず、swresample は溢れたぶんを
  内部に貯める。クラッシュも破綻もしないが、貯まったものは二度と出てこないので、再生を
  続けるかぎり遅延とメモリが増え続ける。現在は出力フレームを
  `入力サンプル数 × 出力レート ÷ 入力レート` で確保している。

- **CRT の静的リンク（`+crt-static`）は動かなかった。** `cargo xwin build` を直接
  呼べば動くが、`cargo tauri build --runner cargo-xwin` を経由すると cargo-xwin が
  フラグを取り違え、静的と動的の CRT ライブラリが混ざったリンク行を生成する
  （`libucrt.lib` が落ちて `strlen` が未定義になる）。環境変数でも
  `.cargo/config.toml` でも解決しない。動的リンクでも VC++ 再頒布可能パッケージへの
  依存は生じないので、これ以上追っていない。

- **既定では `gui.exe` になる。** cargo のバイナリ名がクレート名 `gui` を取るから
  である。`mainBinaryName` で `smartcut.exe` にしている。0.2.0 で
  `tauri.windows.conf.json` から `tauri.conf.json` へ移し、そこで Linux のバイナリ名も
  兼ねるようにした。
