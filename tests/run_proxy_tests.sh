#!/usr/bin/env bash
# The proxy has to be interchangeable with the recording it stands for.
#
# Only two things make that true, and both are silent when they break: the
# proxy's pictures must carry the recording's own presentation times, and its
# entry points must be the recording's entry points. Get the first wrong and
# the timeline shows a picture from a moment other than the one under the
# playhead; get the second wrong and the film strip's cells stop landing on
# the places a cut costs nothing.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
OUT="${TMPDIR:-/tmp}/smartcut-proxy-out"
FIX="${TMPDIR:-/tmp}/smartcut-fixtures"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
mkdir -p "$OUT"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FIX/mpeg2.ts" ] || bash tests/make_demo_media.sh >/dev/null 2>&1

pass=0; fail=0
ok()   { printf "  ok    %-24s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-24s %s\n" "$1" "$2"; fail=$((fail+1)); }

# Entry points, and the picture actually shown at a handful of instants.
check() {
  local name=$1 src=$2; shift 2
  if [ ! -f "$src" ]; then printf "  SKIP  %-24s %s\n" "$name" "no $src"; return; fi
  local proxy="$OUT/$name.mp4"
  rm -f "$proxy" "$OUT/$name.marks"
  if ! "$BIN" "$src" --proxy -o "$proxy" >"$OUT/$name.log" 2>&1; then
    bad "$name build" "$(tail -1 "$OUT/$name.log")"; return
  fi

  local a b
  a=$("$BIN" "$src" --analyze 2>/dev/null | sed -n 's/.*  \([0-9]*\) access points.*/\1/p')
  b=$("$BIN" "$proxy" --as-proxy --analyze 2>/dev/null | sed -n 's/.*  \([0-9]*\) access points.*/\1/p')
  if [ -n "$a" ] && [ "$a" = "$b" ]; then
    ok "$name points" "$a access points, both"
  else
    bad "$name points" "source $a, proxy ${b:-?}"
  fi

  # The picture at a given instant has to be the same picture, which is to
  # say: both sides report the same presentation time back.
  local at got_s got_p
  for at in "$@"; do
    got_s=$("$BIN" "$src" --preview "$at" -o "$OUT/s.jpg" 2>/dev/null |
            sed -n 's/.*got \([0-9.]*\)s.*/\1/p')
    got_p=$("$BIN" "$proxy" --as-proxy --preview "$at" -o "$OUT/p.jpg" 2>/dev/null |
            sed -n 's/.*got \([0-9.]*\)s.*/\1/p')
    if [ -n "$got_s" ] && [ "$got_s" = "$got_p" ]; then
      ok "$name at $at" "both show ${got_s}s"
    else
      bad "$name at $at" "source ${got_s:-?}s, proxy ${got_p:-?}s"
    fi
  done

  local size
  size=$(stat -c%s "$proxy" 2>/dev/null || echo 0)
  local from
  from=$(stat -c%s "$src" 2>/dev/null || echo 0)
  if [ "$size" -gt 10000 ] && [ "$size" -lt "$from" ]; then
    ok "$name size" "$((size/1000)) kB from $((from/1000)) kB"
  else
    bad "$name size" "$size bytes from $from"
  fi
}

echo "proxy stands in for the recording"
check mpeg2   "$FIX/mpeg2.ts"      3.400 9.017 15.250
check opengop "$FIX/opengop.mp4"   2.000 5.500
check ntsc    "$FIX/ntsc.mp4"      1.234 4.000
check ntv     "$MEDIA/full_ntv.ts" 3.400 900.250 1799.900
check atx     "$MEDIA/full_atx.ts" 600.500 1500.125

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
