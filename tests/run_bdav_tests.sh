#!/usr/bin/env bash
# A disc this program writes has to be a disc it -- and a player -- can read.
#
# Reading a disc is checked next door, in run_disc_tests.sh, against discs
# real tools wrote. This is the other direction, and it has two halves that
# fail differently:
#
#   * The index has to be readable. `smartcut <folder>` opens what was
#     written and lists it, which is the same reader a recorder's own disc
#     goes through.
#   * The numbers in it have to be true. Nothing in this program reads the
#     entry point map, the arrival times or the clip index's account of the
#     stream -- a player does -- so tests/bdav_index.py stands in for one and
#     checks each of them against the stream they are about.
#
# The metadata half is the reason the feature exists: what a recording says
# about the programme in it has to come out the other side, whether it
# arrived in a broadcast's own tables or in the playlist of the disc the
# recording was read off.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
OUT="${TMPDIR:-/tmp}/smartcut-bdav"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-42s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-42s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
skip() { printf "  SKIP  %-42s %s\n" "$1" "${2:-}"; }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi }
# For fields whose value is hundreds of bytes of ARIB text: say how much
# agreed rather than printing it.
alike() {
  if [ "$2" = "$3" ]; then ok "$1" "${#2} hex digits"; else bad "$1" "differs"; fi
}
has()  { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "no [$2] in [$3]" ;; esac }
field() { echo "$1" | sed -n "s/^$2=//p"; }

rm -rf "$OUT"; mkdir -p "$OUT"

echo "writing a disc of two recordings ..."
"$BIN" "$FX/mpeg2.ts" --keep 0-4 --keep 6-10 --bdav "$OUT/disc" \
  --disc-title "テストディスク" --programme "一本目の番組" >"$OUT/one.log" 2>&1
# The second is given everything a disc's index says, by hand: a stream that
# has been through tools that kept none of its tables still belongs on a disc
# under the channel it came off.
"$BIN" "$FX/mpeg2.ts" --keep 2-8 --bdav "$OUT/disc" \
  --disc-title "テストディスク" --programme "二本目の番組" \
  --channel "衛星第一,161" --made "2026-09-05 22:30:00" \
  --about "見本の番組内容。出演:テスト太郎" >"$OUT/two.log" 2>&1

# --- the disc opens, and says what it was told ---------------------------

list=$("$BIN" "$OUT/disc" 2>/dev/null)
same "the disc holds two recordings" "2 recording(s)" "$(echo "$list" | sed -n 's/.*\(2 recording(s)\).*/\1/p' | head -1)"
has  "the disc's own name comes back" "テストディスク" "$list"
has  "the first recording is named" "一本目の番組" "$list"
has  "the second is numbered after it" "二本目の番組" "$list"
# The second recording was written onto a disc that already held one, and
# adding to a disc must not disturb what is on it.
[ -f "$OUT/disc/BDAV/STREAM/00001.m2ts" ] && [ -f "$OUT/disc/BDAV/STREAM/00002.m2ts" ] \
  && ok "a second run adds to the disc" || bad "a second run adds to the disc"

# --- and every number in the index is true -------------------------------

facts=$(python3 tests/bdav_index.py "$OUT/disc" 2>&1)
same "both playlists are listed in the index" "2" "$(field "$facts" playlists)"
for clip in 00001 00002; do
  same "$clip: the playlist names its clip" "$clip" "$(field "$facts" "$clip.clip")"
  same "$clip: the clip index agrees on the packets" "True" "$(field "$facts" "$clip.packets_agree")"
  same "$clip: and on when it starts and ends" "True" "$(field "$facts" "$clip.times_agree")"
  same "$clip: and on which PID carries the clock" "True" "$(field "$facts" "$clip.pcr_agrees")"
  # The one thing a cut of a broadcast most easily loses: the map has to
  # describe the streams as the stream's own map describes them, or a player
  # goes looking for sound that is declared as something else.
  same "$clip: and on what the streams are" "True" "$(field "$facts" "$clip.streams_agree")"
  # What makes a disc seekable. Every entry point is followed to the packet
  # it names and the picture there is checked against the time it claims.
  same "$clip: every entry point lands on its picture" "0" "$(field "$facts" "$clip.entry_points_wrong")"
  [ "$(field "$facts" "$clip.entry_points")" -gt 0 ] \
    && ok "$clip: the map is not empty" "$(field "$facts" "$clip.entry_points") entry points" \
    || bad "$clip: the map is not empty"
  # The arrival times libavformat writes into a .m2ts are nonsense -- a
  # counter that steps backwards every packet -- and are written again from
  # the stream's own clock. Forwards, and spanning the recording.
  same "$clip: the arrival times never go back" "0" "$(field "$facts" "$clip.arrival_backwards")"
  seconds=$(field "$facts" "$clip.seconds")
  arrival=$(field "$facts" "$clip.arrival_seconds")
  python3 -c "import sys; sys.exit(0 if abs($arrival-$seconds) < 1.0 else 1)" \
    && ok "$clip: they span the recording" "${arrival}s of ${seconds}s" \
    || bad "$clip: they span the recording" "${arrival}s of ${seconds}s"
done

# The chapter points are the one thing on a recorder's disc that a viewer
# uses every time: one at the start of each kept range, which is where the
# commercials were.
same "a mark at each kept range" "2" "$(field "$facts" 00001.marks)"
same "the marks are inside the recording" "True" "$(field "$facts" 00001.mark_in_range)"

# What was given on the command line is what the index says, and what was not
# given is left empty rather than filled with something.
same "the channel it was told about" "161" "$(field "$facts" 00002.channel_number)"
same "and when it was told the recording was made" "20260905223000" \
  "$(field "$facts" 00002.made)"
[ "$(field "$facts" 00002.description_length)" -gt 0 ] \
  && ok "and what it was told the programme was" \
    "$(field "$facts" 00002.description_length) bytes" \
  || bad "and what it was told the programme was"
same "a recording told none of it says none of it" "0" \
  "$(field "$facts" 00001.channel_number)"

# --- what the recording said about itself --------------------------------

echo
echo "the programme's own name, into the disc's index and out again"

# The name a recording arrived with comes out the other side. A recording
# read off a disc carries what its playlist said; one off the air carries
# what the broadcast said; and a name given on the command line is neither
# and beats both.
"$BIN" "$OUT/disc" --title 1 --keep 0-3 --bdav "$OUT/again" >"$OUT/again.log" 2>&1
again=$("$BIN" "$OUT/again" 2>/dev/null)
has "a cut of a disc's recording keeps its name" "一本目の番組" "$again"

# The rest of what a recorder writes down -- the moment, the channel and what
# the programme was about -- only a real recording has: the fixtures are
# streams with no broadcast around them.
#
# The four fields are compared as the bytes that went into the playlist. What
# they say when they are read back as words is what the reader is for, and
# `--title` prints it.
slice() { python3 -c "print(open('$1/BDAV/PLAYLIST/00001.rpls','rb').read()[$2:$3].hex())"; }
made()    { slice "$1" 50 57; }
channel() { slice "$1" 64 88; }    # the number, and the name after it
about()   { slice "$1" 344 1546; } # the description, to the end of the field

if [ -f "$MEDIA/atx.ts" ]; then
  "$BIN" "$MEDIA/atx.ts" --keep 0-3 --bdav "$OUT/air" >"$OUT/air.log" 2>&1
  air=$("$BIN" "$OUT/air" 2>/dev/null)
  name=$(echo "$air" | sed -n 's/^  1  [0-9:.]*  \(.*[^ ]\)  *[0-9]* mark(s)$/\1/p')
  [ -n "$name" ] && ok "a broadcast's programme name is read" "$name" \
    || bad "a broadcast's programme name is read" "$air"
  # Each of the three, out of the broadcast's own tables and into the disc.
  aired=$(made "$OUT/air")
  [ "$aired" != "00000000000000" ] && ok "and when it went out" "$aired" \
    || bad "and when it went out" "nothing was written"
  facts=$(python3 tests/bdav_index.py "$OUT/air" 2>&1)
  # A satellite service is numbered by the three digits a viewer knows it by;
  # `atx.ts` is one, so the field is filled in rather than left at zero.
  [ "$(field "$facts" 00001.channel_number)" -gt 0 ] \
    && ok "the channel it came off" "$(field "$facts" 00001.channel_number)" \
    || bad "the channel it came off" "no number was written"
  [ "$(field "$facts" 00001.channel_length)" -gt 0 ] \
    && ok "and what that channel calls itself" \
    || bad "and what that channel calls itself"
  [ "$(field "$facts" 00001.description_length)" -gt 0 ] \
    && ok "and what the programme was about" \
      "$(field "$facts" 00001.description_length) bytes" \
    || bad "and what the programme was about"

  # Now round the loop: a cut *of that disc* has to arrive with all of it
  # again, this time out of the playlist rather than out of the air.
  "$BIN" "$OUT/air" --title 1 --keep 0-2 --bdav "$OUT/air2" >"$OUT/air2.log" 2>&1
  air2=$("$BIN" "$OUT/air2" 2>/dev/null)
  has  "a cut of it keeps the programme's name" "$name" "$air2"
  same "and the moment it was made" "$aired" "$(made "$OUT/air2")"
  alike "and the channel, byte for byte" "$(channel "$OUT/air")" "$(channel "$OUT/air2")"
  alike "and what it was about" "$(about "$OUT/air")" "$(about "$OUT/air2")"
else
  for what in "a broadcast's programme name is read" "and when it went out" \
              "the channel it came off" "and what that channel calls itself" \
              "and what the programme was about" \
              "a cut of it keeps the programme's name" "and the moment it was made" \
              "and the channel, byte for byte" "and what it was about"; do
    skip "$what" "no atx.ts"
  done
fi

echo
echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
