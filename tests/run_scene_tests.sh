#!/usr/bin/env bash
# Scene detection, checked against something known rather than by eye.
#
# A commercial break's edges and its internal 15-second junctions are always
# hard cuts -- that is what a break is made of -- and the silence-and-logo
# detector finds them by a completely independent route. So every boundary it
# reports should also carry a scene mark. Anything it misses is a scene the
# picture comparison failed to see.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
WORK="${TMPDIR:-/tmp}/smartcut-scene-out"
mkdir -p "$WORK"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
check() {
  local name=$1 src=$2 want_edges=$3 want_grid=$4
  if [ ! -f "$src" ]; then printf "  SKIP  %-24s %s\n" "$name" "no $src"; return; fi
  SMARTCUT_SCENES_OUT="$WORK/scenes.txt" "$BIN" "$src" --analyze --scenes >"$WORK/s.log" 2>/dev/null
  "$BIN" "$src" --analyze --detect-cm --logo >"$WORK/cm.log" 2>/dev/null
  local res
  res=$(SCENES="$WORK/scenes.txt" CM="$WORK/cm.log" WE="$want_edges" WG="$want_grid" python3 - <<'PY'
import os, re, sys
scenes = [float(l) for l in open(os.environ["SCENES"])]
def hms(s):
    h, m, rest = s.split(":")
    return int(h) * 3600 + int(m) * 60 + float(rest)
blocks = []
for line in open(os.environ["CM"]):
    m = re.match(r"\s+(\d+:\d+:[\d.]+)\s+→\s+(\d+:\d+:[\d.]+)\s+\(\s*[\d.]+s, 継ぎ目", line)
    if m:
        blocks.append((hms(m.group(1)), hms(m.group(2))))
near = lambda t: any(abs(x - t) < 0.75 for x in scenes)
edges = [t for b in blocks for t in b]
grid = [b[0] + 15 * i for b in blocks for i in range(int((b[1] - b[0]) // 15) + 1)]
eh, gh = sum(map(near, edges)), sum(map(near, grid))
ok = blocks and eh >= int(os.environ["WE"]) and gh >= int(os.environ["WG"])
print(f"{'OK' if ok else 'BAD'}|{len(scenes)}|{len(blocks)}|{eh}/{len(edges)}|{gh}/{len(grid)}")
PY
)
  IFS='|' read -r verdict nsc nblk edge grid <<<"$res"
  if [ "$verdict" = "OK" ]; then
    printf "  ok    %-24s %s scenes, CM %s blocks: edges %s, 15s junctions %s\n" \
      "$name" "$nsc" "$nblk" "$edge" "$grid"
    pass=$((pass+1))
  else
    printf "  FAIL  %-24s %s\n" "$name" "$res"
    fail=$((fail+1))
  fi
}

echo "scene marks vs. commercial boundaries"
check "commercial broadcast" "$MEDIA/full_ntv.ts" 8 31

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
