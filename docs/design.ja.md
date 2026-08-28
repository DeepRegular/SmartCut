# 移植方針

[← ドキュメント一覧](README.ja.md) ・ [← smartcut](../README.ja.md) ・ [English](design.md)

決定: **Rust コア + Tauri GUI**

- コア: `rsmpeg` / `ffmpeg-next`（libavformat / libavcodec バインディング）で
  デマルチプレクス〜パケット選別〜マルチプレクスを自前制御。
  - タイムスタンプを自分で振れるので継ぎ目の問題が原理的に消える
  - `nal_ref_idc` をパケットから直接読めるので #3 のサンプリングが不要
  - 中間ファイルも ffprobe の複数パスも不要になる
- GUI: Tauri（Windows / macOS / Linux、配布サイズが小さい）。
  タイムライン UI は Web 技術で作れる。

この試作は**リファレンス実装兼テストオラクル**として使っている。
`tests/run_tests.sh`（Python）と `tests/run_rust_tests.sh`（Rust）は
同じフレームハッシュ照合を共有する。

## ライセンスと特許（配布するなら先に決める）

- **x264 / x265 は GPL。** リンクするとアプリ全体が GPL になる。
- 回避策は**ハードウェアエンコーダ**（NVENC / QSV / VideoToolbox / AMF）。
  再エンコードするのは部分 GOP だけなので画質面の妥協は小さく、
  ライセンス面はクリーンになる。この試作の `--video-encoder` で切り替え可能。
- H.264 / HEVC の**特許ライセンス**（MPEG LA / Access Advance）は
  商用配布では別途検討が要る。
