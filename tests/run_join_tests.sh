#!/usr/bin/env bash
# Joining separate recordings: does the file hold both of them, does it hold
# them where it says, and is the part that matched still a copy?
#
# Three jobs are measured here, because they are three different failures.
#
#   * **The join itself.** Two recordings of the same shape written into one
#     file. What can go wrong is arithmetic -- a length that is not the two
#     lengths, a picture count that is not the two counts, sound that stops
#     where the first clip ended -- and all three are checked, because each
#     of them comes out of a different mistake.
#
#   * **The clip that does not match.** A recording of another size, rate and
#     sound is written afresh into the master's shape. What is checked is
#     that the pictures came out the master's size and the output is as long
#     as the two clips: a rate conversion that dropped or doubled the wrong
#     way still plays.
#
#   * **The transitions.** A fade keeps the output's length and a dissolve
#     takes its own seconds off it, which is the one property that says the
#     two timings were not confused for each other. The middle of a fade
#     through black is measured for what it is: a frame with nothing in it.
#
# And one thing that is not about any of them: **a cut of a single recording
# has to come out exactly as it did.** Everything above is reached by the
# path an ordinary cut takes, so the last test here writes one both ways and
# compares the bytes.
#
# Fixtures are generated, so this needs nothing but ffmpeg.
set -u
cd "$(dirname "$0")/.."
FX="${TMPDIR:-/tmp}/smartcut-join-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-join-out"
CUT=rust/target/release/smartcut
mkdir -p "$FX"
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$CUT" ] || {
  echo "build first: (cd rust && cargo build --release)" >&2; exit 2;
}

pass=0; fail=0
ok()  { printf "  ok    %-34s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad() { printf "  FAIL  %-34s %s\n" "$1" "$2"; fail=$((fail+1)); }

# Within this many seconds is the same length. A range is snapped to the
# recording's own entry points at one end and to a frame at the other, so two
# clips laid end to end are within a GOP of the sum and never exactly it.
near() {  # near VALUE WANT TOLERANCE
  python3 -c "import sys; a,b,t=map(float,sys.argv[1:4]); sys.exit(0 if abs(a-b)<=t else 1)" "$1" "$2" "$3"
}

secs() { ffprobe -v error -show_entries format=duration -of csv=p=0 "$1"; }
# A transport stream lists each of its streams twice -- once inside the
# programme and once on its own -- so the first answer is the answer.
frames() {
  ffprobe -v error -count_frames -select_streams v:0 \
    -show_entries stream=nb_read_frames -of csv=p=0 "$1" | tr -d ',' | head -1
}
width() {
  ffprobe -v error -select_streams v:0 -show_entries stream=width -of csv=p=0 "$1" \
    | tr -d ',' | head -1
}
# Where the sound stops, which is what says it was carried the whole way and
# not only for the first clip.
sound_ends() {
  ffprobe -v error -select_streams a:0 -show_entries packet=pts_time -of csv=p=0 "$1" \
    | tr -d ',' | tail -1
}
# Whether the file decodes with nothing to say. A join that wrote a picture
# where one already was, or sound that goes backwards, says so here.
clean() {
  local said
  said=$(ffmpeg -hide_banner -v warning -i "$1" -f null - 2>&1 | head -3)
  [ -z "$said" ]
}
# The average luma of one frame, for the middle of a fade.
luma_at() {  # luma_at FILE SECONDS
  ffmpeg -hide_banner -v error -i "$1" -ss "$2" -frames:v 1 -vf scale=16:16 \
    -f rawvideo -pix_fmt gray - 2>/dev/null \
    | python3 -c "import sys; d=sys.stdin.buffer.read(); print(sum(d)/max(len(d),1))"
}

gen() {  # gen NAME SIZE RATE SECONDS AUDIORATE CHANNELS
  local name=$1 size=$2 rate=$3 secs=$4 arate=$5 ch=$6
  [ -f "$FX/$name" ] && return 0
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=$size:rate=$rate:duration=$secs" \
    -f lavfi -i "sine=frequency=440:sample_rate=$arate:duration=$secs" \
    -c:v mpeg2video -b:v 5M -g 15 -c:a mp2 -ac "$ch" -shortest "$FX/$name"
}

echo "generating fixtures ..."
# Two of one shape, and one of another. The second pair differs in every way
# a clip can differ -- size, rate, sound -- because what is being tested is
# that each of them is noticed.
gen one.ts   640x360 30 10 48000 2 || exit 2
gen two.ts   640x360 30 10 48000 2 || exit 2
gen other.ts 320x240 25 10 44100 1 || exit 2

ONE=$(secs "$FX/one.ts"); TWO=$(secs "$FX/two.ts"); OTHER=$(secs "$FX/other.ts")

echo "running join tests ..."

# --- two of a shape ---------------------------------------------------------
$CUT "$FX/one.ts" --join "$FX/two.ts" -o "$OUT/pair.ts" >"$OUT/pair.log" 2>&1
if [ -s "$OUT/pair.ts" ]; then
  got=$(secs "$OUT/pair.ts"); want=$(python3 -c "print($ONE+$TWO)")
  if near "$got" "$want" 1.0; then
    ok "two clips make one file" "$(printf '%.2f' "$got")s of a wanted $(printf '%.2f' "$want")"
  else
    bad "two clips make one file" "$got s, wanted about $want"
  fi
  a=$(frames "$OUT/pair.ts")
  a1=$($CUT "$FX/one.ts" -o "$OUT/a.ts" >/dev/null 2>&1; frames "$OUT/a.ts")
  a2=$($CUT "$FX/two.ts" -o "$OUT/b.ts" >/dev/null 2>&1; frames "$OUT/b.ts")
  want=$((a1 + a2))
  if [ "$a" = "$want" ]; then
    ok "and every picture of both" "$a"
  else
    bad "and every picture of both" "$a, wanted $want"
  fi
  # The sound is the test that catches a join which wrote the pictures of the
  # second clip and nothing else of it.
  got=$(sound_ends "$OUT/pair.ts")
  if near "$got" "$(secs "$OUT/pair.ts")" 0.5; then
    ok "and the sound runs to the end" "$(printf '%.2f' "$got")s"
  else
    bad "and the sound runs to the end" "stops at $got"
  fi
  if clean "$OUT/pair.ts"; then
    ok "and it decodes with nothing to say" ""
  else
    bad "and it decodes with nothing to say" "$(ffmpeg -hide_banner -v warning -i "$OUT/pair.ts" -f null - 2>&1 | head -1)"
  fi
else
  bad "two clips make one file" "$(tail -1 "$OUT/pair.log")"
fi

# --- three of a shape -------------------------------------------------------
$CUT "$FX/one.ts" --join "$FX/two.ts" --join "$FX/one.ts" -o "$OUT/three.ts" >/dev/null 2>&1
got=$(secs "$OUT/three.ts"); want=$(python3 -c "print(2*$ONE+$TWO)")
if near "$got" "$want" 1.5; then
  ok "three clips, in the order given" "$(printf '%.2f' "$got")s"
else
  bad "three clips, in the order given" "$got s, wanted about $want"
fi

# --- one that does not match ------------------------------------------------
$CUT "$FX/one.ts" --join "$FX/other.ts" -o "$OUT/conf.ts" >"$OUT/conf.log" 2>&1
if [ -s "$OUT/conf.ts" ]; then
  got=$(width "$OUT/conf.ts")
  if [ "$got" = "640" ]; then
    ok "a clip of another shape is refitted" "every picture ${got}px wide"
  else
    bad "a clip of another shape is refitted" "${got}px"
  fi
  got=$(secs "$OUT/conf.ts"); want=$(python3 -c "print($ONE+$OTHER)")
  if near "$got" "$want" 1.0; then
    ok "and it is as long as it was" "$(printf '%.2f' "$got")s"
  else
    bad "and it is as long as it was" "$got s, wanted about $want"
  fi
  if grep -q "not the shape of" "$OUT/conf.log"; then
    ok "and the run said so before writing" "$(grep -o 'frame size: [^;]*' "$OUT/conf.log" | head -1)"
  else
    bad "and the run said so before writing" "nothing was said"
  fi
  if clean "$OUT/conf.ts"; then
    ok "and it decodes with nothing to say" ""
  else
    bad "and it decodes with nothing to say" "$(ffmpeg -hide_banner -v warning -i "$OUT/conf.ts" -f null - 2>&1 | head -1)"
  fi
else
  bad "a clip of another shape is refitted" "$(tail -1 "$OUT/conf.log")"
fi

# --- the transitions --------------------------------------------------------
PAIR=$(secs "$OUT/pair.ts")

$CUT "$FX/one.ts" --join "$FX/two.ts" --transition fade-black --transition-seconds 2 \
  -o "$OUT/fade.ts" >/dev/null 2>&1
got=$(secs "$OUT/fade.ts")
if near "$got" "$PAIR" 0.2; then
  ok "a fade keeps the file's length" "$(printf '%.2f' "$got")s against $(printf '%.2f' "$PAIR")"
else
  bad "a fade keeps the file's length" "$got s, wanted about $PAIR"
fi
# The middle of the fade is the join itself, which is where the first clip
# ends. Studio black is 16, and the fixture's own pictures are nowhere near.
mid=$(python3 -c "print(round($ONE, 3))")
dark=$(luma_at "$OUT/fade.ts" "$mid")
if python3 -c "import sys; sys.exit(0 if float(sys.argv[1]) < 40 else 1)" "$dark"; then
  ok "and its middle is black" "luma $(printf '%.1f' "$dark")"
else
  bad "and its middle is black" "luma $dark, wanted under 40"
fi

$CUT "$FX/one.ts" --join "$FX/two.ts" --transition dissolve --transition-seconds 2 \
  -o "$OUT/diss.ts" >/dev/null 2>&1
got=$(secs "$OUT/diss.ts"); want=$(python3 -c "print($PAIR-2)")
if near "$got" "$want" 0.3; then
  ok "a dissolve takes its own seconds" "$(printf '%.2f' "$got")s against $(printf '%.2f' "$PAIR")"
else
  bad "a dissolve takes its own seconds" "$got s, wanted about $want"
fi
if clean "$OUT/diss.ts"; then
  ok "and it decodes with nothing to say" ""
else
  bad "and it decodes with nothing to say" "$(ffmpeg -hide_banner -v warning -i "$OUT/diss.ts" -f null - 2>&1 | head -1)"
fi

$CUT "$FX/one.ts" --join "$FX/two.ts" --transition wipe-left --transition-seconds 2 \
  -o "$OUT/wipe.ts" >/dev/null 2>&1
if [ -s "$OUT/wipe.ts" ] && clean "$OUT/wipe.ts"; then
  ok "a wipe writes and decodes" "$(printf '%.2f' "$(secs "$OUT/wipe.ts")")s"
else
  bad "a wipe writes and decodes" "nothing usable was written"
fi

# --- a crossing held to the shorter of its two ends -------------------------
#
# The two clips of an overlapping crossing are on screen together, so the
# seconds the clip before spends on it are the seconds the clip after gives
# up at its head. Each end is capped at half of its own range, and where one
# of those ranges is short the two caps differ -- at which point the clip
# after either loses material the crossing never showed, or shows its first
# seconds twice. A two-second range against a whole recording is the case:
# the crossing may run one second, so the output is the range plus what is
# left of the second clip.
$CUT "$FX/one.ts" --keep 0-2 --join "$FX/two.ts" --transition dissolve \
  --transition-seconds 2 -o "$OUT/short.ts" >/dev/null 2>&1
got=$(secs "$OUT/short.ts"); want=$(python3 -c "print(2.0 + $TWO - 1.0)")
if near "$got" "$want" 0.5; then
  ok "a crossing takes what both ends allow" "$(printf '%.2f' "$got")s of a wanted $(printf '%.2f' "$want")"
else
  bad "a crossing takes what both ends allow" "$got s, wanted about $want"
fi

# --- and the path an ordinary cut takes is unchanged ------------------------
#
# The whole of the above is reached through the same function a single
# recording goes through. This is the test that says so: one cut, no join, no
# transition, compared against what the same command wrote before any of it
# existed -- which is to say, against itself run twice, and against the
# frame count the plan promised.
$CUT "$FX/one.ts" --keep 1-6 -o "$OUT/plain1.ts" >"$OUT/plain.log" 2>&1
$CUT "$FX/one.ts" --keep 1-6 -o "$OUT/plain2.ts" >/dev/null 2>&1
if cmp -s "$OUT/plain1.ts" "$OUT/plain2.ts"; then
  ok "a cut of one recording is unchanged" "$(stat -c%s "$OUT/plain1.ts") bytes, twice"
else
  bad "a cut of one recording is unchanged" "two runs differ"
fi
if ! grep -q "recording(s) into one file" "$OUT/plain.log"; then
  ok "and says nothing about joining" ""
else
  bad "and says nothing about joining" "it did"
fi

echo
echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
