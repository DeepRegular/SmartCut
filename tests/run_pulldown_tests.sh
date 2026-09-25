#!/usr/bin/env bash
# 2:3 pulldown through a cut: does the output repeat the fields the recording
# did, and does the parity of its fields still alternate at every seam?
#
# libavcodec's MPEG-2 encoder never writes `repeat_first_field`, so until
# 0.8.7 a re-encoded stretch of film came out two fields a picture: a fifth
# short in the stream's own terms, with the length carried only by the
# timestamps. And on a recording whose first picture happened to be film, the
# re-encoded stretch announced a progressive sequence among copied pictures
# that say interlaced. See `Mpeg2Display` in cut.rs.
#
# What is checked, on the elementary stream of each cut:
#   * fields    within one of twice the frames the range covers
#   * breaks    no two consecutive fields of the same parity
#   * sequence  every sequence extension says interlaced, as the recording's do
#   * repeats   the range is film, so some picture repeats a field
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
OUT="${TMPDIR:-/tmp}/smartcut-pulldown-out"
mkdir -p "$OUT"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
check() {
  local name=$1 src=$2 range=$3
  if [ ! -f "$src" ]; then printf "  SKIP  %-30s no %s\n" "$name" "$src"; return; fi
  if ! "$BIN" "$src" --keep "$range" -o "$OUT/p.ts" >"$OUT/p.log" 2>&1; then
    printf "  FAIL  %-30s the cut failed: %s\n" "$name" "$(tail -1 "$OUT/p.log")"
    fail=$((fail+1)); return
  fi
  ffmpeg -v error -i "$OUT/p.ts" -map 0:v -c copy -f mpeg2video -y "$OUT/p.m2v"
  local line
  line=$(python3 tests/mpeg2_fields.py "$OUT/p.m2v")
  local ok
  ok=$(python3 - "$range" "$line" <<'PY'
import sys, re
a, b = map(float, sys.argv[1].split("-"))
f = dict(kv.split("=") for kv in sys.argv[2].split())
want = round((b - a) * 30000 / 1001 * 2)
good = (abs(int(f["fields"]) - want) <= 1 and f["breaks"] == "0"
        and f["progressive_sequence"] == "0" and int(f["repeats"]) > 0)
print("True" if good else f"want {want} fields")
PY
)
  if [ "$ok" = "True" ]; then
    printf "  ok    %-30s %s\n" "$name" "$line"; pass=$((pass+1))
  else
    printf "  FAIL  %-30s %s (%s)\n" "$name" "$line" "$ok"; fail=$((fail+1))
  fi
}

echo "pulldown through a cut"
# A clip that opens on film: the container calls the whole of it progressive.
check "film clip, all re-encoded"   "$MEDIA/rff_clip.ts" 10.0441-11.5
check "film clip, copy and tail"    "$MEDIA/rff_clip.ts" 2.5366-4.5386
check "film clip, mid-GOP both ends" "$MEDIA/rff_clip.ts" 12.31-19.87
# A clip that opens on video and turns to film: the container calls it
# interlaced, and the encoder is switched to whole frames for the film.
check "video to film, film head"    "$MEDIA/rff_tt.ts" 52.7-60.3
check "video to film, across"       "$MEDIA/rff_tt.ts" 40.1-54.2

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
