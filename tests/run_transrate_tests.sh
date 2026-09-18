#!/usr/bin/env bash
# Writing the pictures back smaller, so that a night's cuts fit a disc.
#
# Three things have to hold, and the first of them is the one that says
# whether this program reads MPEG-2 at all.
#
# **A picture written back unchanged has to be the picture that arrived.**
# Every field of every macroblock is read and written again from what was
# read -- the address, the type, the motion vectors, the DC difference, every
# coefficient -- so a table with one row wrong, a length counted wrong or a
# run misplaced comes out as bytes that differ. It is the whole of the
# evidence that the walk is right, and it is checked over tens of thousands
# of pictures rather than over a fixture.
#
# **A cut asked to fit a size has to fit it**, and what it cost has to be
# visible: the size that came out, and the picture measured against the same
# cut written whole.
#
# **What cannot be made smaller has to say so**, rather than coming out the
# size it always was and leaving somebody to find that out from a disc.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
DIAG=rust/target/release/examples/transdiag
FIT=rust/target/release/examples/fitdiag
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
WORK="${SMARTCUT_WORK:-${TMPDIR:-/tmp}}/smartcut-transrate"
mkdir -p "$WORK"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -x "$DIAG" ] || {
  echo "build first: (cd rust && cargo build --release --example transdiag)" >&2
  exit 2
}
[ -x "$FIT" ] || {
  echo "build first: (cd rust && cargo build --release --example fitdiag)" >&2
  exit 2
}

pass=0; fail=0

# How many pictures may be handed back as they arrived.
#
# One, and it is the same one every time: a recording that starts in the
# middle of a picture has a first picture that is not a whole one. The cut
# writes it through untouched and says so. Anything beyond that is a picture
# this program could not read, which is what this suite is looking for.
DECLINE_LIMIT=1

## The identity ------------------------------------------------------------

identity() {
  local name=$1 src="$MEDIA/$2" secs=${3:-30}
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  local out
  out=$("$DIAG" "$src" --seconds "$secs" 2>&1)
  local exact differed declined
  # The first line only: the per-picture-kind lines below it say the same
  # words about a third of the pictures each.
  exact=$(echo "$out" | sed -n 's/.*unchanged: \([0-9]*\) exact.*/\1/p' | head -1)
  differed=$(echo "$out" | sed -n 's/.*exact, \([0-9]*\) differed.*/\1/p' | head -1)
  declined=$(echo "$out" | sed -n 's/.*declined \([0-9]*\):.*/\1/p' | head -1)
  declined=${declined:-0}
  if [ -z "${exact:-}" ]; then
    printf "  FAIL  %-26s 読めなかった\n" "$name"; fail=$((fail+1)); return
  fi
  if [ "${differed:-1}" != "0" ] || [ "$declined" -gt "$DECLINE_LIMIT" ]; then
    printf "  FAIL  %-26s %s 枚が元と違う / %s 枚読めず\n" "$name" "$differed" "$declined"
    echo "$out" | sed -n 's/^    \(I\|P\|B\|slice\) /        &/p' | head -4
    fail=$((fail+1)); return
  fi
  printf "  ok    %-26s %s 枚が完全一致（読めず %s 枚）\n" "$name" "$exact" "$declined"
  pass=$((pass+1))
}

## The fit -----------------------------------------------------------------

# A cut of the same stretch, written whole and written to a size, and what the
# second cost against the first.
fits() {
  local name=$1 src="$MEDIA/$2" share=$3
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  local plain="$WORK/plain.ts" small="$WORK/small.ts"
  rm -f "$plain" "$small"
  "$BIN" "$src" --keep 30.0-420.0 -o "$plain" >/dev/null 2>&1
  if [ ! -s "$plain" ]; then
    printf "  FAIL  %-26s 元の出力が空\n" "$name"; fail=$((fail+1)); return
  fi
  # The size to aim at, taken from the cut that was written whole: this suite
  # is about whether a size asked for is reached, not about whether the
  # estimate that usually names one is right.
  #
  # The two are not the same question, and the stretch below is long for that
  # reason. What names a size on the way in is a *mean* rate over the whole
  # recording, and a minute taken out of the middle of one can be half again
  # as busy as the mean -- an opening title over a sixty-second window is a
  # cut the estimate says already fits, nothing is rewritten, and the failure
  # is the estimate's rather than the rewrite's. Over several minutes the two
  # meet, which is the case a disc is actually written from.
  local whole target
  whole=$(stat -c%s "$plain")
  target=$(python3 -c "print(int($whole * $share))")
  "$BIN" "$src" --keep 30.0-420.0 --fit "$target" -o "$small" >/dev/null 2>&1
  if [ ! -s "$small" ]; then
    printf "  FAIL  %-26s 縮めた出力が空\n" "$name"; fail=$((fail+1)); return
  fi
  local got
  got=$(stat -c%s "$small")
  # It has to be under the size, and it has to have actually been made
  # smaller: a cut that came out at the size it always was has not fitted
  # anything, it has ignored the ask.
  if [ "$got" -gt "$target" ]; then
    printf "  FAIL  %-26s %s バイトに収まらず %s バイト\n" "$name" "$target" "$got"
    fail=$((fail+1)); return
  fi
  local ratio
  ratio=$(python3 -c "print(f'{$got / $whole:.3f}')")
  # And the picture, against the same cut written whole. Anything below this
  # is not a smaller version of the recording any more.
  local psnr
  # The filter's own summary, which it prints at the level ffmpeg logs at
  # rather than through the error channel.
  # A minute of it is enough to say what the rewrite did, and decoding the
  # whole of both is most of what this suite would cost.
  psnr=$(ffmpeg -v info -t 60 -i "$plain" -t 60 -i "$small" -lavfi "[0:v][1:v]psnr" -f null - 2>&1 |
    sed -n 's/.*average:\([0-9.]*\).*/\1/p' | tail -1)
  psnr=${psnr:-0}
  local ok
  ok=$(python3 -c "print(1 if $psnr >= 32.0 else 0)")
  if [ "$ok" != "1" ]; then
    printf "  FAIL  %-26s %s 倍に収まったが PSNR %s dB\n" "$name" "$ratio" "$psnr"
    fail=$((fail+1)); return
  fi
  printf "  ok    %-26s %s 倍（目標 %s）・PSNR %s dB\n" "$name" "$ratio" "$share" "$psnr"
  pass=$((pass+1))
  rm -f "$plain" "$small"
}

# A recording whose pictures this cannot rewrite is copied, and says so.
declines() {
  local name=$1 src="$MEDIA/$2"
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  local out
  out=$("$BIN" "$src" --keep 10.0-40.0 --video-share 0.6 -o "$WORK/other.ts" 2>&1)
  if echo "$out" | grep -q "only MPEG-2 can be written back smaller"; then
    printf "  ok    %-26s コピーして、そう言う\n" "$name"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s 何も言わなかった\n" "$name"
    fail=$((fail+1))
  fi
  rm -f "$WORK/other.ts"
}

echo "MPEG-2 を読み書きして元に戻るか"
identity "AT-X"              atx.ts
identity "BS フジ"           bsfuji.ts
identity "地デジ 日本海テレビ" full_ntv.ts
identity "地デジ NHK E"      terrestrial_nhke.ts
identity "SD 16:9"           sd169.ts
identity "デモ素材"          demo_broadcast.ts

echo
echo "指定した大きさに収まるか"
fits "AT-X 80%"              atx.ts 0.80
fits "BS フジ 70%"           bsfuji.ts 0.70
fits "地デジ 60%"            full_ntv.ts 0.60

echo
echo "縮められない録画"
declines "VC-1 の Blu-ray"     bd-clip.ts

## What the estimate is built on ------------------------------------------

# A share is a disc divided by a rate, so the rate decides whether a disc
# comes out a coaster. Where the recording was read off a disc there is no
# counting it -- the entry point map never looked at a picture -- and it is
# sampled instead. The sample has to agree with the count, and the estimate
# built on either has to be the size of the file it describes.
#
# What this caught: a recording opened off a BDAV disc had no rate at all,
# and what stood in for it -- the file's own rate less a tenth -- was 2% low
# on six broadcast recordings. 2% of a disc is a quarter of a gigabyte over.
rate_of() { echo "$1" | sed -n 's/.*pictures \([0-9.]*\) Mbit\/s.*/\1/p'; }
out_by()  { echo "$1" | sed -n 's/.*out by \([-+][0-9.]*\)%.*/\1/p'; }
# Is $1 within $2 of nought? Both in percent, and neither the shell nor the
# sign is to be trusted with it.
within() {
  [ -n "$1" ] || return 1
  awk -v v="$1" -v lim="$2" 'BEGIN { exit !((v < 0 ? -v : v) <= lim) }'
}
# Write one recording onto a disc of its own and give back the clip.
onto_a_disc() {
  local src=$1 keep=$2 disc=$3
  rm -rf "$disc"
  "$BIN" "$src" --keep "$keep" --bdav "$disc" --programme "レート見本" \
    >"$disc.log" 2>&1
  [ -f "$disc/BDAV/STREAM/00001.m2ts" ] && echo "$disc/BDAV/STREAM/00001.m2ts"
}

# The sample against the count, on the fixture, which needs no media.
sample_agrees() {
  local name="標本と全数"
  if [ ! -f "$FX/mpeg2.ts" ]; then
    printf "  SKIP  %-26s run tests/run_tests.sh first\n" "$name"; return
  fi
  local clip
  clip=$(onto_a_disc "$FX/mpeg2.ts" 0-20 "$WORK/ratedisc")
  if [ -z "$clip" ]; then
    printf "  FAIL  %-26s ディスクが書けなかった\n" "$name"; fail=$((fail+1)); return
  fi
  local walked sampled wr sr off
  walked=$("$FIT" --index scan "$clip" 2>/dev/null)
  sampled=$("$FIT" --index disc "$clip" 2>/dev/null)
  wr=$(rate_of "$walked"); sr=$(rate_of "$sampled")

  # The map has to have been read at all: the disc index falling back on a
  # guess would pass the comparison below while measuring nothing.
  if echo "$sampled" | grep -q "read off the stream"; then
    printf "  ok    %-26s ディスクの索引でも測る\n" "映像レートの出どころ"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s %s\n" "映像レートの出どころ" "$(echo "$sampled" | grep Mbit)"
    fail=$((fail+1))
  fi

  off=$(awk -v a="$wr" -v b="$sr" 'BEGIN { printf("%.2f", (a > 0) ? (b / a - 1) * 100 : 999) }')
  if within "$off" 5; then
    printf "  ok    %-26s 数えて %s、測って %s Mbit/s (%s%%)\n" "$name" "$wr" "$sr" "$off"
    pass=$((pass+1))
  else
    printf "  FAIL  %-26s 数えて %s、測って %s Mbit/s (%s%%)\n" "$name" "$wr" "$sr" "$off"
    fail=$((fail+1))
  fi
  rm -rf "$WORK/ratedisc" "$WORK/ratedisc.log"
}

# And the estimate against the file, which needs a real recording: the
# fixture is twenty seconds long and a clip is padded out to a 196,608-byte
# boundary, so a tenth of that one is padding and nothing about its size is
# about its pictures.
estimate_is_the_size() {
  local name=$1 src="$MEDIA/$2"
  if [ ! -f "$src" ]; then printf "  SKIP  %-26s no %s\n" "$name" "$2"; return; fi
  local clip
  clip=$(onto_a_disc "$src" 0-120 "$WORK/sizedisc")
  if [ -z "$clip" ]; then
    printf "  FAIL  %-26s ディスクが書けなかった\n" "$name"; fail=$((fail+1)); return
  fi
  local which o line=""
  for which in scan disc; do
    o=$(out_by "$("$FIT" --index "$which" "$clip" 2>/dev/null)")
    if within "$o" 3; then
      line="$line $which ${o}%"
    else
      printf "  FAIL  %-26s %s で %s%%\n" "$name" "$which" "${o:-?}"
      fail=$((fail+1)); rm -rf "$WORK/sizedisc" "$WORK/sizedisc.log"; return
    fi
  done
  printf "  ok    %-26s%s\n" "$name" "$line"
  pass=$((pass+1))
  rm -rf "$WORK/sizedisc" "$WORK/sizedisc.log"
}

echo
echo "見積もりの元になるレート"
sample_agrees
estimate_is_the_size "AT-X"        atx.ts
estimate_is_the_size "BS フジ"     bsfuji.ts
estimate_is_the_size "地デジ"      full_ntv.ts

echo
echo "ok $pass / fail $fail"
[ "$fail" -eq 0 ]
