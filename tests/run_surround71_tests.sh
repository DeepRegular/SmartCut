#!/usr/bin/env bash
# 7.1: eight channels in, and where each of them ends up.
#
# The same method as the downmix suite -- every channel carries a tone of its
# own, so a channel that went to the wrong place, went missing or was mixed
# into another shows up in the spectrum -- over the shapes 7.1 actually
# arrives in: AAC in a transport stream, an MP4 and a Matroska file, FLAC,
# Opus and plain PCM in Matroska, and Blu-ray LPCM in an m2ts. FFmpeg writes
# none of the disc codecs at 7.1 (its E-AC-3, TrueHD and DTS encoders stop at
# 5.1 and fold quietly), so those are not here; what is here is every path a
# 7.1 track can take through a cut.
#
# Not here: 7.1(wide), whose extra pair is at the front. FFmpeg's AAC encoder
# writes it with a program config element, and its own decoder reads that
# back as eight channels in no named arrangement -- so there is no fixture to
# be made that says where its channels are.
set -u
cd "$(dirname "$0")/.."
BIN=${BIN:-rust/target/release/smartcut}
FX="${TMPDIR:-/tmp}/smartcut-fixtures/71"
OUT="${TMPDIR:-/tmp}/smartcut-71"
mkdir -p "$FX" "$OUT"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
python3 -c "import numpy" 2>/dev/null || { echo "  SKIP  numpy not installed"; exit 0; }

# FL=400 FR=600 FC=200 LFE=800 BL=1000 BR=1200 SL=1400 SR=1600, joined by
# name so that each tone is where the layout says it is.
TONES=(400 600 200 800 1000 1200 1400 1600)
make() {
  local out=$1 layout=$2 names=$3; shift 3
  [ -f "$FX/$out" ] && return
  local in=() pads="" map="" i=0
  for f in "${TONES[@]}"; do
    in+=(-f lavfi -i "sine=frequency=$f:sample_rate=48000:duration=20")
    pads="$pads[$((i + 1)):a]"
    i=$((i + 1))
  done
  i=0
  for n in $names; do
    map="$map|$i.0-$n"
    i=$((i + 1))
  done
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=20" "${in[@]}" \
    -filter_complex "${pads}join=inputs=8:channel_layout=$layout:map=${map#|}[a]" \
    -map 0:v -map "[a]" "$@" "$FX/$out" || { echo "cannot make $out" >&2; exit 2; }
}
PLAIN="FL FR FC LFE BL BR SL SR"
H264=(-c:v libx264 -g 30 -keyint_min 30 -sc_threshold 0 -bf 2 -b:v 1500k -pix_fmt yuv420p)
echo "generating the 7.1 fixtures ..."
make aac.ts   7.1 "$PLAIN" -c:v mpeg2video -g 15 -b:v 2000k -c:a aac -b:a 512k -f mpegts
make aac.mp4  7.1 "$PLAIN" "${H264[@]}" -c:a aac -b:a 512k
make aac.mkv  7.1 "$PLAIN" "${H264[@]}" -c:a aac -b:a 512k
make flac.mkv 7.1 "$PLAIN" "${H264[@]}" -c:a flac
make opus.mkv 7.1 "$PLAIN" "${H264[@]}" -c:a libopus -b:a 512k
make pcm.mkv  7.1 "$PLAIN" "${H264[@]}" -c:a pcm_s24le
make lpcm.m2ts 7.1 "$PLAIN" "${H264[@]}" -c:a pcm_bluray -f mpegts -mpegts_m2ts_mode 1

# What each channel should carry. AAC low-passes the LFE, so an 800 Hz tone
# put there does not survive it; the lossless codecs keep it, and so does
# Opus, which treats the LFE as a channel like any other.
LOSSY="400;600;200;;1000;1200;1400;1600"
WHOLE="400;600;200;800;1000;1200;1400;1600"
# Folded by swresample's matrix: the sides go into the backs for 5.1, and
# every surround into its own side for stereo. The LFE is left out of both.
TO51="400;600;200;;1000,1400;1200,1600"
TO20="400,200,1000,1400;600,200,1200,1600"

pass=0; fail=0
t() {
  local name=$1 in="$FX/$2" out="$OUT/$3" channels=$4 want=$5; shift 5
  rm -f "$out"
  local log
  if ! log=$("$BIN" "$in" --cut 5-10 "$@" -o "$out" 2>&1); then
    printf "  FAIL  %s: the cutter failed\n%s\n" "$name" "$(tail -3 <<<"$log")"
    fail=$((fail+1)); return
  fi
  echo "$name"
  # The pictures as well: a cut that put the sound right and broke the
  # video is not a cut.
  local broken
  broken=$(ffmpeg -v error -i "$out" -map 0:v -f null - 2>&1 | wc -l)
  if [ "$broken" -ne 0 ]; then
    printf "  BAD  %s decode error(s) in the pictures\n" "$broken"
    fail=$((fail+1)); return
  fi
  if python3 tests/downmix.py "$out" "$channels" "$want" "${EXTRA[@]}"; then
    pass=$((pass+1))
  else
    fail=$((fail+1))
  fi
}

echo "running 7.1 tests ..."
# Cut as they come: the smart render copies what it can and writes the frames
# at the seams afresh, and neither may move a channel.
EXTRA=(--config 7 --layout 7.1)
t "AAC 7.1 in TS, cut"        aac.ts    aac.ts    8 "$LOSSY"
EXTRA=(--layout 7.1)
t "AAC 7.1 in MP4, cut"       aac.mp4   aac.mp4   8 "$LOSSY"
t "AAC 7.1 in MKV, cut"       aac.mkv   aac.mkv   8 "$LOSSY"
t "FLAC 7.1, cut"             flac.mkv  flac.mkv  8 "$WHOLE"
t "Opus 7.1, cut"             opus.mkv  opus.mkv  8 "$WHOLE"
t "Blu-ray LPCM 7.1, cut"     lpcm.m2ts lpcm.m2ts 8 "$WHOLE"
# Matroska's PCM names no arrangement at all, so eight channels of it are
# taken as the plain 7.1 every reader assumes for eight.
EXTRA=()
t "PCM 7.1 in MKV, cut"       pcm.mkv   pcm.mkv   8 "$WHOLE"

# Written afresh as a whole, which is where an encoder's own idea of eight
# channels could replace the track's.
EXTRA=(--layout 7.1)
t "AAC 7.1 re-encoded"        aac.mkv   aac-re.mkv 8 "$LOSSY" --audio-mode reencode
t "LPCM 7.1 to AAC"           lpcm.m2ts lpcm-aac.ts 8 "$LOSSY" --audio-codec aac

# Folded. The two counts the disc codecs top out at, and stereo.
EXTRA=()
t "7.1 to 5.1"                aac.mkv   to51.mkv  6 "$TO51" --audio-channels 6
t "7.1 to stereo"             aac.ts    to20.ts   2 "$TO20" --audio-channels 2
t "7.1 to AC-3 5.1"           aac.mkv   ac3.mkv   6 "$TO51" --audio-codec ac3 --audio-channels 6
t "LPCM 7.1 to stereo LPCM"   lpcm.m2ts to20.m2ts 2 "$TO20" --audio-channels 2

# AC-3 has no 7.1. Asked for it without a count, the cut says so rather than
# writing something else.
rm -f "$OUT/ac3-8.mkv"
if log=$("$BIN" "$FX/flac.mkv" --cut 5-10 --audio-codec ac3 -o "$OUT/ac3-8.mkv" 2>&1); then
  printf "  FAIL  AC-3 at 7.1 was written\n"; fail=$((fail+1))
elif grep -q "8 channels" <<<"$log"; then
  printf "  ok    AC-3 at 7.1 is refused, and says why\n"; pass=$((pass+1))
else
  printf "  FAIL  AC-3 at 7.1 failed without saying why:\n%s\n" "$(tail -3 <<<"$log")"; fail=$((fail+1))
fi

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
