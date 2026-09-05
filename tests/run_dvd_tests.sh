#!/usr/bin/env bash
# A DVD, read as a folder and as an image, has to give the same answers --
# and the same answers as the stream it is made of.
#
# Two discs are built here out of the ordinary fixtures. The first is what a
# DVD normally is: one title set's stream, written in two files because the
# format made discs write it in pieces, with a title over all of it and a
# shorter one over the same stream less its last cell. The second is the awkward
# one: a title whose two halves were multiplexed separately, so its clock
# starts again in the middle -- which is a real thing an authoring tool does,
# and the reason a DVD title is not always one row.
#
# What is claimed, and so what is checked: a title is a run of sectors of one
# stream however many files that stream is written in; the chapters come off
# the disc's own tables and land on the stream's own clock; and cutting a
# title changes nothing about the pictures, whether the disc is a folder, an
# image, or neither.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-dvd"
ONE="$OUT/one"
ONE_ISO="$OUT/one.iso"
TWO="$OUT/two"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }
MKISO=$(command -v genisoimage || command -v mkisofs || true)
[ -n "$MKISO" ] || { echo "needs genisoimage (apt install genisoimage)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-44s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-44s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { # name expected actual
  if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi
}
has()  { # name needle haystack
  case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "no [$2] in [$3]" ;; esac
}

echo "building two discs in $OUT ..."
rm -rf "$OUT"; mkdir -p "$ONE/VIDEO_TS" "$TWO/VIDEO_TS"

# A DVD's stream is an MPEG program stream with a navigation pack opening
# every VOBU, which is what ffmpeg's `dvd` muxer writes.
# The sound is taken twice, as two streams. A disc with one of everything
# would let an index that declared one of everything pass without having read
# anything, and what is being checked here is that the index is read.
vob() { # <out> [extra ffmpeg args ...]
  local out=$1; shift
  ffmpeg -hide_banner -loglevel error -y -i "$FX/mpeg2.ts" \
    -map 0:v:0 -map 0:a:0 -map 0:a:0 "$@" -c copy -f dvd "$out" || exit 2
}

# --- the ordinary disc ---------------------------------------------------
# One stream, written in two files. The split is at a sector because that is
# the only thing the format asks of it -- a real disc splits at a gigabyte,
# which this fixture is a long way short of.
vob "$OUT/whole.vob"
WHOLE=$(stat -c%s "$OUT/whole.vob")
HALF=$(( WHOLE / 2 / 2048 * 2048 ))
dd if="$OUT/whole.vob" of="$ONE/VIDEO_TS/VTS_01_1.VOB" bs=2048 count=$((HALF / 2048)) status=none
dd if="$OUT/whole.vob" of="$ONE/VIDEO_TS/VTS_01_2.VOB" bs=2048 skip=$((HALF / 2048)) status=none

# Four cells, a title over all four and a title over the first three: a
# programme, and the same programme without its ending, which is the shape
# the disc this was written against has.
INDEX=$(python3 tests/dvd_index.py "$ONE/VIDEO_TS" 4 4 3) || exit 2
# The navigation packs were rewritten in place, so the plain stream this is
# compared against has to be the pieces as they now stand.
cat "$ONE/VIDEO_TS/VTS_01_1.VOB" "$ONE/VIDEO_TS/VTS_01_2.VOB" > "$OUT/whole.vob"

START=$(awk '/^start /{print $2}' <<<"$INDEX")
T1=$(awk '/^title 1 /{print $3}' <<<"$INDEX")
T2=$(awk '/^title 2 /{print $3}' <<<"$INDEX")
LAST_SECTOR=$(awk '/^cell 4 /{print $4}' <<<"$INDEX")
CELL3_END=$(awk '/^cell 3 /{print $4}' <<<"$INDEX")
CHAPTERS=$(awk '/^chapters /{$1=""; print}' <<<"$INDEX")

"$MKISO" -quiet -udf -V SMARTCUT_DVD -o "$ONE_ISO" "$ONE" || exit 2

# --- the disc whose title has two clocks ---------------------------------
# The second half was multiplexed on its own, a thousand seconds along, so
# the timestamps start again where the two meet. Both halves hold the same
# number of VOBUs, so two cells cut on the seam.
vob "$OUT/a.vob"
vob "$OUT/b.vob" -output_ts_offset 1000
cat "$OUT/a.vob" "$OUT/b.vob" > "$TWO/VIDEO_TS/VTS_01_1.VOB"
python3 tests/dvd_index.py "$TWO/VIDEO_TS" 2 2 >/dev/null || exit 2

# The picture is only ever named by the chooser, which the list does not
# draw; `discdiag` is the one that prints every track the index declares.
# Built here rather than in the middle of a check, so that a first run does
# not look like a hang.
(cd rust && cargo build -q --release --example discdiag) || exit 2
rich() { (cd rust && cargo run -q --release --example discdiag -- "$1" 2>&1); }

echo "running tests ..."

one_folder=$("$BIN" "$ONE" 2>&1)
one_iso=$("$BIN" "$ONE_ISO" 2>&1)
two_folder=$("$BIN" "$TWO" 2>&1)

# --- what the index says -------------------------------------------------
rows() { grep -cP '^[* ]?\s*\d+\s+\d\d:\d\d:' <<<"$1"; }
same "folder: how many recordings" "2" "$(rows "$one_folder")"
same "image: how many recordings"  "2" "$(rows "$one_iso")"
has  "it is read as a DVD" "dvd -- one" "$one_folder"
has  "and so is the image" "dvd -- one" "$one_iso"

# The two titles are the whole stream and the whole stream less its last
# cell, so they are two rows and not one.
hms() { python3 -c "import sys;t=float(sys.argv[1]);print('%02d:%02d:%06.3f'%(t//3600,t//60%60,t%60))" "$1"; }
has "the long title's length is the index's"  "$(hms "$T1")" "$one_folder"
has "the short title's length is the index's" "$(hms "$T2")" "$one_folder"

# Two titles that named the same stretch would be one row; these do not. A
# title's name is the stream it plays and the sectors of it, and the sectors
# are the disc's own -- counted across both files, not within either.
input_of() { "$BIN" "$ONE" --title "$1" 2>&1 | grep -m1 '^input'; }
has "the long title is the whole stream"  "VTS_01_1.VOB@0-$LAST_SECTOR" "$(input_of 1)"
has "the short title stops a cell early"  "VTS_01_1.VOB@0-$CELL3_END"   "$(input_of 2)"

# --- what the index says it carries --------------------------------------
has "the picture is read out of the index" "MPEG-2 720x480 NTSC 4:3" "$(rich "$ONE")"
has "the first sound track is too"         "0x00c0  MPEG-1 audio 2ch 48kHz ja" "$one_folder"
has "and the second, on its own id"        "0x00c1  MPEG-1 audio 2ch 48kHz ja" "$one_folder"

# --- the chapters --------------------------------------------------------
# One program per cell, so four chapters, and each of them where the cells
# before it end. These are the times the editor puts down as keyframes.
marks() { grep -m1 '^marks' <<<"$1" | sed 's/.*\[//;s/\].*//'; }
got=$(marks "$("$BIN" "$ONE" --title 1 2>&1)")
# Within a frame, and no closer. What the reader prints is the chapter on the
# stream's own clock less the container's start, and those are two different
# measurements of the same instant: the navigation pack times the pictures,
# and the container starts at whichever stream begins first -- the sound, by
# ten milliseconds, on a stream ffmpeg multiplexed.
if python3 - "$got" $CHAPTERS <<'EOF'
import sys
got = [float(x) for x in sys.argv[1].split(",")]
want = [float(x) for x in sys.argv[2:]]
ok = len(got) == len(want) and all(abs(a - b) < 1 / 29.97 for a, b in zip(got, want))
sys.exit(0 if ok else 1)
EOF
then ok "the chapters are the disc's own" "$got"
else bad "the chapters are the disc's own" "want $CHAPTERS, got $got"
fi

# The navigation pack times the pictures; the container begins wherever the
# first of its streams does. Within a frame of each other, and that is all
# that can be claimed.
start_of() { ffprobe -v error -show_entries format=start_time -of default=nw=1:nk=1 "$1"; }
CONTAINER=$(start_of "$OUT/whole.vob")
if python3 -c "import sys; sys.exit(0 if abs(float(sys.argv[1])-float(sys.argv[2])) < 1/29.97 else 1)" \
     "$START" "$CONTAINER"; then
  ok "the navigation pack agrees with the container" "$START vs $CONTAINER"
else
  bad "the navigation pack agrees with the container" "$START vs $CONTAINER"
fi

# --- the cut -------------------------------------------------------------
# The claim the whole feature makes: reading a title out of a folder, out of
# an image, or reading the stream itself, are the same read.
cut_md5() { # <input> <out> [args...]
  local src=$1 out=$2; shift 2
  "$BIN" "$src" -o "$OUT/$out" --keep 2.0-8.0 "$@" >/dev/null 2>&1
  md5sum "$OUT/$out" 2>/dev/null | cut -c1-12
}
plain=$(cut_md5 "$OUT/whole.vob" plain.ts)
folder=$(cut_md5 "$ONE" folder.ts --title 1)
image=$(cut_md5 "$ONE_ISO" image.ts --title 1)
same "a cut of the plain stream and of the folder" "$plain" "$folder"
same "a cut of the folder and of the image"        "$folder" "$image"
[ -n "$plain" ] || bad "the cut produced something" "no output"

# And it is a cut, not a re-encode: a range this size lands on the copy path
# almost end to end.
copied=$("$BIN" "$ONE" --title 1 --keep 2.0-8.0 2>&1 | grep -oP 'copied \K[0-9.]+s \([0-9.]+%\)' | head -1)
has "and it was copied, not re-encoded" "%" "$copied"
ok  "how much was copied" "$copied"

# Every picture in the range comes out. A program stream leaves the
# presentation time off a quarter of its pictures, and skipping those is what
# a cut of one used to do.
frames() { ffprobe -v error -count_frames -select_streams v \
  -show_entries stream=nb_read_frames -of csv=p=0 "$1" | head -1 | tr -d ','; }
want_frames=$(python3 -c "print(round(6.0 * 30000/1001))")
got=$(frames "$OUT/folder.ts")
if [ "$got" -ge $((want_frames - 2)) ] && [ "$got" -le $((want_frames + 2)) ]; then
  ok "six seconds is six seconds of pictures" "$got frames"
else
  bad "six seconds is six seconds of pictures" "want ~$want_frames, got $got"
fi

# --- the disc whose title has two clocks ---------------------------------
same "a title with two clocks is two rows" "2" "$(rows "$two_folder")"
has  "and says which piece each is"        "(1/2)" "$two_folder"
has  "and how many there are"              "(2/2)" "$two_folder"
# Each piece begins where its own clock does, not where the title's does.
two_a=$("$BIN" "$TWO" --title 1 2>&1)
two_b=$("$BIN" "$TWO" --title 2 2>&1)
a_start=$(grep -oP 'start=\K[0-9.]+' <<<"$two_a")
a_dur=$(grep -oP 'dur=\K[0-9.]+' <<<"$two_a")
b_start=$(grep -oP 'start=\K[0-9.]+' <<<"$two_b")
# The second piece begins before the first has finished, which no clock does:
# it is a different clock, and that is the whole of why these are two rows.
if python3 -c "import sys
a, d, b = (float(x) for x in sys.argv[1:])
sys.exit(0 if b < a + d - 1 else 1)" "$a_start" "$a_dur" "$b_start"; then
  ok "the second piece is on its own clock" "$a_start +${a_dur}s, then $b_start"
else
  bad "the second piece is on its own clock" "$a_start +${a_dur}s, then $b_start"
fi

# --- what is not a DVD ---------------------------------------------------
same "an ordinary recording is not a disc" "0" \
  "$("$BIN" "$FX/mpeg2.ts" 2>&1 | grep -c 'recording(s)')"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
