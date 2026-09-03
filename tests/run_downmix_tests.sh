#!/usr/bin/env bash
# Changing the channel count, which is the one audio setting that cannot be a
# copy.
#
# A 5.1 frame cannot be spliced into a stereo track -- nor a stereo one into a
# 5.1 track -- so asking for a different count asks for the whole track to be
# re-encoded, and what comes out is checked here, channel by channel. Folding
# down is what this is for and most of what is tested; spreading up is tested
# because the control offers it and a list can hold recordings of both kinds.
#
# The fixture gives every channel a tone of its own, so the fold is visible in
# the spectrum rather than merely plausible in the metadata: FC belongs in
# both output channels, BL in the left only, and the LFE nowhere. A downmix
# that dropped a channel, doubled one, or wired the surrounds to the wrong
# side passes every header check and fails this one.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-downmix"
mkdir -p "$OUT"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
python3 -c "import numpy" 2>/dev/null || { echo "  SKIP  numpy not installed"; exit 0; }

# 5.1 AAC in a transport stream, which is the shape a broadcast surround
# track has. 48 kHz, because that is the only rate a broadcast uses and the
# one the tones below are chosen to sit clear of.
#
# Every input is named to the channel it belongs in: `join` fills a layout in
# its own order otherwise, which is not the order the inputs were given in,
# and a fixture whose channels are not where it says they are would make this
# whole suite measure the wrong thing convincingly.
#
# The LFE gets a tone as well, and the encoder throws it away: an LFE channel
# is low-passed at around 120 Hz, so an 800 Hz tone put into one does not
# survive the encode. That is the right behaviour to bake in -- the LFE is
# silent in the source, and the check below is that it stays out of the fold.
if [ ! -f "$FX/surround.ts" ]; then
  echo "generating the 5.1 fixture ..."
  mkdir -p "$FX"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=20" \
    -f lavfi -i "sine=frequency=400:sample_rate=48000:duration=20" \
    -f lavfi -i "sine=frequency=600:sample_rate=48000:duration=20" \
    -f lavfi -i "sine=frequency=200:sample_rate=48000:duration=20" \
    -f lavfi -i "sine=frequency=800:sample_rate=48000:duration=20" \
    -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=20" \
    -f lavfi -i "sine=frequency=1200:sample_rate=48000:duration=20" \
    -filter_complex "[1:a][2:a][3:a][4:a][5:a][6:a]join=inputs=6:channel_layout=5.1:\
map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-BL|5.0-BR[a]" \
    -map 0:v -map "[a]" \
    -c:v mpeg2video -g 15 -keyint_min 15 -b:v 2000k \
    -c:a aac -b:a 384k -f mpegts "$FX/surround.ts" || exit 2
fi

# FL=400 FR=600 FC=200 LFE=800(gone) BL=1000 BR=1200, so a stereo fold is
# L = FL + FC + BL and R = FR + FC + BR, and a mono fold is everything but
# the LFE.
STEREO="400,200,1000;600,200,1200"
MONO="400,600,200,1000,1200"

pass=0; fail=0
t() {
  local name=$1 out="$OUT/$2" channels=$3 want=$4; shift 4
  rm -f "$out"
  if ! "$BIN" "$FX/surround.ts" --cut 5-10 "$@" -o "$out" >/dev/null 2>&1; then
    printf "  FAIL  %s: the cutter failed\n" "$name"; fail=$((fail+1)); return
  fi
  echo "$name"
  if python3 tests/downmix.py "$out" "$channels" "$want" "${CONFIG[@]}"; then
    pass=$((pass+1))
  else
    fail=$((fail+1))
  fi
}

echo "running downmix tests ..."
CONFIG=(--config 2)
t "5.1 to stereo (TS)"   stereo.ts  2 "$STEREO" --audio-channels 2
CONFIG=()
t "5.1 to stereo (MP4)"  stereo.mp4 2 "$STEREO" --audio-channels 2
CONFIG=(--config 1)
t "5.1 to mono (TS)"     mono.ts    1 "$MONO"   --audio-channels 1
# An explicit rate is taken as given; the derived one comes down with the
# channel count instead, which the note below reports rather than checks.
CONFIG=(--config 2)
t "stereo at 128 kbit/s" rate.ts    2 "$STEREO" --audio-channels 2 --audio-bitrate 128k
# The other direction, which only `--audio-channels` reaches -- the window
# offers the three counts that get asked for and 7.1 is not among them, but
# the engine takes any count from 1 to 8 and this is what says so. Nothing is
# folded, so every tone stays where it was and the two channels 7.1 has that
# 5.1 does not arrive empty -- and the ADTS header has to say 7, which is the
# configuration 8 channels are written as.
CONFIG=(--config 7)
t "5.1 to 7.1 (TS)"      up71.ts    8 "400;600;200;;1000;1200;;" --audio-channels 8
# Asking for the count the recording already has is not a downmix, and must
# leave the copy path alone: six channels out, the LFE still empty.
CONFIG=(--config 6)
t "5.1 asked for as 5.1" same.ts    6 "400;600;200;;1000;1200" --audio-channels 6
# And no request at all is the ordinary smart render, which copies.
CONFIG=(--config 6)
t "untouched"            copy.ts    6 "400;600;200;;1000;1200"

# Where the sound ended up, not just what is in it. The impulse suite cannot
# ask this -- its fixture is mono, and there is nothing to fold -- so the
# question is asked here, on a 5.1 fixture whose every channel carries the
# same 2 ms impulse every 0.5 s. Folded, that is still an impulse train, and
# where its impulses land is a sample-level reading of A/V alignment through
# the rematrix.
#
# In MP4, as the impulse suite's own fixture is: `audio_sync.py` reads the
# clicks off a timeline that starts at zero, and a transport stream's does
# not.
if [ ! -f "$FX/surround_clicks.mp4" ]; then
  echo "generating the 5.1 click fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=30" \
    -f lavfi -i "aevalsrc=if(lt(mod(t\,0.5)\,0.002)\,0.9\,0):s=48000:d=30" -shortest \
    -af "pan=5.1|FL=c0|FR=c0|FC=c0|LFE=0*c0|BL=c0|BR=c0" \
    -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -bf 3 -b:v 1500k \
    -profile:v high -level 3.1 -pix_fmt yuv420p \
    -c:a aac -b:a 384k "$FX/surround_clicks.mp4" || exit 2
fi
sync_check() {
  local name=$1 out="$OUT/$2" ranges=$3; shift 3
  rm -f "$out"
  if ! "$BIN" "$FX/surround_clicks.mp4" "$@" -o "$out" >/dev/null 2>&1; then
    printf "  FAIL  %-24s the cutter failed\n" "$name"; fail=$((fail+1)); return
  fi
  local res
  # A downmix is a whole-track re-encode however it was asked for, so the
  # boundaries are sample-accurate and measured against that bar.
  res=$(OUT="$out" SRC="$FX/surround_clicks.mp4" RANGES="$ranges" SMARTCUT_AUDIO=reencode \
        python3 tests/audio_sync.py)
  if [[ "$res" == OK* ]]; then
    printf "  ok    %-24s %s\n" "$name" "${res#OK|}"; pass=$((pass+1))
  else
    printf "  FAIL  %-24s %s\n" "$name" "${res#BAD|}"; fail=$((fail+1))
  fi
}
sync_check "sync, cut middle"   clicks.mp4  "0.0-8.0,20.0-30.0" --cut 8.0-20.0 --audio-channels 2
sync_check "sync, three ranges" clicks3.mp4 "1.3-5.7,9.1-14.3,21.7-27.9" \
  --keep 1.3-5.7 --keep 9.1-14.3 --keep 21.7-27.9 --audio-channels 2

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
