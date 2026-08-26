#!/usr/bin/env bash
# Commercial detection, pinned against what the recordings actually contain.
#
# Every number here was checked by eye first -- thumbnails every thirty
# seconds across each recording -- because the detector's own output is not
# evidence about the detector. Three of the five have no commercials at all,
# and they are the most valuable part of the suite: a false block is worse
# than a missed one, since acting on it removes programme.
#
# That asymmetry is what gets measured. A block count says whether the
# detector found the right number of things and nothing about what it would
# have done to the recording, so each case is scored in seconds instead, on
# two separate budgets:
#
#   本編を誤削除  programme swallowed by a block -- gone once the cut is made
#   CM 残り       commercial left outside one -- a nuisance, visible to anyone
#
# The two never share a budget: one accuracy number would let a regression in
# the expensive direction hide behind an improvement in the cheap one.
#
# Sub-second boundary placement is not checked that way -- the eyeballed truth
# is only good to about a tenth of a second. It is checked by the 15-second
# multiple in cm_score.py, which is sharper and independent of the detector.
#
# Material lives in ~/media (override with SMARTCUT_MEDIA); it cannot go in
# the repository, being several gigabytes of off-air recording. When it is
# absent the suite exits non-zero: a run that checked nothing must not be
# reportable as a run that passed.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0; missing=0

# name, file, 本編を誤削除の上限, CM 残りの上限, 尺, then the true spans.
# A span written "~START-END" is grey: broadcaster's own material around the
# break, which costs nothing whichever side of the cut it lands on.
check() {
  local name=$1 file=$2 over=$3 under=$4 dur=$5; shift 5
  local src="$MEDIA/$file"
  if [ ! -f "$src" ]; then
    printf "  MISSING  %-22s %s\n" "$name" "$file"
    missing=$((missing+1)); return
  fi
  local out report summary why
  out=$("$BIN" "$src" --analyze --detect-cm --logo 2>/dev/null \
        | sed -n 's/^   \([0-9:.]*\)  →  \([0-9:.]*\).*継ぎ目.*/\1 \2/p')
  report=$(printf '%s\n' "$out" | python3 tests/cm_score.py "$over" "$under" "$dur" "$@")
  summary=$(printf '%s\n' "$report" | head -1)
  why=$(printf '%s\n' "$report" | tail -n +2 | grep -v '^[[:space:]]*$' || true)
  if [ -z "$why" ]; then
    printf "  ok    %-24s %s\n" "$name" "$summary"; pass=$((pass+1))
  else
    printf "  FAIL  %-24s %s\n" "$name" "$summary"
    printf '%s\n' "$why" | sed 's/^/            /'
    fail=$((fail+1))
  fi
}

echo "CM 検出（目視で確かめた正解と突き合わせ）"
# 地デジ・ロゴあり: 頭に CM 4 秒、そのあと 15 秒の正確な倍数が 4 ブロック。
# 頭のは録画が番組より早く始まっただけで、継ぎ目を持たない -- ロゴが最初に
# 現れたところが番組の頭だという、それだけで見つかる。
check "地デジ 日本海テレビ" full_ntv.ts 1.0 1.0 1805.502 \
  0.0-3.8 589.8-739.8 1358.8-1478.8 1544.8-1664.8 1743.8-1803.8
# AT-X・ロゴなし: 末尾の 5 分だけが CM
check "AT-X（ロゴなし）"    full_atx.ts 1.0 1.0 1806.986 1486.8-1786.9
# BS フジ・アニメ枠: CM 2 ブロックが 15 秒の正確な倍数（134.9s = 9x15、
# 105.0s = 7x15）。頭と末尾は録画が枠をまたいでいて、CM のほかに枠 ID
# （BS FUJI PRESENTS / アニメギルド / +Ultra）と番宣が挟まる。境目は
# フレーム単位で確かめた -- 3.930 が枠 ID の最初のピクチャ、189.916 が
# CM の最初、324.818 が本編（EPISODE 2 のアイキャッチ）の最初。
# CM 残りの上限だけ緩い。末尾のブロックは最後の継ぎ目 1683.9 で終わるが、
# 録画はその 3.4 秒あとまで続いていて、そこはまだ CM である。継ぎ目からは
# 判別できない -- 日本海テレビは同じ形（最後のリセットが尺の 2 秒手前）で
# そのあと本編が始まっており、伸ばせばあちらを 1.7 秒削ってしまう。分ける
# には録画全体のロゴを見るしかなく、30 秒の復号を CM 3.4 秒のために払う
# ことになる。取りこぼしは安いほうの誤りなので、払わない。
check "BS フジ（枠つき）"   bsfuji_thunder3_02.ts 1.0 4.0 1687.346 \
  0.0-3.9305 ~3.9305-29.0 189.9163-324.8178 1007.9335-1112.9384 \
  ~1643.5-1653.9121 1653.9121-1687.3455
# CM の無い録画 -- 出してはいけない側
check "BS フジ（CM なし）"  full_bsfuji.ts 0.5 0.5 1439.979
check "NHK E（CM なし）"    terrestrial_nhke.ts 0.5 0.5 389.035

echo "=== $pass passed, $fail failed, $missing missing ==="
if [ "$missing" -gt 0 ]; then
  echo "素材の無い分は通っていない。SMARTCUT_MEDIA を確かめること。" >&2
fi
[ "$fail" -eq 0 ] && [ "$missing" -eq 0 ]
