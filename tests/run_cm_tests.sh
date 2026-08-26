#!/usr/bin/env bash
# Commercial detection, pinned against what the recordings actually contain.
#
# Every number here was checked by eye first -- thumbnails every thirty
# seconds across each recording -- because the detector's own output is not
# evidence about the detector. Two of the four have no commercials at all,
# and they are the more valuable half of the suite: a false block is worse
# than a missed one, since acting on it removes programme.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0

# name, file, expected block count, then "start-end" per block (±2s)
check() {
  local name=$1 file=$2 want=$3; shift 3
  local src="$MEDIA/$file"
  if [ ! -f "$src" ]; then printf "  SKIP  %-24s no %s\n" "$name" "$file"; return; fi
  local out
  out=$("$BIN" "$src" --analyze --detect-cm --logo 2>/dev/null \
        | sed -n 's/^   \([0-9:.]*\)  →  \([0-9:.]*\).*継ぎ目.*/\1 \2/p')
  local got
  got=$(echo "$out" | grep -c . )
  [ -z "$out" ] && got=0
  local why=""
  [ "$got" = "$want" ] || why=" 個数 $want→$got"
  if [ -z "$why" ] && [ "$want" != "0" ]; then
    local i=1
    for span in "$@"; do
      local a b ga gb
      a=${span%-*}; b=${span#*-}
      ga=$(echo "$out" | sed -n "${i}p" | awk '{print $1}')
      gb=$(echo "$out" | sed -n "${i}p" | awk '{print $2}')
      local ok
      ok=$(python3 - "$a" "$b" "$ga" "$gb" <<'PY'
import sys
def secs(s):
    h, m, rest = s.split(":")
    return int(h) * 3600 + int(m) * 60 + float(rest)
a, b, ga, gb = (float(sys.argv[1]), float(sys.argv[2]), secs(sys.argv[3]), secs(sys.argv[4]))
print("ok" if abs(ga - a) <= 2.0 and abs(gb - b) <= 2.0 else f"{ga:.1f}-{gb:.1f}")
PY
)
      [ "$ok" = "ok" ] || why="$why 第${i}ブロック($span→$ok)"
      i=$((i+1))
    done
  fi
  # Commercials are sold in fifteen-second units, so a block that really is
  # one is a whole number of them long. Nothing in the detector enforces
  # this -- it comes out that way only if the boundaries are the real cuts,
  # which makes it the sharpest check available on that.
  if [ -z "$why" ] && [ "$want" != "0" ]; then
    local off
    off=$(echo "$out" | python3 -c "
import sys
def secs(s):
    h, m, rest = s.split(':')
    return int(h) * 3600 + int(m) * 60 + float(rest)
bad = []
for i, line in enumerate(sys.stdin):
    a, b = line.split()
    span = secs(b) - secs(a)
    if span < 30:          # an edge, not a run of commercials
        continue
    off = abs(span - round(span / 15) * 15)
    if off > 0.25:
        bad.append(f'第{i+1}({span:.1f}s)')
print(' '.join(bad))
")
    [ -z "$off" ] || why="$why 15秒の倍数でない: $off"
  fi
  if [ -z "$why" ]; then
    printf "  ok    %-24s %s ブロック\n" "$name" "$got"; pass=$((pass+1))
  else
    printf "  FAIL  %-24s%s\n" "$name" "$why"; fail=$((fail+1))
  fi
}

echo "CM 検出（目視で確かめた正解と突き合わせ）"
# 地デジ・ロゴあり: 頭に CM 4 秒、そのあと 15 秒の正確な倍数が 4 ブロック。
# 頭のは録画が番組より早く始まっただけで、継ぎ目を持たない -- ロゴが最初に
# 現れたところが番組の頭だという、それだけで見つかる。
check "地デジ 日本海テレビ" full_ntv.ts 5 \
  0.0-3.8 589.8-739.8 1358.8-1478.8 1544.8-1664.8 1743.8-1803.8
# AT-X・ロゴなし: 末尾の 5 分だけが CM
check "AT-X（ロゴなし）"    full_atx.ts 1 1486.8-1786.9
# CM の無い録画 -- 出してはいけない側
check "BS フジ（CM なし）"  full_bsfuji.ts 0
check "NHK E（CM なし）"    terrestrial_nhke.ts 0

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
