#!/usr/bin/env bash
# VP9 and AV1: does a range come out joined, and is the middle of it still
# the recording's own bytes?
#
# These two were written off for years on the grounds that they have no form
# that can be joined end to end. They have. Neither carries a parameter set
# the way H.264 does: a VP9 key frame states its own size and colour in the
# clear, and every AV1 encoder measured here writes a sequence header OBU in
# front of every key frame. So a partial GOP written afresh and the
# recording's own pictures after it splice exactly the way the other codecs
# do, and what needs testing is that they did.
#
# Three things are measured for each, and all three are needed.
#
#   * **The frame count.** A join that dropped or doubled a picture still
#     decodes.
#   * **A clean decode by the decoder players actually use** -- libvpx for
#     VP9, dav1d for AV1, neither of them the one that wrote the seam. A
#     stream libavcodec is happy to read back is not yet a stream.
#   * **How much of the output is the recording byte for byte.** This is the
#     one that says smart rendering happened at all: a run that quietly
#     re-encoded the lot would pass the first two and fail this. The share
#     has to be what the plan said it would be.
#
# Fixtures are generated here, so this needs nothing but ffmpeg with libvpx
# and one AV1 encoder.
set -u
cd "$(dirname "$0")/.."
FX="${TMPDIR:-/tmp}/smartcut-open-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-open-out"
CUT=rust/target/release/smartcut
mkdir -p "$FX"
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$CUT" ] || {
  echo "build first: (cd rust && cargo build --release)" >&2; exit 2;
}

pass=0; fail=0
ok()   { printf "  ok    %-26s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-26s %s\n" "$1" "$2"; fail=$((fail+1)); }
skip() { printf "  SKIP  %-26s %s\n" "$1" "$2"; }

have() { ffmpeg -hide_banner -encoders 2>/dev/null | grep -q " $1 "; }

# Which AV1 encoder is here. The cutter prefers them in this order too, and
# the fixture is written by whichever answers so that the test is measuring a
# join rather than one encoder's stream read by another's rules.
AV1ENC=
for e in libsvtav1 librav1e libaom-av1; do have "$e" && { AV1ENC=$e; break; }; done

# A picture that is expensive to code, so that a re-encoded second is
# visibly a re-encoded second in the byte counts. testsrc2 moves enough.
gen() {
  local name=$1; shift
  [ -f "$FX/$name" ] && return 0
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=20" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=20" \
    -shortest "$@" "$FX/$name" || return 1
}

echo "generating fixtures in $FX ..."
# SVT-AV1 talks to the terminal itself rather than through libav, and a
# fixture being made is not something anyone needs a dozen lines about.
export SVT_LOG=1
if have libvpx-vp9; then
  gen vp9.webm -c:v libvpx-vp9 -b:v 800k -g 30 -keyint_min 30 \
       -deadline good -cpu-used 5 -row-mt 1 -c:a libopus -b:a 96k
  # Profile 2. The pixel format is copied from the recording to the encoder,
  # so a ten-bit recording is the one that says whether it was copied right.
  gen vp9-10.webm -pix_fmt yuv420p10le -c:v libvpx-vp9 -b:v 900k -g 30 \
       -keyint_min 30 -deadline good -cpu-used 5 -row-mt 1 -c:a libopus -b:a 96k
fi
case "$AV1ENC" in
  libsvtav1)  gen av1.mkv -c:v libsvtav1 -b:v 800k -g 30 -preset 10 -c:a libopus -b:a 96k ;;
  librav1e)   gen av1.mkv -c:v librav1e -b:v 800k -g 30 -speed 10 -c:a libopus -b:a 96k ;;
  libaom-av1) gen av1.mkv -c:v libaom-av1 -b:v 800k -g 30 -cpu-used 8 -row-mt 1 \
                   -c:a libopus -b:a 96k ;;
esac

# What share of the output's pictures are a packet of the recording's,
# unchanged. Hashes rather than offsets: a muxer moves bytes about and
# renumbers everything, and the one thing it does not touch is the payload.
verbatim() {
  python3 - "$1" "$2" <<'PY'
import json, subprocess, sys

def hashes(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v", "-show_packets",
         "-show_data_hash", "md5", "-of", "json", path],
        capture_output=True).stdout
    return [p.get("data_hash") for p in json.loads(out)["packets"]]

made, source = hashes(sys.argv[1]), set(hashes(sys.argv[2]))
same = sum(1 for h in made if h in source)
print(f"{len(made)} {same}")
PY
}

# One cut, and everything that can be asked of it.
#
#   run <name> <fixture> <decoder> <in> <out> <frames> <copied share, %>
run() {
  local name=$1 src=$FX/$2 dec=$3 a=$4 b=$5 want=$6 share=$7
  local made=$OUT/$name.${2##*.}
  [ -f "$src" ] || { skip "$name" "no encoder for the fixture"; return; }
  local plan
  plan=$("$CUT" "$src" --keep "$a-$b" -o "$made" 2>&1) || {
    bad "$name" "the cut failed: $(printf '%s' "$plan" | tail -1)"; return;
  }
  # What the plan said it would copy, which is the figure the bytes are held
  # to below. Read back rather than assumed: the fixture's entry points are
  # the encoder's business, not this test's.
  local said
  said=$(printf '%s' "$plan" | sed -n 's/.*copied .*(\([0-9.]*\)%).*/\1/p' | tail -1)
  local errs
  errs=$(ffmpeg -v error -c:v "$dec" -i "$made" -f null - 2>&1 | head -3)
  [ -z "$errs" ] || { bad "$name" "$dec: $errs"; return; }
  local got
  got=$(ffprobe -v error -select_streams v -count_frames \
        -show_entries stream=nb_read_frames -of csv=p=0 "$made")
  [ "$got" = "$want" ] || { bad "$name" "$got pictures, wanted $want"; return; }
  local counts n same pct
  counts=$(verbatim "$made" "$src") || { bad "$name" "could not read the packets"; return; }
  n=${counts% *}; same=${counts#* }
  pct=$(awk -v s="$same" -v n="$n" 'BEGIN{ printf "%.1f", n ? 100*s/n : 0 }')
  # At least what the plan promised, give or take a picture. The two are
  # counted differently -- the plan in seconds of display, this in packets --
  # and the measured figure reads high for AV1, because a picture shown a
  # second time is an OBU of two bytes and every one of them in the file is
  # the same two bytes. So the floor is what is tested: a run that quietly
  # re-encoded more than it said would fall through it.
  awk -v got="$pct" -v said="${said:-0}" 'BEGIN{ exit !(got >= said - 1.5) }' || {
    bad "$name" "$pct% of the bytes are the recording's, the plan said $said%"; return;
  }
  ok "$name" "$got pictures, $pct% byte-identical (plan said $share%)"
}

echo "running tests ..."
# 30-picture GOPs at 30/s: an entry point on every whole second. 4.5 to 12.5
# starts and ends halfway through one, so both ends are written afresh and
# the eight seconds between them are the recording's.
run "vp9 mid-GOP"    vp9.webm libvpx-vp9 4.5 12.5 240 87
run "vp9 aligned"    vp9.webm libvpx-vp9 5   13   240 100
run "vp9 10-bit"     vp9-10.webm libvpx-vp9 4.5 12.5 240 87
run "av1 mid-GOP"    av1.mkv  libdav1d   4.5 12.5 240 87
run "av1 aligned"    av1.mkv  libdav1d   5   13   240 100

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
