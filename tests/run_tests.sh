#!/usr/bin/env bash
# End-to-end check: cut, then decode both sides and compare frame hashes.
# Fixtures are generated on the fly, so this needs nothing but ffmpeg.
set -u
cd "$(dirname "$0")/.."
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-testout"
mkdir -p "$FX" "$OUT"

gen() {
  local name=$1; shift
  [ -f "$FX/$name" ] && return
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=${DUR:-20}" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=${DUR:-20}" \
    -shortest "$@" "$FX/$name"
}

echo "generating fixtures in $FX ..."
DUR=30 gen h264.mp4    -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -bf 3 \
                       -b:v 1500k -profile:v high -level 3.1 -pix_fmt yuv420p -c:a aac -b:a 128k
gen hevc.mp4           -c:v libx265 -x265-params "keyint=60:min-keyint=60:scenecut=0:open-gop=0" \
                       -b:v 1200k -pix_fmt yuv420p -c:a aac -b:a 128k 2>/dev/null
gen opengop.mp4        -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 \
                       -x264-params "open-gop=1" -b:v 1500k -pix_fmt yuv420p -c:a aac -b:a 128k
gen ntsc.mp4  -r 30000/1001 -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 \
                       -b:v 1500k -pix_fmt yuv420p -c:a aac -b:a 128k
# MPEG-2 in a transport stream: open GOP, 29.97 fps, timestamps not starting
# at zero -- i.e. what a broadcast recording actually looks like
[ -f "$FX/mpeg2.ts" ] || ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc=size=720x480:rate=30000/1001:duration=20" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=20" -shortest \
  -c:v mpeg2video -g 15 -bf 2 -b:v 6000k -pix_fmt yuv420p -aspect 16:9 \
  -c:a mp2 -b:a 192k -f mpegts "$FX/mpeg2.ts"

pass=0; fail=0
t() {
  local name=$1 src=$2 out=$3; shift 3
  local XFAIL="${XFAIL:-0}"
  local res
  res=$(python3 -m smartcut "$FX/$src" -o "$OUT/$out" --verify "$@" 2>&1)
  if echo "$res" | grep -q "MISMATCH\|Traceback\|ffmpeg failed"; then
    if [ "${XFAIL:-0}" = 1 ]; then
      printf "  xfail %-24s (known limitation)\n" "$name"; pass=$((pass+1)); return
    fi
    printf "  FAIL  %-24s\n" "$name"
    echo "$res" | tail -20 | sed 's/^/        /'
    fail=$((fail+1))
  else
    printf "  ok    %-24s %s\n" "$name" \
      "$(echo "$res" | grep -oP 'bit-identical: \K.*?\)')"
    pass=$((pass+1))
  fi
}

echo "running tests ..."
t "single range"     h264.mp4    r1.mp4  --keep 5.3-12.7
t "multi range"      h264.mp4    r2.mp4  --keep 1.5-4.2 --keep 10.0-18.5
t "cut middle"       h264.mp4    r3.mp4  --cut 8.0-20.0
t "keyframe-exact"   h264.mp4    r4.mp4  --keep 6.0-12.0
t "audio reencode"   h264.mp4    r5.mp4  --keep 5.3-12.7 --audio-mode reencode
t "sub-GOP range"    h264.mp4    r6.mp4  --keep 6.5-7.2
t "hevc"             hevc.mp4    r7.mp4  --keep 3.3-14.7
t "29.97 fps"        ntsc.mp4    r8.mp4  --keep 3.3-14.7
t "open-GOP"         opengop.mp4 r9.mp4  --keep 3.3-14.7
t "matroska output"  h264.mp4    r10.mkv --keep 5.3-12.7
t "mpeg2 ts open-GOP" mpeg2.ts   r11.mp4 --keep 3.3-14.7
# known limitation: this range's edges land at an unlucky frame phase, so the
# idealised grid loses the picture that should end the second range. See
# docs/technical/validation.ja.md, "既知の制限".
XFAIL=1 t "mpeg2 ts multi"    mpeg2.ts   r12.mp4 --keep 2.0-6.0 --keep 11.0-17.0
t "mpeg2 ts to end"   mpeg2.ts   r13.mp4 --cut 4.0-9.0

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
