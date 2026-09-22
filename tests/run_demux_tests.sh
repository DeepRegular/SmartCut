#!/usr/bin/env bash
# Does the reader get every packet the recording holds?
#
# Not always. libavformat's Matroska demuxer requires a WebVTT block to be
# `identifier\nsettings\ntext`, and one that is not makes it give up on the
# cluster the block sits in and resume at the next thing that looks like a
# top-level element -- the real next cluster, which begins on a key picture,
# or a false match inside coded picture data. Everything in between is
# dropped, from every stream it was delivering. The packets that do arrive
# are whole and correctly timed, so nothing downstream can tell: playback
# stops part way, the film strip has stretches with no picture in them, the
# frame rate reads as varying, and a cut writes fewer frames than it planned.
#
# Recordings off a video site are written this way, and 42% of their pictures
# and sound went missing before `input::keep_only` was added. What that does
# is say which streams a reader reads, so the blocks it cannot parse are
# never parsed -- the same thing `ffmpeg -map 0:v` does.
#
# The fixture is made here rather than shipped: an ordinary file is written
# with ffmpeg, and then the two newlines in front of each cue's text are
# overwritten with bytes that are not newlines. Nothing else changes, so the
# element sizes still add up and the file is a Matroska file throughout --
# only its subtitle blocks are now the shape libavformat rejects.
set -u
cd "$(dirname "$0")/.."
FX="${TMPDIR:-/tmp}/smartcut-demux-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-demux-out"
CUT=rust/target/release/smartcut
DIAG=rust/target/release/examples/demuxdiag
mkdir -p "$FX"
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$CUT" ] || {
  echo "build first: (cd rust && cargo build --release)" >&2; exit 2;
}
[ -x "$DIAG" ] || {
  echo "build first: (cd rust && cargo build --release --example demuxdiag)" >&2; exit 2;
}

pass=0; fail=0
ok()   { printf "  ok    %-30s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-30s %s\n" "$1" "$2"; fail=$((fail+1)); }
skip() { printf "  SKIP  %-30s %s\n" "$1" "$2"; }

# 20 seconds, a key picture every two, and a cue inside every other cluster.
# The cues sit early in their clusters so that what a resync skips is most of
# one: a cue at the very end of a cluster would cost nothing and prove
# nothing.
echo "generating fixtures in $FX ..."
{
  echo "WEBVTT"
  echo
  for i in $(seq 0 9); do
    printf '00:00:%02d.000 --> 00:00:%02d.500\nCUE%04d\n\n' "$((i * 2))" "$((i * 2 + 1))" "$i"
  done
} > "$FX/cues.vtt"

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=320x180:rate=30:duration=20" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=20" \
  -i "$FX/cues.vtt" -map 0:v -map 1:a -map 2:s \
  -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -b:v 800k \
  -c:a aac -c:s webvtt -shortest "$FX/vtt.mkv" || {
  echo "could not write the fixture" >&2; exit 2;
}

python3 - "$FX/vtt.mkv" "$FX/vtt-unparsable.mkv" <<'PY'
import sys

# ffmpeg writes a cue's block as `identifier\nsettings\ntext`, and with
# neither an identifier nor settings that is two newlines and then the text.
# Overwriting the newlines with anything else is what libavformat cannot
# read: same length, same element sizes, same file everywhere else.
src, dst = sys.argv[1], sys.argv[2]
data = bytearray(open(src, "rb").read())
n, at = 0, 0
while True:
    i = bytes(data).find(b"\n\nCUE", at)
    if i < 0:
        break
    data[i] = data[i + 1] = ord("x")
    n += 1
    at = i + 1
open(dst, "wb").write(bytes(data))
if n != 10:
    sys.exit(f"patched {n} cue(s), wanted 10")
PY
[ -f "$FX/vtt-unparsable.mkv" ] || { echo "could not patch the fixture" >&2; exit 2; }

# How many pictures the container holds. Asked of the plain fixture only:
# the patch changes twenty bytes inside subtitle payloads and not one byte of
# the pictures, so the two files hold the same ones -- and `ffprobe` is no
# more able to read the patched one than anything else that leaves every
# stream switched on. That the two cuts come out identical is checked below,
# which is what makes this figure the right one for both.
held() {
  ffprobe -v error -select_streams v -count_packets \
    -show_entries stream=nb_read_packets -of csv=p=0 "$1" 2>/dev/null
}
frames() {
  ffprobe -v error -select_streams v -count_frames \
    -show_entries stream=nb_read_frames -of csv=p=0 "$1" 2>/dev/null
}
# The pictures of a file, as one hash of their payloads in order. A muxer
# renumbers and moves everything; what it does not touch is the payload.
pictures() {
  ffprobe -v error -select_streams v -show_packets -show_data_hash md5 \
    -of csv=p=0 -show_entries packet=data_hash "$1" 2>/dev/null | md5sum | cut -d' ' -f1
}

echo "running tests ..."

# First, that the fixture is the fixture: the plain file loses nothing and the
# patched one loses a great deal. Without this the tests below would pass and
# say nothing at all about this program.
if "$DIAG" "$FX/vtt.mkv" >/dev/null 2>&1; then
  ok "a readable cue costs nothing" "every stream hands over the same count either way"
else
  bad "a readable cue costs nothing" "the plain fixture already loses packets"
fi

# Not a failure if the loss has gone: a libavformat that reads these blocks
# is the fault being fixed where it lives, and then there is nothing here to
# guard against. It is worth saying out loud, though, because everything
# below it becomes a test of nothing.
report=$("$DIAG" "$FX/vtt-unparsable.mkv" 2>/dev/null)
if [ $? -ne 0 ]; then
  lost=$(printf '%s' "$report" | awk '$2=="video"{print $6, $7}')
  ok "an unparsable cue costs the lot" "video loses $lost when every stream is left on"
else
  skip "an unparsable cue costs the lot" "this libavformat reads them: nothing below is being tested"
fi

# And now what it is all for: the cut reads the same file and writes every
# picture of it.
want=$(held "$FX/vtt.mkv")

for name in vtt vtt-unparsable; do
  made=$OUT/$name-cut.mkv
  if ! "$CUT" "$FX/$name.mkv" --keep 0-20 -o "$made" >"$OUT/$name.log" 2>&1; then
    bad "$name: the cut runs" "$(tail -1 "$OUT/$name.log")"
    continue
  fi
  got=$(frames "$made")
  if [ "$got" = "$want" ]; then
    ok "$name: the cut writes them all" "$got of $want pictures"
  else
    bad "$name: the cut writes them all" "$got of $want pictures"
  fi
  # A run of pictures the reader never saw reads as pictures held on screen,
  # and that is what the rate check calls variable. It is the same fault
  # wearing a different hat, and worth its own line: it is what the editor
  # shows a person.
  if grep -q "the pictures vary" "$OUT/$name.log"; then
    bad "$name: the rate is not varying" "read as variable"
  else
    ok "$name: the rate is not varying" "$(sed -n 's/.*[0-9]x[0-9]* \([0-9.]*fps\).*/\1/p' "$OUT/$name.log" | head -1)"
  fi
done

# The two cuts are the same cut. Nothing but the subtitle payloads differs
# between the fixtures, so anything the reader dropped out of one of them
# would show up here as a different set of pictures.
plain=$(pictures "$OUT/vtt-cut.mkv")
patched=$(pictures "$OUT/vtt-unparsable-cut.mkv")
if [ -n "$plain" ] && [ "$plain" = "$patched" ]; then
  ok "both cuts are the same pictures" "$plain"
else
  bad "both cuts are the same pictures" "$plain vs $patched"
fi

# The index too: every key picture of the recording, not only the ones a
# resync happened to land on.
for name in vtt vtt-unparsable; do
  keys=$(ffprobe -v error -select_streams v -skip_frame nokey -count_frames \
         -show_entries stream=nb_read_frames -of csv=p=0 "$FX/$name.mkv")
  found=$(sed -n 's/.*[^0-9]\([0-9]*\) access points.*/\1/p' "$OUT/$name.log" | head -1)
  if [ "$found" = "$keys" ]; then
    ok "$name: the index finds them all" "$found of $keys access points"
  else
    bad "$name: the index finds them all" "$found of $keys access points"
  fi
done

echo
echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
