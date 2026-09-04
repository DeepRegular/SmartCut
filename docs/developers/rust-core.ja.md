# Rust コア（`rust/`）

[← ドキュメント](../README.ja.md) ・ [← SmartCut](../../README.ja.md) ・ [English](rust-core.md)

配布されているのは Rust 実装である。Python 実装はテストオラクルとして残してある。
「最初のフレームが 13 ミリ秒早い」という制限は、この実装では解消している（後述）。

音声については独立したページがある。[音声](../technical/audio.ja.md)を参照。

## 移植の状況

| 項目 | 状況 |
|---|---|
| アクセスポイント索引と leading picture の解析 | 完了。出力は Python と同一 |
| leading picture の参照判定 | 完了。しかも **Python より正確** |
| planner | 完了。11 ケースで Python と一致 |
| カット（コピー経路） | 完了 |
| カット（再エンコード経路） | 完了 |
| SPS/PPS 混在の解決 | 完了（`avc3` とパラメータセットの再挿入） |
| 音声（コピー） | 完了。同期を実測で確認 |
| 音声（スマートレンダリング） | 完了。境界のフレームだけを焼き直す |
| 音声（再エンコード） | 完了。サンプル精度で、出力は MPEG-2 AAC |

映像については、`tests/run_rust_tests.sh` の 13 ケースが `tests/run_tests.sh`
（Python）と同じ無劣化率に達している。

```
h264 single range      lossless 180/222   first=0.00000 step=0.033333 jitter=0
h264 cut middle        lossless 540/540   first=0.00000 step=0.033333 jitter=0
hevc                   lossless 300/342   first=0.00000 step=0.033333 jitter=0
ntsc 29.97fps          lossless 300/342   first=0.00000 step=0.033367 jitter=0
mpeg2 ts open-GOP      lossless 328/342   first=0.00000 step=0.033367 jitter=0
```

そのうえで、タイムスタンプが全ケースで正確である。Python 版は 13 ミリ秒早く始まる。

## タイムスタンプ問題の解消

Python 版は生のエレメンタリストリームを ffmpeg に渡すしかなく、その結果最初の
フレームが 13 ミリ秒早くなっていた（`irregular=[(0, 0.046667)]`）。

Rust 版は各ピクチャの表示インデックスから、**PTS と DTS を整数ティックで直接
割り当てる**。出力のタイムベースは `1/fps の分子` なので、1 フレームがちょうど
`fps の分母` ティックになり、丸めが一切発生しない。

```
h264 keyframe-exact   lossless 180/180   first=0.00000 step=0.033333 jitter=0
mpeg2 ts open-GOP     lossless 283/283   first=0.00000 step=0.033367 jitter=0
```

`tests/run_rust_tests.sh` で検証している。全ケースがちょうど 0.000 秒から始まり、
ジッタは 0 である。

## SPS/PPS 混在の解決

MP4 の `avcC` はパラメータセットを 1 組しか持てないが、再エンコード部分の SPS は
必ず元ストリームのものと異なる。さらに MP4 は NAL を長さ前置で格納するのに対し、
エンコーダが吐くのは Annex-B である。どちらかを放置すると、映像は
`sps_id 32 out of range` や `Invalid NAL unit size` で崩壊する。

必要なことは 3 つある。

- サンプルエントリを **`avc3` / `hev1`** にして、ストリーム内のパラメータセットを
  許可する。
- エンコーダ出力を Annex-B から長さ前置へ組み直す。
- **コピー部分のキーフレームの前に、元の SPS/PPS を再挿入する。** 再エンコード部分の
  SPS が有効なままだと、コピー部分が誤った SPS で復号される。そのため継ぎ目ごとに
  元のパラメータセットを復元する。

背景は[難所 1](../technical/algorithm.ja.md#1-パラメータセットspsppsが一致しない)に
ある。

## 参照判定が Python より正確になった点

Python はビットストリームを読むために ffmpeg をもう一度起動する必要があり、
**ファイル内の 1 か所を標本にして全体に適用**していた。

Rust ではパケットを走査しているその場で `nal_ref_idc` を読めるので、判定を
**アクセスポイントごとに厳密に**行える。追加のパスも追加のコストも不要である。

## 音声

エンジンの音声側は独立したページに分けてある。内容は次のとおり。

| | |
|---|---|
| [3 つのモード](../technical/audio.ja.md#3-つのモード) | `copy` / `smart` / `reencode` と、それぞれの代償 |
| [区間ごとに切る](../technical/audio.ja.md#区間ごとに切るそして起きかけた累積ずれ) | MP4 の `stts` トラックでずれが累積する理由と、その抑え方 |
| [音声のスマートレンダリング](../technical/audio.ja.md#音声のスマートレンダリング) | またがるフレームだけの焼き直し、ガードフレーム、無音判定 |
| [MPEG-2 AAC で書き出す](../technical/audio.ja.md#mpeg-2-aac-で書き出す--aac) | ADTS ヘッダを多重化器ではなく自前で組む理由 |
| [ダウンミックス](../technical/audio.ja.md#ダウンミックス--audio-channels) | 5.1ch をステレオへ畳む処理と、それが全編再エンコードになる理由 |
| [音声多重放送](../technical/audio.ja.md#音声多重放送) | 各音声トラックを独立に切る |
