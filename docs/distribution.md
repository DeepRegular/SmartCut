# 配布

[← ドキュメント一覧](README.md) ・ [← smartcut](../README.ja.md)

## 配布（AppImage）

```bash
cargo install tauri-cli --version ^2 --locked   # 一度だけ
cd gui/src-tauri && cargo tauri build --bundles appimage
# -> target/release/bundle/appimage/smartcut_0.1.0_amd64.AppImage
```

**184MB。745 個の共有ライブラリを丸ごと抱えている**——WebKitGTK 4.1 も、
`libavcodec` / `libavformat` / `libavutil` / `libavfilter` / `libswscale` /
`libswresample` も入っているので、**動かす側に ffmpeg を入れる必要はない**。
このツールはシステムの FFmpeg 7.1 に動的リンクしているため、そこを同梱できるか
どうかが配布可否そのものだった（linuxdeploy が `ldd` を辿って拾ってくれる）。

| 条件 | 値 |
|---|---|
| 必要な glibc | **2.39 以上**（Ubuntu 24.04 / Debian 13 / Fedora 40 以降） |
| 必要な FUSE | あり（無ければ `--appimage-extract-and-run`） |
| 必要な ALSA | `libasound.so.2`（同梱されない。下記） |
| ビルド環境 | Debian 13、glibc 2.41 |

glibc だけは同梱できない（AppImage の原理的な制約）ので、そこが下限になる。

**ALSA は同梱されない。** 音声再生（cpal）を入れたことで、バイナリが
`libasound.so.2` と `libjack.so.0` を dlopen ではなく **DT_NEEDED で直接**
要求するようになった。ところがこの 2 つは linuxdeploy が参照する AppImage の
除外リストに載っているため、`ldd` を辿る同梱の対象から外れ、**実行側の
システムのものを使う**。libasound2 はデスクトップ Linux ならまず入っている
うえ、ALSA を抱き込むと逆に環境差で壊れやすくなるので、除外はそのままに
してある。同梱されているかどうかは展開して確かめられる：

```bash
./smartcut_0.1.0_amd64.AppImage --appimage-extract >/dev/null
ldd squashfs-root/usr/bin/gui | grep -E 'asound|jack|pulse'
# libasound.so.2 / libjack.so.0 -> /lib/x86_64-linux-gnu/...   (システム側)
# libpulse.so.0                 -> squashfs-root/usr/bin/../lib/...  (同梱)
```

動作は AppImage の実体で確認済み：素材を開き、走査（266 枚 / シーン 55 箇所）、
`Ctrl+D` の CM 検出、カット、無劣化バッジの切り替わりまで通っている。
ライブラリ解決の確認は別のマシンでも取れる——`DISPLAY` を外して起動すると、
「GTK を初期化できない」まで**到達して**落ちる。ライブラリが欠けていれば
そこまで行けないので、これが通れば依存は足りている。

音声再生を入れた後のビルドでも、AppImage の実体で `mpeg2.ts` を開き
（無劣化点 41 / 720x480 / 29.97fps / 音声あり）、サムネイル 41 枚の走査と
`Space` の再生まで確認した。再生は 4 秒で 117 フレーム進み、ALSA まわりの
エラーもパニックも出ない。

## 配布（Windows）

Linux の開発 VM から `x86_64-pc-windows-msvc` へクロスビルドする。

```bash
./gui/build-windows.sh
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/smartcut_0.1.0_x64-setup.exe
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/portable/smartcut-portable-x64.zip
```

| 成果物 | サイズ | 中身 |
|---|---|---|
| NSIS インストーラ | 52.6MB | 展開後 167MB（exe 9.6MB + FFmpeg DLL 8 個） |
| 可搬 zip | 65.7MB | 同じ一式。解凍して `smartcut.exe` を叩くだけ |

**移植のために書き換えたコードは、音声出力の 1 箇所だけ。** コードは全部
libav 越しなので `Command::new` も POSIX パスも出てこない。要ったのは
*リンクする FFmpeg の実体*だけで、追加したのは `tauri.windows.conf.json` と
ビルドスクリプトの 2 つ。ただしサウンドカードだけは libav の向こう側にあり、
そこは移植先の作法に合わせる必要があった（下記「詰まった点」）。

### FFmpeg をどこから持ってくるか

`ffmpeg-sys-next` は `FFMPEG_DIR` の `include/` と `lib/*.lib` を見る。gyan の
shared ビルドは MSVC 形式のインポートライブラリと DLL の両方を同梱しているので、
展開してそこを指すだけで済む。

**7.1 系であることが条件。** VM の system FFmpeg が 7.1.5 で、
`ffmpeg-sys-next 7.1.3` はバージョンごとにバインディングを出し分けるため、
別系列を掴ませると API がずれる。ところが upstream は 8.1 が出た時点で 7.1 の
Windows ビルド配布を打ち切っていて、BtbN も gyan の本家サイトも 8.1 / 9.0 しか
置いていない。gyan の GitHub リリース（`GyanD/codexffmpeg`）に 7.1.1 が
残っているので、そこから取る。

DLL は 8 個。`avfilter` が `postproc` を開くので、exe のインポートテーブルに
名前が出てこない `postproc-58.dll` も要る。

### 動かす側に必要なもの

| 項目 | 状況 |
|---|---|
| FFmpeg | **不要**（DLL を exe と同じ場所に置いて配る） |
| VC++ 再頒布可能パッケージ | **不要**。exe が引く C ランタイムは UCRT（`api-ms-win-crt-*`）だけで、これは Windows 10 以降に入っている |
| WebView2 ランタイム | 必要。Windows 11 は標準搭載、Windows 10 も Edge 経由でほぼ入っている。無ければ NSIS インストーラが既定でブートストラッパを取りに行く（可搬 zip は自分で入れる） |
| アーキテクチャ | x64 のみ |

### 検証

VM 上の wine 10.0 で確認した。

- **CLI の出力が Linux ビルドと 1 バイトも違わない** — 同じ `mpeg2.ts` を
  `--cut 5-10` した結果が md5 まで一致する。索引（41 アクセスポイント・
  39 オープン GOP）も、境界の部分 GOP を再エンコードした 0.3% も同一。
  ただし**可搬 zip に入っている `smartcut.exe` は GUI のほう**なので、CLI は
  別に建てる:

  ```bash
  cd rust && FFMPEG_DIR=~/win-deps/ffmpeg-7.1.1-full_build-shared \
    XWIN_ACCEPT_LICENSE=1 cargo xwin build --release \
    --target x86_64-pc-windows-msvc -p smartcut-cli
  ```

  DLL は GUI 側と同じものを exe の隣に置く。いま **FFmpeg DLL が解決できて
  いることを確かめられるのは、実質この経路だけ**になっている（次項）。
- **GUI は wine 上では起動しなくなった**（2026-08-27 現在）。`tao` の
  `event_loop.rs:709` で `assertion failed: subclass_result.as_bool()` ——
  `SetWindowSubclass` が失敗している。WebView2 の初期化より手前なので、
  以前ここに書いていた「1180x800 のウィンドウが出て『Could not find the
  WebView2 Runtime』で止まる」には**もう到達しない**。

  **ビルドの退行ではない。** すでに公開した v0.1.0 の exe（Releases の可搬
  zip）を同じ wine で叩いても、同じ行で同じパニックになる。原因は wine 側
  （comctl32 のサブクラス化）にある。そのぶん「GUI を WebView2 まで到達させて
  DLL 解決を確かめる」という論法——AppImage を `DISPLAY` 無しで起動して GTK の
  初期化まで到達させるのと同じ論法——は今は使えず、同じ DLL 一式を読む CLI が
  Linux と md5 一致するところで代替している。

wine で取れるのはここまで。**実機の Windows で動かして最初に出たのが、
プレビューの音が鳴らないという不具合だった**（下記「詰まった点」）。wine の
WASAPI はどんな形式でも受けるので、ここまでの検証では踏めない類のもの。

インストーラの中身と単体 exe の差は 3 バイトしかない。Tauri がバンドル種別を
exe に焼くためで、インストーラ側が `NSIS`、可搬 zip 側が `UNKNOWN` になる。

### 詰まった点

- **プレビューの音が Windows だけ鳴らない。** 素材のサンプルレートと
  チャンネル数をそのままサウンドカードに要求していた。Linux ではそれで通る
  ——cpal の既定出力は ALSA の `default`、その実体は `plug` の連鎖で、カードが
  できない変換は `plug` が引き受ける。**WASAPI の共有モードは引き受けない。**
  共有モードは全アプリケーションを 1 つの形式で混ぜるので、`IAudioClient` は
  その形式でしか初期化できず、`IsFormatSupported` は違う形式に対して
  `S_FALSE` と「最も近い形式」を返す。cpal はこれを対応なしとして扱う
  （`is_format_supported` が `S_FALSE` を `Ok(false)` に落とす）ので、
  `StreamConfigNotSupported` で開けずに終わる。混ぜる形式はサウンド設定の
  とおりなので、**出力を 44.1kHz にしている PC では 48kHz の放送が丸ごと
  無音**になり、ステレオ出力に 5.1ch の放送を出しても同じことになる。
  デバイスに「何で混ぜているか」を訊いてそこへ合わせ、レートとチャンネル
  配置の変換を swresample に足して直した（`playback_audio::candidates`）。
  **素材の形式を第一候補に残してある**ので、Linux 側は今までどおり無変換で
  鳴る。ついでに固定ピリオドを外した候補も末尾に足した——ALSA の
  `snd_pcm_hw_params_set_buffer_size` は割り切れないサイズを拒むので、
  44.1kHz の 882 フレームは通らないカードがある。
- **`resampling::Context::run` の出力フレームは入力と同じサンプル数しかない。**
  レートを上げる向き（48kHz → 96kHz など）では足りず、溢れたぶんは
  swresample が内部に溜め込む。落ちも壊れもしないが、溜まったものは二度と
  出てこないので、再生が続くかぎり遅延とメモリが伸びていく。出力フレームは
  こちらで `input.samples() × 出力レート ÷ 入力レート` を確保するようにした。
- **CRT の静的リンク（`+crt-static`）は通らなかった。** `cargo xwin build` を
  直に叩けば通るのに、`cargo tauri build --runner cargo-xwin` 経由だと
  cargo-xwin がフラグを取り違えて、静的 CRT と動的 CRT のライブラリが混ざった
  リンク行になる（`libucrt.lib` が落ちて `strlen` が未定義になる）。環境変数と
  `.cargo/config.toml` のどちらに書いても駄目。動的リンクのままでも VC++
  再頒布パッケージ依存は出ていないので、これ以上は追っていない。
- **既定では `gui.exe` になる。** cargo のバイナリ名がクレート名 `gui` だから。
  `tauri.windows.conf.json` に `mainBinaryName` を書いて `smartcut.exe` にした。
  Windows 用の設定ファイルなので、Linux 側のバンドルには影響しない。
