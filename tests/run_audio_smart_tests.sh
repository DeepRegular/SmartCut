#!/usr/bin/env bash
# Smart rendering, codec by codec.
#
# Smart mode re-encodes the frames a boundary falls inside and copies every
# other frame. Whether that can be done at all turns on one thing: a packet
# the encoder makes has to cover exactly the samples the frame it replaces
# covered. AAC's encoder delays its output by a whole frame, so its packets
# land on the recording's own grid and always did. **Every other encoder
# here delays by part of a frame** -- AC-3 by 256 samples of 1536, MP2 by
# 481 of 1152 -- and its packets used to be discarded, which left those
# codecs copied, boundaries and all.
#
# They are fed a lead-in now (see `audio::lead_in`), so this asks the same
# three questions of each codec:
#
#   * were the boundary frames re-encoded at all,
#   * is what lies past the cut inside the last of them silent, and
#   * is the sound still where it was -- which is the question a lead-in got
#     wrong would answer badly, since a patch built from the wrong samples
#     lands the whole frame somewhere else.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-audio-smart"
mkdir -p "$OUT" "$FX"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-46s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-46s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

# How many samples each codec puts in a frame, which is the unit everything
# below is measured in.
frame_of_aac=1024; frame_of_ac3=1536; frame_of_eac3=1536; frame_of_mp2=1152

# A tone that never stops, so that anything left past the cut is audible.
# Two channels of it, at a level nothing here can mistake for silence.
if [ ! -f "$FX/tone.ts" ]; then
  echo "generating the tone fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=20" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=20" \
    -filter_complex "[1:a]aformat=channel_layouts=stereo,volume=0.5[a]" \
    -map 0:v -map "[a]" -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a pcm_s16le -f nut "$FX/tone.nut" || exit 2
  ffmpeg -hide_banner -loglevel error -y -i "$FX/tone.nut" \
    -c:v copy -c:a aac -b:a 192k -f mpegts "$FX/tone.ts" || exit 2
fi
# And the impulse fixture the sync suite uses, in a transport stream so that
# every codec below can be written into one.
if [ ! -f "$FX/clicks.nut" ]; then
  echo "generating the click fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=30" \
    -f lavfi -i "aevalsrc=if(lt(mod(t\,0.5)\,0.002)\,0.9\,0):s=48000:d=30" -shortest \
    -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a pcm_s16le -f nut "$FX/clicks.nut" || exit 2
fi

# The fixture written with one codec's sound, in whichever container the
# name asks for.
clip() { # <source.nut> <codec> <out.ts|out.mp4>
  local fmt=mpegts
  [ "${3##*.}" = mp4 ] && fmt=mp4
  ffmpeg -hide_banner -loglevel error -y -i "$1" -c:v copy -c:a "$2" -b:a 256k \
    -ac 2 -ar 48000 -f "$fmt" "$3"
}

echo "running audio smart rendering tests ..."
for codec in aac ac3 eac3 mp2; do
  eval "frame=\$frame_of_$codec"
  src="$OUT/tone-$codec.ts"
  clip "$FX/tone.nut" "$codec" "$src" || { bad "$codec fixture" "ffmpeg"; continue; }

  # One range, both edges off the frame grid on purpose.
  for mode in copy smart; do
    SMARTCUT_DEBUG=1 "$BIN" "$src" --keep 1.3-11.7 --audio-mode "$mode" \
      -o "$OUT/$codec-$mode.ts" >"$OUT/$codec-$mode.log" 2>&1 \
      || bad "$codec $mode ran" "$(tail -2 "$OUT/$codec-$mode.log")"
  done

  # --- were the boundary frames re-encoded at all -------------------------
  prepared=$(sed -n 's/.*audio 0x[0-9a-f]*: \([0-9]*\) frame(s) prepared.*/\1/p' \
    "$OUT/$codec-smart.log" | head -1)
  if [ "${prepared:-0}" -ge 2 ]; then
    ok "$codec: the boundary frames are re-encoded" "$prepared frames"
  else
    bad "$codec: the boundary frames are re-encoded" \
      "${prepared:-none} -- $(grep -m1 'note:' "$OUT/$codec-smart.log")"
  fi
  same "$codec: and nothing said it could not be" "0" \
    "$(grep -c "copied, boundaries and all" "$OUT/$codec-smart.log")"

  # --- is what lies past the cut silent -----------------------------------
  # The last frame of the cut reaches past the range's end, and what fills
  # the rest of it is the material the cut was made to get rid of. Copied,
  # that is the tone still going; patched, it is silence. Measured over the
  # last eighth of a frame, which is inside the masked stretch wherever the
  # cut landed inside it.
  tail_n=$((frame / 8))
  # How long the cut's sound is, in samples. Decoded rather than asked of
  # the container: what is being measured is where the last frame ends, and
  # a transport stream's own duration is a timestamp rather than a count.
  len=$(( $(ffmpeg -v error -i "$OUT/$codec-copy.ts" -vn -ac 2 -ar 48000 \
              -f s16le - | wc -c) / 4 ))
  copy_tail=$(python3 tests/bd_audio.py peak "$OUT/$codec-copy.ts" 2 \
    "$((len - tail_n))" "$len")
  smart_tail=$(python3 tests/bd_audio.py peak "$OUT/$codec-smart.ts" 2 \
    "$((len - tail_n))" "$len")
  # And the sound a whole frame earlier, which is inside the range and must
  # be the recording's own either way.
  kept=$(python3 tests/bd_audio.py peak "$OUT/$codec-smart.ts" 2 \
    "$((len - frame * 3))" "$((len - frame * 2))")
  if [ "$copy_tail" -gt 1000 ] && [ "$smart_tail" -lt 300 ]; then
    ok "$codec: past the cut is silenced" "copy $copy_tail -> smart $smart_tail"
  else
    bad "$codec: past the cut is silenced" "copy $copy_tail -> smart $smart_tail"
  fi
  if [ "$kept" -gt 1000 ]; then
    ok "$codec: and what is kept is not" "peak $kept"
  else
    bad "$codec: and what is kept is not" "peak $kept"
  fi

  # --- is the sound still where it was ------------------------------------
  # The impulses say so to the sample. A patch built from the wrong samples
  # -- which is what a lead-in of the wrong length produces -- moves the
  # frame it replaces, and a moved impulse is what shows up here.
  #
  # In an MP4, because that is the container the impulse suite measures
  # against: a transport stream carries a clock that starts where the
  # broadcaster left it, and `audio_sync.py` reads a file's own start as
  # priming to be added to every click. Which it is, in an MP4. In a
  # transport stream it is 1.4 seconds of nothing to do with the sound, and
  # every click comes out that much late. What is being asked here is about
  # the frames, not the container, and it is asked of every codec alike.
  csrc="$OUT/clicks-$codec.mp4"
  clip "$FX/clicks.nut" "$codec" "$csrc" || { bad "$codec click fixture" "ffmpeg"; continue; }
  "$BIN" "$csrc" --keep 1.3-5.7 --keep 9.1-14.3 --keep 21.7-27.9 --audio-mode smart \
    -o "$OUT/clicks-$codec-cut.mp4" >"$OUT/clicks-$codec-cut.log" 2>&1
  res=$(OUT="$OUT/clicks-$codec-cut.mp4" SRC="$csrc" SMARTCUT_FRAME="$frame" \
        RANGES="1.3-5.7,9.1-14.3,21.7-27.9" python3 tests/audio_sync.py)
  if [[ "$res" == OK* ]]; then
    ok "$codec: the sound is where it was" "${res#OK|}"
  else
    bad "$codec: the sound is where it was" "${res#BAD|}"
  fi
done

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
