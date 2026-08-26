#!/usr/bin/env bash
# The scrubbing side of the engine: the picture handed to the timeline has to
# be the picture that was asked for.
#
# Worth its own suite because the failure is silent. A transport stream seeks
# by byte position, so the landing is approximate -- and when it lands late
# the decoder simply returns a later picture, which looks perfectly fine on
# screen while disagreeing with the frame counter beside it.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
OUT="${TMPDIR:-/tmp}/smartcut-preview-out"
mkdir -p "$OUT"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
check() {
  local name=$1 src=$2 at=$3 slack=${4:-0.017}
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s %s\n" "$name" "no $src"; return; fi
  local line
  line=$("$BIN" "$src" --preview "$at" -o "$OUT/p.jpg" 2>/dev/null | grep "asked")
  local got
  got=$(echo "$line" | sed -n 's/.*got \([0-9.]*\)s.*/\1/p')
  local kind
  kind=$(echo "$line" | sed -n 's/.*s  \([IPB]\) picture.*/\1/p')
  local bytes
  bytes=$(echo "$line" | sed -n 's/.*(\([0-9]*\) bytes).*/\1/p')
  # Half the gap between pictures: any more and a different picture came
  # back. Under 2:3 pulldown a picture lasts two or three fields, so the gaps
  # alternate 33.4ms and 50.1ms and the slack has to cover the wider one.
  local ok
  ok=$(python3 -c "print(abs($got-$at) <= $slack and $bytes > 2000)" 2>/dev/null)
  if [ "$ok" = "True" ]; then
    printf "  ok    %-26s asked %8.3fs  got %8.3fs  %s  %s bytes\n" "$name" "$at" "$got" "$kind" "$bytes"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s asked %8.3fs  got %8s  %s\n" "$name" "$at" "${got:-?}" "$line"
    fail=$((fail+1))
  fi
}

echo "preview accuracy"
# Times chosen to sit in the middle of a GOP, where an overshoot shows up.
check "ts near start"       "$MEDIA/full_ntv.ts"        3.400
check "ts mid file"         "$MEDIA/full_ntv.ts"      900.250
check "ts on a boundary"    "$MEDIA/full_ntv.ts"     1200.000
check "ts late"             "$MEDIA/full_ntv.ts"     1799.900
check "pulldown mid file"   "$MEDIA/full_atx.ts"      600.500 0.026
check "pulldown late"       "$MEDIA/full_atx.ts"     1500.125 0.026
check "short ts mid file"   "$MEDIA/terrestrial_nhke.ts" 200.750

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
