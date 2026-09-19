#!/usr/bin/env bash
# What a recording is described by, when its opening is not its own.
#
# libavformat describes a track from the first frames it meets, a few
# megabytes into the file. A broadcast recording's first frames are regularly
# not the programme's: a tuner told to start early records the end of whatever
# was on before, and the sound of that can be a different shape. Three seconds
# of a bulletin read in mono ahead of a fifty minute documentary in stereo was
# enough to have the whole track called mono -- the encoder at every seam
# opened with one channel, the ADTS header in front of its frames saying one
# channel, and a whole-track re-encode folding the programme flat from
# beginning to end.
#
# So the fixture here is that recording in miniature: four seconds of mono
# joined to forty of stereo, on one PID, as a broadcast would carry it. Beside
# it, the same shape with a 5.1 programme behind a stereo opening, where the
# bitrate the container states is wrong for the same reason the channel count
# was -- it is the opening's, and a track re-encoded at it is a 5.1 programme
# written at a stereo programme's rate. And a guard: a recording that really
# is mono throughout, which must come out mono, because the correction is for
# a track whose opening disagrees with itself and not a licence to call
# everything stereo.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-audio-head"
mkdir -p "$OUT" "$FX"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-46s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-46s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

# Two pieces and a join. Written separately and concatenated rather than
# filtered into one stream, because what is wanted is exactly what a recorder
# leaves behind: two stretches of AAC whose own headers disagree, carried on
# the same PID with nothing in between to announce the change.
if [ ! -f "$FX/mono_head.ts" ]; then
  echo "generating the mono-head fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=30:duration=4" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4" \
    -ac 1 -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 96k -f mpegts "$FX/head_mono.ts" || exit 2
  # The programme itself, in stereo, and the two channels differ: a fold to
  # mono is then something the content itself shows, not only the header.
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=30:duration=40" \
    -f lavfi -i "sine=frequency=300:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=900:sample_rate=48000:duration=40" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a]" \
    -map 0:v -map "[a]" -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 160k -f mpegts "$FX/head_stereo.ts" || exit 2
  printf "file '%s'\nfile '%s'\n" "$FX/head_mono.ts" "$FX/head_stereo.ts" \
    > "$FX/head_list.txt"
  ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i "$FX/head_list.txt" \
    -c copy -f mpegts "$FX/mono_head.ts" || exit 2
fi
# The same again with 5.1 behind the opening, and a different tone in every
# channel so that the fold a mis-described track would have suffered is
# visible in the sound and not only in the headers.
if [ ! -f "$FX/stereo_head_51.ts" ]; then
  echo "generating the 5.1 fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=30:duration=4" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4" \
    -f lavfi -i "sine=frequency=660:sample_rate=48000:duration=4" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a]" \
    -map 0:v -map "[a]" -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 160k -f mpegts "$FX/head51_stereo.ts" || exit 2
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=30:duration=40" \
    -f lavfi -i "sine=frequency=200:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=400:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=600:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=800:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=40" \
    -f lavfi -i "sine=frequency=1200:sample_rate=48000:duration=40" \
    -filter_complex "[1:a][2:a][3:a][4:a][5:a][6:a]join=inputs=6:channel_layout=5.1[a]" \
    -map 0:v -map "[a]" -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 384k -f mpegts "$FX/head51_body.ts" || exit 2
  printf "file '%s'\nfile '%s'\n" "$FX/head51_stereo.ts" "$FX/head51_body.ts" \
    > "$FX/head51_list.txt"
  ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i "$FX/head51_list.txt" \
    -c copy -f mpegts "$FX/stereo_head_51.ts" || exit 2
fi
# The guard: mono from first frame to last, and long enough that the middle
# of it is looked at.
if [ ! -f "$FX/all_mono.ts" ]; then
  echo "generating the all-mono fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=30:duration=44" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=44" \
    -ac 1 -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 96k -f mpegts "$FX/all_mono.ts" || exit 2
fi

# What the cutter says the sound is, off its own first lines.
described() {
  "$BIN" "$1" 2>/dev/null | sed -n 's/^audio  : aac \([0-9]*\)Hz \([0-9]*\)ch.*/\1 \2/p'
}
# What a track in a written file is, asked of the container rather than of the
# frames: the two are separate claims and a cut has to get both right. The
# first line only -- a transport stream's track is listed twice, once under
# the programme that carries it and once on its own.
declared() {
  ffprobe -v error -select_streams a:0 -show_entries stream=channels \
    -of default=nw=1:nk=1 "$1" 2>/dev/null | head -1
}

echo
echo "a recording whose opening is the programme before it"
same "described by its own sound, not its opening" "48000 2" "$(described "$FX/mono_head.ts")"

# A cut wholly inside the programme, with all four boundaries off any frame
# edge, so that smart rendering has something to re-encode at each of them.
RANGES=(--keep 10.123-20.456 --keep 25.789-35.05)
ffmpeg -hide_banner -loglevel error -y -ss 10 -t 26 -i "$FX/mono_head.ts" \
  -map 0:a:0 -c copy -f adts "$OUT/source.aac" || exit 2

if "$BIN" "$FX/mono_head.ts" "${RANGES[@]}" -o "$OUT/smart.ts" >/dev/null 2>&1; then
  same "the stream says what the frames are" "2" "$(declared "$OUT/smart.ts")"
  ffmpeg -hide_banner -loglevel error -y -i "$OUT/smart.ts" -map 0:a:0 -c copy \
    -f adts "$OUT/smart.aac" 2>/dev/null
  # Every header stereo -- including the handful the boundaries land inside,
  # which is the whole of what went wrong -- and all but that handful still
  # the recording's own bytes, which is what smart rendering means.
  if python3 tests/aac_frames.py "$OUT/smart.aac" "$OUT/source.aac" \
       --channels 2 --max-reencoded 8 --min-reencoded 1 > "$OUT/frames.txt" 2>&1; then
    ok "smart: every frame of the cut is stereo" \
       "$(sed -n 's/.*(\(.*%\)).*/\1/p' "$OUT/frames.txt" | head -1) verbatim"
  else
    bad "smart: every frame of the cut is stereo" "$(grep BAD "$OUT/frames.txt" | head -1)"
  fi
else
  bad "smart: the cutter failed"
  bad "the stream says what the frames are"
fi

# The mode with nothing to copy: every frame is written here, so a track
# described mono was a track folded flat for its whole length.
if "$BIN" "$FX/mono_head.ts" "${RANGES[@]}" --audio-mode reencode \
     -o "$OUT/reencode.ts" >/dev/null 2>&1; then
  same "reencode: the whole track is stereo" "2" "$(declared "$OUT/reencode.ts")"
else
  bad "reencode: the cutter failed"
fi

# A range that spans the change itself, written to a layout that is neither
# shape. The ranges above are wholly inside the programme, so every frame
# they re-encode is stereo and one conversion covers all of them; here the
# mono opening and the programme's stereo both have to be converted, because
# 5.1 was asked for and is neither. A conversion built for the first shape
# and kept for the second is refused by swresample -- "Input changed" -- and
# the cut ended there, at the instant the programme began.
if "$BIN" "$FX/mono_head.ts" --keep 1.0-20.0 --audio-mode reencode \
     --audio-channels 6 -o "$OUT/across.ts" >/dev/null 2>&1; then
  same "a range across the change, written as 5.1" "6" "$(declared "$OUT/across.ts")"
  # And the sound covers the range rather than stopping where the shape
  # changed: 19 seconds of it, to within a frame either way.
  frames=$(ffprobe -v error -select_streams a:0 -count_packets \
    -show_entries stream=nb_read_packets -of default=nw=1:nk=1 "$OUT/across.ts" \
    2>/dev/null | head -1)
  want=$(( 19 * 48000 / 1024 ))
  if [ -n "$frames" ] && [ "$frames" -ge $(( want - 2 )) ] && [ "$frames" -le $(( want + 2 )) ]; then
    ok "and the sound runs the whole way across it" "$frames frames"
  else
    bad "and the sound runs the whole way across it" "want about $want, got ${frames:-none}"
  fi
else
  bad "a range across the change, written as 5.1" "the cutter failed"
  bad "and the sound runs the whole way across it"
fi

# What a written track is carried at. An average over the file, so never
# exact; `near` allows a tenth either way, which is far inside the difference
# being looked for -- 208 kbit/s against 386.
rate_of() {
  ffprobe -v error -select_streams a:0 -show_entries stream=bit_rate \
    -of default=nw=1:nk=1 "$1" 2>/dev/null | head -1
}
near() {
  local name=$1 want=$2 got=$3
  if [ "$want" -gt 0 ] && [ "$got" -gt 0 ] \
     && [ $(( (got - want) * 10 )) -lt "$want" ] \
     && [ $(( (want - got) * 10 )) -lt "$want" ]; then
    ok "$name" "$(( got / 1000 )) kbit/s"
  else
    bad "$name" "want about $(( want / 1000 )) kbit/s, got $(( got / 1000 ))"
  fi
}

echo
echo "and the same with 5.1 behind the opening"
same "described as the 5.1 it is" "48000 6" "$(described "$FX/stereo_head_51.ts")"
if "$BIN" "$FX/stereo_head_51.ts" "${RANGES[@]}" -o "$OUT/smart51.ts" >/dev/null 2>&1; then
  same "the stream says 5.1" "6" "$(declared "$OUT/smart51.ts")"
  ffmpeg -hide_banner -loglevel error -y -i "$OUT/smart51.ts" -map 0:a:0 -c copy \
    -f adts "$OUT/smart51.aac" 2>/dev/null
  if python3 tests/aac_frames.py "$OUT/smart51.aac" --channels 6 \
       > "$OUT/frames51.txt" 2>&1; then
    ok "smart: every frame of the cut is 5.1"
  else
    bad "smart: every frame of the cut is 5.1" "$(grep BAD "$OUT/frames51.txt" | head -1)"
  fi
else
  bad "smart: the cutter failed on the 5.1 recording"
  bad "the stream says 5.1"
fi
# The bitrate is the opening's too, and a whole-track re-encode is where that
# shows: 208 kbit/s read off two seconds of stereo, spent on six channels.
# Given up with the rest of the opening's description, so what the codec is
# worth at six channels stands in -- which is what the recording itself was
# carried at.
if "$BIN" "$FX/stereo_head_51.ts" "${RANGES[@]}" --audio-mode reencode \
     -o "$OUT/reencode51.ts" >/dev/null 2>&1; then
  same "reencode: the whole track is 5.1" "6" "$(declared "$OUT/reencode51.ts")"
  near "and at 5.1's own rate, not the opening's" \
       "$(rate_of "$FX/head51_body.ts")" "$(rate_of "$OUT/reencode51.ts")"
else
  bad "reencode: the cutter failed on the 5.1 recording"
  bad "and at 5.1's own rate, not the opening's"
fi

echo
echo "and a recording that really is mono"
same "described as the mono it is" "48000 1" "$(described "$FX/all_mono.ts")"
if "$BIN" "$FX/all_mono.ts" --keep 10.123-20.456 -o "$OUT/mono.ts" >/dev/null 2>&1; then
  same "and is written as mono" "1" "$(declared "$OUT/mono.ts")"
  ffmpeg -hide_banner -loglevel error -y -i "$OUT/mono.ts" -map 0:a:0 -c copy \
    -f adts "$OUT/mono.aac" 2>/dev/null
  if python3 tests/aac_frames.py "$OUT/mono.aac" --channels 1 > "$OUT/mono.txt" 2>&1; then
    ok "and every frame of it says so"
  else
    bad "and every frame of it says so" "$(grep BAD "$OUT/mono.txt" | head -1)"
  fi
else
  bad "the cutter failed on the all-mono recording"
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
