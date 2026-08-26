#!/usr/bin/env bash
# Audio checks for the Rust cutter.
#
# A steady tone proves nothing about sync, so the fixture is silence with a
# 2 ms impulse every 0.5 s. Where those impulses land in the output is a
# direct, sample-level measurement of A/V alignment.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-audio-out"
mkdir -p "$OUT"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

if [ ! -f "$FX/clicks.mp4" ]; then
  echo "generating click fixture ..."
  mkdir -p "$FX"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=30" \
    -f lavfi -i "aevalsrc=if(lt(mod(t\,0.5)\,0.002)\,0.9\,0):s=48000:d=30" -shortest \
    -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -bf 3 -b:v 1500k \
    -profile:v high -level 3.1 -pix_fmt yuv420p -c:a aac -b:a 192k "$FX/clicks.mp4"
fi

pass=0; fail=0
t() {
  local name=$1 ranges=$2; shift 2
  local out="$OUT/$(echo "$name" | tr ' ' '_').mp4"
  if ! "$BIN" "$FX/clicks.mp4" "$@" ${SMARTCUT_AUDIO:+--audio-mode "$SMARTCUT_AUDIO"} \
       -o "$out" >/dev/null 2>&1; then
    printf "  FAIL  %-24s cutter error\n" "$name"; fail=$((fail+1)); return
  fi
  local res
  res=$(OUT="$out" SRC="$FX/clicks.mp4" RANGES="$ranges" python3 tests/audio_sync.py)
  if [[ "$res" == OK* ]]; then
    printf "  ok    %-24s %s\n" "$name" "${res#OK|}"; pass=$((pass+1))
  else
    printf "  FAIL  %-24s %s\n" "$name" "${res#BAD|}"; fail=$((fail+1))
  fi
}

echo "running audio tests ...${SMARTCUT_AUDIO:+ (audio: $SMARTCUT_AUDIO)}"
t "single range"   "5.3-12.7"            --keep 5.3-12.7
t "keyframe-exact" "6.0-12.0"            --keep 6.0-12.0
t "multi range"    "1.5-4.2,10.0-18.5"   --keep 1.5-4.2 --keep 10.0-18.5
t "cut middle"     "0.0-8.0,20.0-30.0"   --cut  8.0-20.0
# three ranges with awkward, non-frame-aligned edges: if the boundary error
# accumulated, the last range would be well past half a frame out
t "three ranges"   "1.3-5.7,9.1-14.3,21.7-27.9" --keep 1.3-5.7 --keep 9.1-14.3 --keep 21.7-27.9
echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
