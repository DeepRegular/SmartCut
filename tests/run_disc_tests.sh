#!/usr/bin/env bash
# A Blu-ray, read as a folder and as an image, has to give the same answers.
#
# Two discs are built here out of the ordinary fixtures, one of each dialect:
# the MPEG-2 transport stream is remuxed into the 192 byte packets a Blu-ray
# uses, the index files are written around it by tests/disc_index.py, and each
# is wrapped in a UDF image by genisoimage. Then both shapes are opened, and
# the cut each of them produces is compared byte for byte -- which is the
# claim the feature makes: a recording inside an image is read where it lies,
# and reading it that way changes nothing.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-disc"
AV="$OUT/bdav"
AV_ISO="$OUT/bdav.iso"
MV="$OUT/bdmv"
MV_ISO="$OUT/bdmv.iso"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }
MKISO=$(command -v genisoimage || command -v mkisofs || true)
[ -n "$MKISO" ] || { echo "needs genisoimage (apt install genisoimage)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-40s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-40s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { # name expected actual
  if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi
}
has()  { # name needle haystack
  case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "no [$2] in [$3]" ;; esac
}

echo "building two discs in $OUT ..."
rm -rf "$OUT"; mkdir -p "$AV/BDAV/STREAM" "$MV/BDMV/STREAM"
# What a Blu-ray recording is: the same transport stream, each packet behind
# four bytes saying when it arrived.
m2ts() { # <out> [extra ffmpeg args...]
  local out=$1; shift
  ffmpeg -hide_banner -loglevel error -y -i "$FX/mpeg2.ts" "$@" \
    -c copy -f mpegts -mpegts_m2ts_mode 1 "$out" || exit 2
}
for n in a b; do m2ts "$AV/BDAV/STREAM/$n.m2ts"; done
# A pressed disc is two things of a length somebody would keep and one that
# is a logo. Which of them the reader offers is the whole of the question a
# chooser asks.
for n in a b; do m2ts "$MV/BDMV/STREAM/$n.m2ts"; done
m2ts "$MV/BDMV/STREAM/logo.m2ts" -t 3

# Where the stream begins on its own clock. A playlist counts in that clock,
# so an IN point picked out of the air would put the disc's chapter marks
# somewhere the recording is not -- and the reader would be right to throw
# them away. Broadcast material does not start at zero, which is the whole
# reason this is asked rather than assumed.
start_of() { ffprobe -v error -show_entries format=start_time -of default=nw=1:nk=1 "$1"; }
STARTS=$(start_of "$AV/BDAV/STREAM/a.m2ts")
LOGO=$(start_of "$MV/BDMV/STREAM/logo.m2ts")

python3 tests/disc_index.py bdav "$AV" \
  "テスト録画 その1 #07「神様的休息日の過ごし方。」=a.m2ts,20,$STARTS" \
  "テスト録画 その2 BS11=b.m2ts,20,$STARTS" >/dev/null || exit 2
python3 tests/disc_index.py bdmv "$MV" \
  "a.m2ts,20,$STARTS" "b.m2ts,20,$STARTS" "logo.m2ts,3,$LOGO" >/dev/null || exit 2

# UDF, which is the filesystem a Blu-ray carries. What this writes is
# version 1.02 -- no metadata partition, where an authoring tool writes 2.50
# or 2.60 and uses one -- so between this and a real disc both shapes are
# covered.
"$MKISO" -quiet -udf -V SMARTCUT_TEST -o "$AV_ISO" "$AV" || exit 2
"$MKISO" -quiet -udf -V SMARTCUT_TEST -o "$MV_ISO" "$MV" || exit 2

echo "running tests ..."

# --- the index -----------------------------------------------------------
av_folder=$("$BIN" "$AV" 2>&1)
av_iso=$("$BIN" "$AV_ISO" 2>&1)
mv_folder=$("$BIN" "$MV" 2>&1)
mv_iso=$("$BIN" "$MV_ISO" 2>&1)

# A listed recording is a number, a running time and a name.
rows() { grep -cP '^[* ]?\s*\d+\s+\d\d:\d\d:' <<<"$1"; }
same "BDAV folder: how many recordings" "2" "$(rows "$av_folder")"
same "BDAV image: how many recordings"  "2" "$(rows "$av_iso")"
# One row per clip and not one per playlist: the pressed disc names each of
# its three clips in a playlist of its own *and* in the "play all" over the
# lot, and six rows for three clips would be the same recording offered
# twice.
same "BDMV folder: how many recordings" "3" "$(rows "$mv_folder")"
same "BDMV image: how many recordings"  "3" "$(rows "$mv_iso")"

has "BDAV: which dialect" "bdav --" "$av_iso"
has "BDMV: which dialect" "bdmv --" "$mv_iso"
has "BDMV: the disc names itself" "Smartcut Test Disc" "$mv_iso"

names_of() { grep -oP '^[* ]?\s*\d+\s+\S+\s+\K.*?(?=\s+\d+ mark|$)' <<<"$1"; }
want=$'テスト録画 その1 #07「神様的休息日の過ごし方。」\nテスト録画 その2 BS11'
same "BDAV folder: the programmes are named" "$want" "$(names_of "$av_folder")"
same "BDAV image: the programmes are named"  "$want" "$(names_of "$av_iso")"
# A pressed disc names nothing, so the rows are the disc and the clip.
same "BDMV: the rows are named by the disc" \
  $'Smartcut Test Disc 00001\nSmartcut Test Disc 00002\nSmartcut Test Disc 00003' \
  "$(names_of "$mv_iso")"
same "BDAV: the marks are read"  "2" "$(grep -c '2 mark(s)' <<<"$av_iso")"
# Two marks each, from the playlist that plays that clip alone -- not the one
# mark apiece the "play all" carries. A mark is a time on its clip's own
# clock, and only a playlist that plays one clip can be read without knowing
# which of them it meant.
same "BDMV: the marks come from the playlist that plays one clip" \
  "3" "$(grep -c '2 mark(s)' <<<"$mv_iso")"

# Nothing on a disc of twenty second clips clears the five minute mark, so
# what is offered is the fallback: the longest one, and not nothing. The
# threshold itself cannot be exercised by a fixture measured in seconds; it
# is tested in `disc::tests::offers_the_part_of_a_pressed_disc_that_is_the_film`.
same "BDMV: what is offered already ticked" "1" "$(grep -cP '^\*' <<<"$mv_iso")"
same "BDAV: a disc of recordings is all offered" "0" "$(grep -cP '^\*' <<<"$av_iso")"

# --- what a clip carries -------------------------------------------------
# A recorder cuts the language field short, so there is none to show -- and
# none is shown, rather than three bytes of whatever followed.
has "BDAV: the sound is listed by PID" "0x1101  AAC stereo 48kHz" "$av_iso"
has "BDAV: the second sound track too" "0x1103  AAC stereo 48kHz" "$av_iso"
has "BDAV: the captions are listed as what the index says" \
  "0x1102  stream type 0x06" "$av_iso"
has "BDMV: the sound is listed by PID" "0x1100  TrueHD multi 48kHz eng" "$mv_iso"
has "BDMV: and the dub"                "0x1101  TrueHD stereo 48kHz jpn" "$mv_iso"
has "BDMV: the subtitles are named as unusable" \
  "0x1200  PGS eng -- a cut cannot carry this" "$mv_iso"

# --- opening one ---------------------------------------------------------
# Not called `cut`: `md5` below pipes through the real one, and a function of
# that name would be found first.
one() { "$BIN" "$1" --title "$2" --keep 3-9 -o "$3" >"$4" 2>&1; }
one "$AV"     2 "$OUT/av-folder.ts" "$OUT/av-folder.log"
one "$AV_ISO" 2 "$OUT/av-iso.ts"    "$OUT/av-iso.log"
one "$MV"     2 "$OUT/mv-folder.ts" "$OUT/mv-folder.log"
one "$MV_ISO" 2 "$OUT/mv-iso.ts"    "$OUT/mv-iso.log"
"$BIN" "$AV/BDAV/STREAM/00002.m2ts" --keep 3-9 -o "$OUT/plain.ts" >"$OUT/plain.log" 2>&1

md5() { md5sum "$1" 2>/dev/null | cut -d' ' -f1; }
match() { # name a b
  local a; local b
  a=$(md5 "$2"); b=$(md5 "$3")
  if [ -z "$a" ] || [ -z "$b" ]; then
    bad "$1" "$(tail -2 "${4:-/dev/null}")"
  elif [ "$a" = "$b" ]; then
    ok "$1" "${a:0:12}"
  else
    bad "$1" "$2 $a, $3 $b"
  fi
}
match "BDAV: a cut through the disc is the same" \
  "$OUT/av-folder.ts" "$OUT/plain.ts" "$OUT/av-folder.log"
match "BDAV: a cut from inside the image is the same" \
  "$OUT/av-iso.ts" "$OUT/av-folder.ts" "$OUT/av-iso.log"
match "BDMV: a cut from inside the image is the same" \
  "$OUT/mv-iso.ts" "$OUT/mv-folder.ts" "$OUT/mv-iso.log"
# The two discs hold the same stream under different indexes, so a cut of it
# is the same bytes whichever half of the specification described it.
match "the dialect the disc was written in changes nothing" \
  "$OUT/mv-iso.ts" "$OUT/av-iso.ts" "$OUT/mv-iso.log"

# A chapter is a time in the recording, and the recording's clock is not the
# playlist's: the disc counts from zero and the stream does not. The marks
# were written at the IN point and halfway through, so on the recording's own
# clock they are 0 and 10 seconds -- which is where the editor draws them.
same "BDAV: the marks are placed" "marks : 2 [0.000, 10.000]" \
  "$(grep -o 'marks : .*' "$OUT/av-iso.log")"
same "BDMV: the marks are placed" "marks : 2 [0.000, 10.000]" \
  "$(grep -o 'marks : .*' "$OUT/mv-iso.log")"

# The tables the recording carries have to survive being read out of an
# image: this is the pass that reads the transport stream itself, byte by
# byte, rather than through libavformat.
for who in av mv; do
  if grep -q "the broadcast's own tables are not" "$OUT/$who-iso.log"; then
    bad "$who: the broadcast's own tables are kept" \
      "$(grep -o 'note:.*' "$OUT/$who-iso.log")"
  else
    ok "$who: the broadcast's own tables are kept"
  fi
done

# --- naming --------------------------------------------------------------
same "BDAV: the title is announced" \
  "title : テスト録画 その2 BS11" "$(grep '^title :' "$OUT/av-iso.log")"
same "BDAV: the recording is named by its path" \
  "input : $AV_ISO/BDAV/STREAM/00002.m2ts" "$(grep '^input :' "$OUT/av-iso.log")"
same "BDMV: the recording is named by its path" \
  "input : $MV_ISO/BDMV/STREAM/00002.m2ts" "$(grep '^input :' "$OUT/mv-iso.log")"

# --- a disc copied out under somebody else's name ------------------------
#
# The three directories are what makes a disc, not the name of the folder
# around them: `BDAV/` renamed to the programme it holds is still that disc,
# and which dialect it speaks is answered by the playlists inside rather than
# by a name that is now gone.
RENAMED="$OUT/アニメ 2026-08-17"
cp -r "$AV/BDAV" "$RENAMED"
renamed=$("$BIN" "$RENAMED" 2>&1)
same "a renamed disc folder still opens" "2" "$(rows "$renamed")"
has "and is still read as BDAV" "bdav --" "$renamed"
cp -r "$MV/BDMV" "$OUT/season one"
has "a renamed BDMV folder is read as BDMV" "bdmv --" "$("$BIN" "$OUT/season one" 2>&1)"

# --- what is not a disc --------------------------------------------------
notdisc=$("$BIN" "$FX/mpeg2.ts" 2>&1 | head -1)
case "$notdisc" in
  input*) ok "an ordinary recording is not a disc" ;;
  *)      bad "an ordinary recording is not a disc" "$notdisc" ;;
esac

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
