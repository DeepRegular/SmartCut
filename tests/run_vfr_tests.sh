#!/usr/bin/env bash
# Variable frame rate: is every picture still where the recording had it,
# and does a range still last as long as it was asked for?
#
# A picture is on screen until the next picture replaces it. Where pictures
# come at a constant rate that is always one frame away, so the two facts
# about a picture -- when it arrives and how long it stays -- are the same
# number and nothing ever had to tell them apart. A variable-rate recording
# holds a picture for as long as nothing changed: a screen capture holds one
# for minutes, and a phone drops to half rate in the dark. There the two
# differ, and three things go wrong if only the first is counted.
#
#   * **Every seam loses the difference.** A segment that ended on a held
#     picture used to last until that picture's coded length ran out rather
#     than until the next picture arrived, so everything after the seam moved
#     early -- measured at 46 pictures of 505 on one recording and 598 of 628
#     on another, each a whole frame out.
#   * **A range ends early.** The last picture of a range is held to the end
#     of it, and a thirty-second range whose last picture arrived two seconds
#     before the end came out twenty-eight seconds long.
#   * **A range inside a hold has no picture of its own.** It used to stop
#     the run outright with `no pictures decoded`; what belongs there is the
#     picture that was already up.
#
# And a recording can vary the other way, which is what the recordings people
# actually have do: a 23.976 programme with a second or two of 59.94 in it for
# the credits. There the pictures of the fast stretch are 16.7 ms apart and a
# field of the output timeline was 20 ms, so two of them landed on the same
# place and the second was dropped -- and the timeline itself was built on the
# average of the two rates, which is a rate nothing in the recording was ever
# coded at. Measured on one: 11 pictures dropped from a three-range cut and
# 227 gaps wrong, the worst by 45 ms.
#
# The fixtures are made here, and the ranges are aimed at the holds rather
# than put at round numbers: a seam has to land on a held picture for any of
# the above to show at all.
set -u
cd "$(dirname "$0")/.."
FX="${TMPDIR:-/tmp}/smartcut-vfr-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-vfr-out"
CUT=rust/target/release/smartcut
mkdir -p "$FX"
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$CUT" ] || {
  echo "build first: (cd rust && cargo build --release)" >&2; exit 2;
}
ffmpeg -hide_banner -encoders 2>/dev/null | grep -q " libvpx-vp9 " || {
  echo "needs ffmpeg with libvpx-vp9" >&2; exit 2;
}

pass=0; fail=0
ok()  { printf "  ok    %-26s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad() { printf "  FAIL  %-26s %s\n" "$1" "$2"; fail=$((fail+1)); }

echo "generating fixtures in $FX ..."
# The control: the same picture at a constant rate, to say that none of what
# follows moved constant-rate material.
if [ ! -f "$FX/cfr.webm" ]; then
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=320x240:rate=30:duration=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=60" \
    -shortest -c:v libvpx-vp9 -b:v 400k -g 60 -keyint_min 60 \
    -deadline good -cpu-used 6 -row-mt 1 -c:a libopus -b:a 64k "$FX/cfr.webm"
fi
# A second of motion, a second of a frozen picture, over and over. `select`
# throws away the frames of the frozen stretches and `fps` puts the last one
# back in their place, so what gets coded is a picture repeated; `mpdecimate`
# then takes the repeats out again and leaves the rest where they always
# were. What is left is a recording holding a picture for a second at a time.
# Holds that often are what puts one at a segment's end wherever the range
# bounds are put, which is the case the seam arithmetic was wrong about.
if [ ! -f "$FX/vfr.webm" ]; then
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=320x240:rate=30:duration=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=60" \
    -shortest -filter:v "select=lt(mod(t\,2)\,1),fps=30" \
    -c:v libvpx-vp9 -b:v 400k -g 60 -keyint_min 60 \
    -deadline good -cpu-used 6 -row-mt 1 -c:a libopus -b:a 64k "$FX/held.webm"
  # An entry point exactly where the motion starts again, which is to say
  # exactly after a held picture. A copy can only end on an entry point, so
  # this is what puts a held picture at the end of a segment wherever the
  # range bounds fall -- and a held picture at the end of a segment is the
  # whole of what the seam arithmetic used to get wrong. Left to the
  # encoder's own spacing the entry points drift through the pattern and the
  # case turns up or does not, which is not a test.
  ffmpeg -hide_banner -loglevel error -y -i "$FX/held.webm" \
    -vf mpdecimate -fps_mode vfr -c:v libvpx-vp9 -b:v 400k \
    -force_key_frames "expr:gte(t,n_forced*2)" \
    -deadline good -cpu-used 6 -row-mt 1 -c:a copy "$FX/vfr.webm"
  rm -f "$FX/held.webm"
fi

# What the fixture came out as, and a hold well into it for the ranges to be
# aimed at. Asked rather than assumed: where the pictures land is the
# encoder's business and the test has no business guessing it.
read -r NPIC LONGEST HOLD_AT HOLD_FOR <<VFRPROBE
$(python3 tests/vfr_holds.py "$FX/vfr.webm")
VFRPROBE
echo "vfr fixture: $NPIC pictures, longest hold ${LONGEST}s, aiming at the ${HOLD_FOR}s one at ${HOLD_AT}s"
awk -v h="$LONGEST" 'BEGIN{ exit !(h > 0.5) }' || {
  echo "the fixture did not come out variable enough to test with" >&2; exit 2;
}

# A 23.976 recording with bursts of 59.94 in it, which is what a downloaded
# programme looks like: coded at 120 and thinned to every fifth picture, or
# every second picture for the last three tenths of every twenty seconds. The
# timestamps are left where they fall, so the container ends up declaring 24
# and averaging over it -- which is the only thing SmartCut can see before it
# writes a frame, and what it now acts on.
if [ ! -f "$FX/dense.mp4" ]; then
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=320x240:rate=120:duration=40" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=40" \
    -shortest -filter:v "select=if(lt(mod(t\,20)\,19.7)\,not(mod(n\,5))\,not(mod(n\,2)))" \
    -fps_mode passthrough -c:v libx264 -preset veryfast -g 48 -keyint_min 48 \
    -sc_threshold 0 -b:v 600k -c:a aac -b:a 96k "$FX/dense.mp4"
fi

duration() { ffprobe -v error -show_entries format=duration -of csv=p=0 "$1"; }

# run <name> <fixture> <in> <out>
run() {
  local name=$1 src=$FX/$2 a=$3 b=$4 made=$OUT/${1// /_}.webm
  local err
  err=$("$CUT" "$src" --keep "$a-$b" -o "$made" 2>&1) || {
    bad "$name" "the cut failed: $(printf '%s' "$err" | tail -1)"; return;
  }
  ffmpeg -v error -c:v libvpx-vp9 -i "$made" -f null - 2>/dev/null || {
    bad "$name" "the output does not decode"; return;
  }
  local said
  said=$(python3 tests/vfr_placed.py "$src" "$made" "$a" "$b") || {
    bad "$name" "could not read the pictures back"; return;
  }
  case "$said" in
    ok\ *) ;;
    *) bad "$name" "${said#bad }"; return ;;
  esac
  local dur
  dur=$(duration "$made")
  awk -v d="$dur" -v a="$a" -v b="$b" 'BEGIN{ exit !(d >= (b-a) - 0.05 && d <= (b-a) + 0.10) }' || {
    bad "$name" "the cut lasts ${dur}s where $(awk -v a="$a" -v b="$b" 'BEGIN{printf "%.3f", b-a}')s was asked for"
    return
  }
  ok "$name" "${said#ok }, ${dur}s"
}

echo "running tests ..."
run "vfr mid-file"   vfr.webm 20 50
run "vfr from zero"  vfr.webm  0 20
run "cfr control"    cfr.webm 20 50

# The other way round: pictures closer together than the declared rate. The
# count is what matters here -- two pictures with nowhere to go used to become
# one -- and so is the spacing, which a timeline built on the average got
# wrong for every picture in the recording, not only the fast ones.
dense=$OUT/dense.mp4
if err=$("$CUT" "$FX/dense.mp4" --keep 5-35 -o "$dense" 2>&1); then
  said=$(python3 tests/vfr_gaps.py "$FX/dense.mp4" "$dense" 5 35)
  case "$said" in
    ok\ *) ok "faster than declared" "${said#ok }" ;;
    *) bad "faster than declared" "${said#bad }" ;;
  esac
else
  bad "faster than declared" "$(printf '%s' "$err" | tail -1)"
fi

# A range that *ends* inside a hold: its last picture stays up past the end
# of the range, so the range lasts as long as it was asked for only if that
# hold is counted.
run "ends inside a hold" vfr.webm 8 "$(awk -v a="$HOLD_AT" 'BEGIN{printf "%.3f", a+0.5}')"
# And one that *begins* inside a hold and runs long enough to have a copy in
# the middle, so both its seams sit on held pictures.
run "begins inside a hold" vfr.webm \
  "$(awk -v a="$HOLD_AT" 'BEGIN{printf "%.3f", a+0.4}')" \
  "$(awk -v a="$HOLD_AT" 'BEGIN{printf "%.3f", a+14.4}')"

# A range entirely inside one hold has no picture of its own. What belongs
# there is the picture that was already up, shown for the whole of it.
INSIDE_A=$(awk -v a="$HOLD_AT" -v h="$HOLD_FOR" 'BEGIN{ printf "%.3f", a + h * 0.25 }')
INSIDE_B=$(awk -v a="$HOLD_AT" -v h="$HOLD_FOR" 'BEGIN{ printf "%.3f", a + h * 0.75 }')
made=$OUT/inside.webm
want=$(awk -v a="$INSIDE_A" -v b="$INSIDE_B" 'BEGIN{ printf "%.3f", b-a }')
if err=$("$CUT" "$FX/vfr.webm" --keep "$INSIDE_A-$INSIDE_B" -o "$made" 2>&1); then
  n=$(ffprobe -v error -select_streams v -count_frames \
      -show_entries stream=nb_read_frames -of csv=p=0 "$made")
  d=$(duration "$made")
  if [ "$n" = "1" ] && awk -v d="$d" -v w="$want" 'BEGIN{ exit !(d >= w-0.05 && d <= w+0.10) }'; then
    ok "range inside one hold" "1 picture held for ${d}s (${want}s asked for)"
  else
    bad "range inside one hold" "$n picture(s), ${d}s, ${want}s asked for"
  fi
else
  bad "range inside one hold" "$(printf '%s' "$err" | tail -1)"
fi

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
