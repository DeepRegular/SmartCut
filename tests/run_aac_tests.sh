#!/usr/bin/env bash
# What the audio of a cut is made of, byte for byte.
#
# The other audio suites ask where the sound ended up. This one asks what the
# frames themselves are: a Japanese broadcast carries MPEG-2 AAC, and a cut of
# one has to come out as MPEG-2 AAC throughout -- including the frames this
# tool encodes itself, which FFmpeg would otherwise write as MPEG-4 and leave
# a stream that is two kinds of AAC at once.
#
# And it counts. Smart rendering means the frames are the recording's own
# bytes except where a boundary lands inside one, so the count of frames that
# are *not* verbatim is the whole claim, and it is checked here.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
SRC="$MEDIA/full_ntv.ts"
WORK="${TMPDIR:-/tmp}/smartcut-aac"
mkdir -p "$WORK"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
if [ ! -f "$SRC" ]; then echo "  SKIP  no $SRC"; exit 0; fi

# Two ranges, four boundaries, none of them on an audio frame edge, and all
# four in the middle of the programme's own sound.
RANGES=(--keep 100.123-160.456 --keep 400.789-460.05)
# The ordinary case instead: a commercial block taken out, both cuts landing
# in the silence the broadcaster leaves around it. Verified as digital
# silence (-91 dBFS) in this recording.
QUIET=(--keep 605.0-619.9 --keep 620.1-634.9)

# The recording's own frames over the stretch the ranges come from, to
# compare against. Written by the same demuxer that reads the cut back, so a
# frame that matches matches exactly.
if [ ! -f "$WORK/source.aac" ]; then
  ffmpeg -hide_banner -loglevel error -y -ss 95 -t 560 -i "$SRC" \
    -map 0:a:0 -c copy -f adts "$WORK/source.aac" || exit 2
fi
echo "recording:"
python3 tests/aac_frames.py "$WORK/source.aac" --mpeg 2 --profile 1 || exit 2

pass=0; fail=0
t() {
  local name=$1 out="$WORK/cut.${2}"; shift 2
  local checks=()
  while [ $# -gt 0 ] && [ "${1:0:2}" = "--" ] && [ "$1" != "--audio-mode" ] \
        && [ "$1" != "--aac" ] && [ "$1" != "--audio-es" ]; do
    checks+=("$1" "$2"); shift 2
  done
  rm -f "$WORK/cut.aac"
  if ! "$BIN" "$SRC" "${RANGES[@]}" "$@" -o "$out" >/dev/null 2>&1; then
    printf "  FAIL  %s: the cutter failed\n" "$name"; fail=$((fail+1)); return
  fi
  # Read the frames back out of the cut, headers and all.
  ffmpeg -hide_banner -loglevel error -y -i "$out" -map 0:a:0 -c copy \
    -f adts "$WORK/read.aac" 2>/dev/null
  echo "$name"
  if python3 tests/aac_frames.py "$WORK/read.aac" "$WORK/source.aac" "${checks[@]}"; then
    pass=$((pass+1))
  else
    fail=$((fail+1))
  fi
  rm -f "$out" "$WORK/read.aac"
}

echo
# No --audio-mode at all: the default has to be the smart one, and has to
# behave like it.
t "default (TS)"   ts --mpeg 2 --profile 1 --max-reencoded 8 --min-reencoded 1
# Copying touches nothing: every frame is the recording's, MPEG-2 and all.
t "copy (TS)"      ts --mpeg 2 --profile 1 --max-reencoded 0 --audio-mode copy
# Smart rendering touches the frames the four boundaries land inside, and
# a guard beside each. Nothing else, and what it writes is MPEG-2 too.
t "smart (TS)"     ts --mpeg 2 --profile 1 --max-reencoded 8 --min-reencoded 1 \
                      --audio-mode smart
# An MP4 keeps the payloads and drops the framing, so only the payloads can
# be compared -- but the claim is the same one: all but a handful are the
# recording's own.
t "smart (MP4)"    mp4 --payload-only 1 --profile 1 --max-reencoded 8 \
                      --min-reencoded 1 --audio-mode smart
# The whole track re-encoded is still MPEG-2 AAC, which is the combination
# FFmpeg cannot be asked for: the MPEG-TS muxer has no way to tell the ADTS
# framing it uses which version to write.
t "reencode (TS)"  ts --mpeg 2 --profile 1 --audio-mode reencode
# Asking for the other AAC while frames are being copied cannot be honoured --
# the copied ones keep their own headers, and the result would be a stream that
# is two kinds of AAC at once. The request is refused, with a note, and the
# recording's own version is followed: every frame stays MPEG-2.
t "smart, --aac mpeg4 refused" ts --mpeg 2 --profile 1 --max-reencoded 8 \
                      --audio-mode smart --aac mpeg4
# A whole-track re-encode copies nothing, so there it can be asked for either
# way -- and here it is asked for the one the recording does not use.
t "reencode, --aac mpeg4" ts --mpeg 4 --profile 1 --audio-mode reencode --aac mpeg4
# Where a commercial break is cut, the far half of the straddling frame is
# already silent -- there is nothing to remove, so nothing is re-encoded and
# the whole track stays the recording's own bytes. This is what an ordinary
# cut looks like, and what TMPGEnc's smart renderer relies on throughout.
RANGES=("${QUIET[@]}")
t "smart, cut in silence" ts --mpeg 2 --profile 1 --max-reencoded 0 --audio-mode smart

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
