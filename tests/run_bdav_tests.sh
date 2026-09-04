#!/usr/bin/env bash
# A BDAV disc, read as a folder and as an image, has to give the same answers.
#
# The disc is built here out of the ordinary fixtures: the MPEG-2 transport
# stream is remuxed into the 192 byte packets a Blu-ray uses, the index files
# are written around it by tests/bdav_disc.py, and the whole thing is wrapped
# in a UDF image by genisoimage. Then both are opened, and the cut each of them
# produces is compared byte for byte -- which is the claim the feature makes:
# a recording inside an image is read where it lies, and reading it that way
# changes nothing.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-bdav"
DISC="$OUT/disc"
ISO="$OUT/disc.iso"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }
MKISO=$(command -v genisoimage || command -v mkisofs || true)
[ -n "$MKISO" ] || { echo "needs genisoimage (apt install genisoimage)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-34s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-34s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { # name expected actual
  if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi
}

echo "building a disc in $OUT ..."
rm -rf "$OUT"; mkdir -p "$DISC/BDAV/STREAM"
# What a Blu-ray recording is: the same transport stream, each packet behind
# four bytes saying when it arrived.
for n in a b; do
  ffmpeg -hide_banner -loglevel error -y -i "$FX/mpeg2.ts" \
    -c copy -f mpegts -mpegts_m2ts_mode 1 "$DISC/BDAV/STREAM/$n.m2ts" || exit 2
done
# Where the stream begins on its own clock. A playlist counts in that clock,
# so an IN point picked out of the air would put the disc's chapter marks
# somewhere the recording is not -- and the reader would be right to throw
# them away. Broadcast material does not start at zero, which is the whole
# reason this is asked rather than assumed.
STARTS=$(ffprobe -v error -show_entries format=start_time -of default=nw=1:nk=1 \
  "$DISC/BDAV/STREAM/a.m2ts")
python3 tests/bdav_disc.py "$DISC" \
  "テスト録画 その1 #07「神様的休息日の過ごし方。」=a.m2ts,20,$STARTS" \
  "テスト録画 その2 BS11=b.m2ts,20,$STARTS" >/dev/null || exit 2
# UDF, which is the filesystem a Blu-ray carries. What this writes is
# version 1.02 -- no metadata partition, where an authoring tool writes 2.60
# and uses one -- so between this and a real disc both shapes are covered.
"$MKISO" -quiet -udf -V SMARTCUT_TEST -o "$ISO" "$DISC" || exit 2

echo "running tests ..."

# --- the index -----------------------------------------------------------
list_folder=$("$BIN" "$DISC" 2>&1)
list_iso=$("$BIN" "$ISO" 2>&1)

# A listed recording is a number, a running time and a name.
rows() { grep -cP '^\s+\d+\s+\d\d:\d\d:' <<<"$1"; }
same "folder: how many recordings" "2" "$(rows "$list_folder")"
same "image: how many recordings"  "2" "$(rows "$list_iso")"

names_of() { grep -oP '^\s+\d+\s+\S+\s+\K.*?(?=\s+\d+ mark)' <<<"$1"; }
want=$'テスト録画 その1 #07「神様的休息日の過ごし方。」\nテスト録画 その2 BS11'
same "folder: the programmes are named" "$want" "$(names_of "$list_folder")"
same "image: the programmes are named"  "$want" "$(names_of "$list_iso")"
same "the marks are read"  "2" "$(grep -c '2 mark(s)' <<<"$list_iso")"

# --- opening one ---------------------------------------------------------
"$BIN" "$DISC" --title 2 --keep 3-9 -o "$OUT/folder.ts" >"$OUT/folder.log" 2>&1
"$BIN" "$ISO"  --title 2 --keep 3-9 -o "$OUT/iso.ts"    >"$OUT/iso.log" 2>&1
"$BIN" "$DISC/BDAV/STREAM/00002.m2ts" --keep 3-9 -o "$OUT/plain.ts" >"$OUT/plain.log" 2>&1

md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }
folder=$(md5 "$OUT/folder.ts"); iso=$(md5 "$OUT/iso.ts"); plain=$(md5 "$OUT/plain.ts")
if [ -z "$folder" ]; then
  bad "a cut from the folder" "$(tail -2 "$OUT/folder.log")"
elif [ "$folder" = "$plain" ]; then
  ok "a cut through the disc is the same" "${folder:0:12}"
else
  bad "a cut through the disc is the same" "folder $folder, stream $plain"
fi
if [ -z "$iso" ]; then
  bad "a cut from inside the image" "$(tail -2 "$OUT/iso.log")"
elif [ "$iso" = "$folder" ]; then
  ok "a cut from inside the image is the same" "${iso:0:12}"
else
  bad "a cut from inside the image is the same" "image $iso, folder $folder"
fi

# A chapter is a time in the recording, and the recording's clock is not the
# playlist's: the disc counts from zero and the stream does not. The marks
# were written at the IN point and halfway through, so on the recording's own
# clock they are 0 and 10 seconds -- which is where the editor draws them.
same "the marks are placed" "marks : 2 [0.000, 10.000]" \
  "$(grep -o 'marks : .*' "$OUT/iso.log")"

# The tables the recording carries have to survive being read out of an
# image: this is the pass that reads the transport stream itself, byte by
# byte, rather than through libavformat.
if grep -q "the broadcast's own tables are not" "$OUT/iso.log"; then
  bad "the broadcast's own tables are kept" "$(grep -o 'note:.*' "$OUT/iso.log")"
else
  ok "the broadcast's own tables are kept"
fi

# --- naming --------------------------------------------------------------
same "the title is announced" \
  "title : テスト録画 その2 BS11" "$(grep '^title :' "$OUT/iso.log")"
same "the recording is named by its path" \
  "input : $ISO/BDAV/STREAM/00002.m2ts" "$(grep '^input :' "$OUT/iso.log")"

# --- what is not a disc --------------------------------------------------
notdisc=$("$BIN" "$FX/mpeg2.ts" 2>&1 | head -1)
case "$notdisc" in
  input*) ok "an ordinary recording is not a disc" ;;
  *)      bad "an ordinary recording is not a disc" "$notdisc" ;;
esac

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
