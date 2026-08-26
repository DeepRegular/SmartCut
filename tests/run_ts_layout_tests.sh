#!/usr/bin/env bash
# What the cut looks like from outside the elementary streams.
#
# A recording carries its programme on particular PIDs, listed in a PMT on its
# own PID, under a service number, with descriptors saying what and in which
# language. Tools built around broadcast recordings -- DGIndex and its like --
# read all of that before they read a single frame, so a cut that renumbers it
# from scratch is a cut some of them will not open.
#
# This suite exists because two ways of getting that wrong were found in one
# afternoon: muxer options that were never reaching the muxer at all, and a
# PID copied from the wrong stream. Neither showed up in a frame comparison.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
WORK="${TMPDIR:-/tmp}/smartcut-ts-layout"
mkdir -p "$WORK"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0

# `head` rather than the whole file: the tables repeat every few hundred
# milliseconds, so the opening seconds say everything.
layout() { head -c 20000000 "$1" > "$WORK/slice.ts"; python3 tests/ts_layout.py "$WORK/slice.ts"; }
field() { echo "$1" | sed -n "s/^$2=//p"; }

check() {
  local name=$1 src="$MEDIA/$2"
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  "$BIN" "$src" --keep 20.0-60.0 -o "$WORK/out.ts" >/dev/null 2>&1
  if [ ! -s "$WORK/out.ts" ]; then
    printf "  FAIL  %-26s 出力が空\n" "$name"; fail=$((fail+1)); return
  fi
  local a b
  a=$(layout "$src"); b=$(layout "$WORK/out.ts")
  local why=""
  [ "$(field "$a" pmt_pid)"   = "$(field "$b" pmt_pid)" ]   || why="$why PMT($(field "$a" pmt_pid)→$(field "$b" pmt_pid))"
  [ "$(field "$a" video_pid)" = "$(field "$b" video_pid)" ] || why="$why 映像PID($(field "$a" video_pid)→$(field "$b" video_pid))"
  # the audio need not keep its exact PID -- the muxer lays its streams out in
  # a run -- but it must still be declared as what it is, and wrapped as audio
  [ "$(field "$a" audio_stream_type)" = "$(field "$b" audio_stream_type)" ] || why="$why 音声stream_type"
  [ "$(field "$a" audio_stream_id)"   = "$(field "$b" audio_stream_id)" ]   || why="$why 音声PES stream_id"
  if [ -z "$why" ]; then
    printf "  ok    %-26s PMT 0x%x / 映像 0x%x / 音声 stream_type 0x%x stream_id 0x%x\n" \
      "$name" "$(field "$b" pmt_pid)" "$(field "$b" video_pid)" \
      "$(field "$b" audio_stream_type)" "$(field "$b" audio_stream_id)"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s 素材と違う:%s\n" "$name" "$why"; fail=$((fail+1))
  fi
  rm -f "$WORK/out.ts"
}

echo "TS 出力が録画のレイアウトを引き継ぐか"
check "地デジ 日本海テレビ"  full_ntv.ts
check "BS フジ"             bsfuji.ts
check "地デジ NHK E"        terrestrial_nhke.ts

# The sequence header is the other thing read before any decoding: an indexer
# takes the *first* one it meets and believes it for the whole file. A cut
# beginning off an access point re-encodes its opening pictures, and that
# header is written by us -- so it is the one place a 29.97 recording can be
# handed on as something else.
seq() {
  local name=$1 src="$MEDIA/$2"
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  # 20.3s is deliberately not an access point, forcing a re-encoded opening
  "$BIN" "$src" --keep 20.3-60.0 -o "$WORK/out.ts" >/dev/null 2>&1
  ffmpeg -v error -y -i "$src"          -map 0:v:0 -c copy -t 3 "$WORK/a.m2v" 2>/dev/null
  ffmpeg -v error -y -i "$WORK/out.ts"  -map 0:v:0 -c copy -t 3 "$WORK/b.m2v" 2>/dev/null
  local a b
  a=$(python3 tests/seq_header.py "$WORK/a.m2v"); b=$(python3 tests/seq_header.py "$WORK/b.m2v")
  local why=""
  for k in width height frame_rate_code aspect; do
    [ "$(field "$a" $k)" = "$(field "$b" $k)" ] || why="$why $k($(field "$a" $k)→$(field "$b" $k))"
  done
  if [ -z "$why" ]; then
    printf "  ok    %-26s %sx%s %s\n" "$name" "$(field "$b" width)" "$(field "$b" height)" "$(field "$b" frame_rate)"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s 素材と違う:%s\n" "$name" "$why"; fail=$((fail+1))
  fi
  rm -f "$WORK/out.ts" "$WORK/a.m2v" "$WORK/b.m2v"
}

echo
echo "再エンコードした先頭のシーケンスヘッダが素材と同じか"
seq "BS フジ"             bsfuji.ts
seq "地デジ 日本海テレビ"  full_ntv.ts

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
