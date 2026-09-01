#!/usr/bin/env bash
# The seek index has to be worth nothing but the time it saves.
#
# Two claims to keep honest, and both fail silently. Read back, the index must
# describe the recording exactly as the pass that made it did -- the same
# access points, the same open GOPs, the same plan for the same cut, the same
# scene marks -- because everything downstream simply believes it. And the
# byte offsets it carries must land the demuxer on the same picture the old
# timestamp seek reached, or the timeline shows a neighbouring frame and looks
# perfectly correct doing it.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
OUT="${TMPDIR:-/tmp}/smartcut-index-out"
FIX="${TMPDIR:-/tmp}/smartcut-fixtures"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FIX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }

pass=0; fail=0
ok()  { printf "  ok    %-26s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad() { printf "  FAIL  %-26s %s\n" "$1" "$2"; fail=$((fail+1)); }

# Everything the index is asked to reproduce, for one recording.
check() {
  local name=$1 src=$2; shift 2
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s %s\n" "$name" "no $src"; return; fi
  local ix="$OUT/$name.scix"
  rm -f "$ix"

  # The line the CLI prints about the access points, minus the name of
  # whichever strategy produced them -- that is the one part meant to differ.
  local strip='s/  \[.*\]$//'
  local fresh held
  fresh=$("$BIN" "$src" --analyze 2>/dev/null | grep "access points" | sed "$strip")
  # First run writes the index; second must answer from it.
  SMARTCUT_SCENES_OUT="$OUT/$name.fresh.scenes" \
    "$BIN" "$src" --seek-index "$ix" --scenes >"$OUT/$name.build.log" 2>/dev/null
  if [ ! -s "$ix" ]; then
    bad "$name write" "$(tail -1 "$OUT/$name.build.log")"; return
  fi
  held=$("$BIN" "$src" --seek-index "$ix" --analyze 2>/dev/null | grep "access points" | sed "$strip")
  if [ -n "$fresh" ] && [ "$fresh" = "$held" ]; then
    ok "$name points" "$(echo "$fresh" | sed 's/^ *//')"
  else
    bad "$name points" "fresh [$fresh] held [$held]"
  fi

  # The index has to be the source it says it is, not the walk falling back.
  if "$BIN" "$src" --seek-index "$ix" --analyze 2>/dev/null | grep -q "\[seek index\]"; then
    ok "$name source" "read back as the seek index"
  else
    bad "$name source" "fell back to the walk"
  fi

  # A plan is the access points put to work: the same cut has to come out the
  # same to the frame.
  local a b
  a=$("$BIN" "$src" --cut 10-20 --cut 40-50 --analyze 2>/dev/null | grep -E "^ +(copy|re-encode|keep)")
  b=$("$BIN" "$src" --seek-index "$ix" --cut 10-20 --cut 40-50 --analyze 2>/dev/null |
      grep -E "^ +(copy|re-encode|keep)")
  if [ -n "$a" ] && [ "$a" = "$b" ]; then
    ok "$name plan" "$(echo "$a" | wc -l) segment(s), identical"
  else
    bad "$name plan" "the plan changed"
  fi

  # The track travels with the index, so the scene marks have to survive the
  # trip byte for byte.
  SMARTCUT_SCENES_OUT="$OUT/$name.held.scenes" \
    "$BIN" "$src" --seek-index "$ix" --scenes >/dev/null 2>&1
  if cmp -s "$OUT/$name.fresh.scenes" "$OUT/$name.held.scenes"; then
    ok "$name scenes" "$(wc -l <"$OUT/$name.fresh.scenes" | tr -d ' ') marks, identical"
  else
    bad "$name scenes" "the scene marks changed"
  fi

  # And the whole point: seeking by byte offset must reach the same picture
  # as aiming at a timestamp and reading forward did.
  local at
  for at in "$@"; do
    SMARTCUT_BYTE_SEEK=0 "$BIN" "$src" --seek-index "$ix" --preview "$at" \
      -o "$OUT/ts.jpg" >/dev/null 2>&1
    "$BIN" "$src" --seek-index "$ix" --preview "$at" -o "$OUT/by.jpg" >/dev/null 2>&1
    if [ -s "$OUT/by.jpg" ] && cmp -s "$OUT/ts.jpg" "$OUT/by.jpg"; then
      ok "$name at $at" "same picture either way ($(stat -c%s "$OUT/by.jpg") bytes)"
    else
      bad "$name at $at" "byte seek and timestamp seek disagree"
    fi
  done
}

echo "the seek index reproduces the pass that made it"
check mpeg2   "$FIX/mpeg2.ts"       3.400 9.017 15.250
check opengop "$FIX/opengop.mp4"    2.000 5.500
check ntv     "$MEDIA/full_ntv.ts"  3.400 900.250 1200.000 1799.900
check atx     "$MEDIA/full_atx.ts"  600.500 1500.125

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
