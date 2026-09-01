#!/usr/bin/env bash
# Is the audio in a cut really the source's audio, in the right place?
#
# The impulse suite (`run_audio_tests.sh`) measures sync to the sample on
# material built for the purpose. This one asks a blunter question of real
# broadcast audio: decode a window out of the cut, find where it sits in the
# recording by cross-correlation, and see whether it is where the video says
# it should be.
#
# The absolute offset is not the figure to watch -- decoder priming and the
# container's own start time both show up in it, identically everywhere. What
# matters is whether the offset *varies between segments*, because that is
# the cutter putting the audio in the wrong place at a seam.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
SRC="$MEDIA/full_ntv.ts"
WORK="$MEDIA"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
if [ ! -f "$SRC" ]; then echo "  SKIP  no $SRC"; exit 0; fi
python3 -c "import numpy" 2>/dev/null || { echo "  SKIP  numpy not installed"; exit 0; }

# Two kept ranges either side of one cut, on access points so the copy is
# clean; the second range is what a seam error would show up in.
RANGES=(--keep 0.665-400.0 --keep 700.0-1100.0)
PROBES=(100:99.335 300:299.335 800:499.335 1000:699.335)

pass=0; fail=0
run() {
  local name=$1 out=$2 spread=$3; shift 3
  "$BIN" "$SRC" "${RANGES[@]}" "$@" -o "$out" >/dev/null 2>&1
  echo "$name"
  if MAX_SPREAD="$spread" python3 tests/audio_content.py "$SRC" "$out" "${PROBES[@]}"; then
    pass=$((pass+1))
  else
    fail=$((fail+1))
  fi
  rm -f "$out"
}

run "音声コピー (MP4)"          "$WORK/_ac.mp4" 0.040
run "音声コピー (TS)"           "$WORK/_ac.ts"  0.040
run "音声スマート (MP4)"        "$WORK/_as.mp4" 0.040 --audio-mode smart
run "音声スマート (TS)"         "$WORK/_as.ts"  0.040 --audio-mode smart
run "音声サンプル精度 (MP4)"    "$WORK/_ar.mp4" 0.005 --audio-mode reencode
# The one combination that was silently broken: our encoder makes raw AAC,
# and MPEG-TS has to reframe it into ADTS from the stream's own extradata.
run "音声サンプル精度 (TS)"     "$WORK/_ar.ts"  0.005 --audio-mode reencode

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
