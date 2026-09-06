#!/usr/bin/env bash
# The VC-1 encoder has to write something a decoder recognises as the picture.
#
# There is no VC-1 encoder in libavcodec to check this one against, so the
# only honest test is the round trip: encode a picture, hand it to the
# decoder every player uses, and measure what comes back. Two things can go
# wrong and only this catches either of them. A bitstream that is subtly
# malformed still decodes -- to noise, or to a picture with one macroblock
# row of rubbish -- so "it decoded" proves nothing on its own; the numbers
# have to say the picture came back. And a transform or quantiser that is
# scaled wrongly produces a perfectly legal stream of the wrong brightness,
# which no amount of decoding without errors would reveal.
#
# The reference pictures come from the recording named by SMARTCUT_VC1,
# which has to be a VC-1 one -- a Blu-ray stream, or a piece of one.
set -u
cd "$(dirname "$0")/.."
OUT="${TMPDIR:-/tmp}/smartcut-vc1-out"
SRC="${SMARTCUT_VC1:-}"
ENC=rust/target/release/examples/vc1enc
rm -rf "$OUT"; mkdir -p "$OUT"

[ -x "$ENC" ] || {
  echo "build first: (cd rust && cargo build --release --examples)" >&2; exit 2;
}

pass=0; fail=0
ok()  { printf "  ok    %-30s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad() { printf "  FAIL  %-30s %s\n" "$1" "$2"; fail=$((fail+1)); }
skip() { printf "  SKIP  %-30s %s\n" "$1" "$2"; }

if [ -z "$SRC" ] || [ ! -f "$SRC" ]; then
  echo "set SMARTCUT_VC1 to a VC-1 recording (a Blu-ray .m2ts, or a piece of one)" >&2
  exit 2
fi

echo "vc1: $SRC"

# The stream's own headers, and pictures to encode again. Taken from a way
# into the recording: a disc usually opens on black, and a black picture
# would be encoded perfectly by anything at all.
#
# The pictures are read by seeking and then throwing away everything for the
# next few seconds. A seek lands where it lands -- inside a GOP, in front of
# a picture that references one the decoder was never given -- and what comes
# back until the stream has caught up is a picture with holes in it, or on an
# open-GOP recording a uniform grey frame. Either would be measured here as
# though it were the material. Running on past it costs a moment and is the
# whole difference between measuring this encoder and measuring a seek.
AT="${SMARTCUT_VC1_AT:-20}"
SETTLE="${SMARTCUT_VC1_SETTLE:-96}"
SIZE=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
  -of csv=p=0:s=x "$SRC" | head -1)
W=${SIZE%x*}; H=${SIZE#*x}
# How often pictures arrive, as the container states it. Not assumed: a disc
# is as likely to be 24 as 30, and every count below is derived from this.
RATE=$(ffprobe -v error -select_streams v:0 -show_entries stream=avg_frame_rate \
  -of csv=p=0 "$SRC" | head -1)
FPS=$(awk 'BEGIN{ split(ARGV[1], r, "/"); if (r[2] > 0) printf "%.6f", r[1] / r[2] }' "$RATE")
if [ -z "$FPS" ] || [ "${FPS%%.*}" -lt 1 ]; then
  echo "$SRC: the container does not say how often pictures arrive" >&2
  exit 2
fi
ffmpeg -v error -i "$SRC" -map 0:v:0 -c copy -t 2 -f data "$OUT/source.vc1" -y
ffmpeg -v error -ss "$AT" -i "$SRC" -vf "select=gte(n\,$SETTLE)" -fps_mode passthrough \
  -frames:v 4 -pix_fmt yuv420p -f rawvideo "$OUT/frames.yuv" -y

# A picture with nothing in it would pass any test; say so if that is what
# turned up rather than reporting a meaningless success.
range=$(python3 - "$OUT/frames.yuv" "$W" "$H" <<'EOF'
import sys
d = open(sys.argv[1], 'rb').read(int(sys.argv[2]) * int(sys.argv[3]))
print(max(d) - min(d))
EOF
)
if [ "$range" -lt 32 ]; then
  # Flat means one of two things and neither is worth measuring: the
  # recording really is on black here, or the seek came back with nothing.
  skip "reference pictures" "flat at ${AT}s (range $range); set SMARTCUT_VC1_AT"
  exit 0
fi

# Quality has to rise as the quantizer gets finer, and every step has to
# decode. The thresholds are deliberately loose: this is a check that the
# encoder is right, not a measure of how good it is.
prev=0
for q in 16 8 4; do
  name="pquant $q"
  if ! "$ENC" "$OUT/source.vc1" "$OUT/frames.yuv" "$W" "$H" 4 "$q" \
      "$OUT/q$q.vc1" >"$OUT/q$q.log" 2>&1; then
    bad "$name" "the encoder failed: $(tail -1 "$OUT/q$q.log")"
    continue
  fi
  if ! ffmpeg -v error -f vc1 -i "$OUT/q$q.vc1" -pix_fmt yuv420p \
      -f rawvideo "$OUT/q$q.yuv" -y 2>"$OUT/q$q.decode.log"; then
    bad "$name" "the decoder refused it: $(tail -1 "$OUT/q$q.decode.log")"
    continue
  fi
  # Every picture that went in has to come out again.
  want=$(( W * H * 3 / 2 * 4 ))
  got=$(stat -c%s "$OUT/q$q.yuv")
  if [ "$got" -ne "$want" ]; then
    bad "$name" "decoded $got bytes, wanted $want"
    continue
  fi
  # The decoder says nothing about a stream it merely limped through, so a
  # complaint on its error channel counts as a failure.
  if [ -s "$OUT/q$q.decode.log" ]; then
    bad "$name" "the decoder complained: $(head -1 "$OUT/q$q.decode.log")"
    continue
  fi
  psnr=$(ffmpeg -v error \
    -f rawvideo -pix_fmt yuv420p -s "${W}x${H}" -i "$OUT/frames.yuv" \
    -f rawvideo -pix_fmt yuv420p -s "${W}x${H}" -i "$OUT/q$q.yuv" \
    -lavfi "[0:v][1:v]psnr=stats_file=-" -f null - 2>&1 \
    | sed -n 's/.*psnr_y:\([0-9.]*\).*/\1/p' | head -1)
  size=$(stat -c%s "$OUT/q$q.vc1")
  floor=30
  if awk "BEGIN{exit !($psnr < $floor)}"; then
    bad "$name" "the picture came back at ${psnr}dB, which is not the picture"
    continue
  fi
  if awk "BEGIN{exit !($psnr <= $prev)}"; then
    bad "$name" "${psnr}dB is no better than the coarser step's ${prev}dB"
    continue
  fi
  ok "$name" "${psnr}dB, $((size / 4)) bytes a picture"
  prev=$psnr
done

# And the whole thing: a cut whose ends fall between access points, so both
# a head and a tail have to be written, spliced onto a copy of the middle.
# What is checked is what a viewer would notice -- that the decoder takes the
# result without complaint, that every picture asked for is there, and that
# the copied stretch came through byte for byte.
BIN=rust/target/release/smartcut
if [ ! -x "$BIN" ]; then
  skip "a cut of it" "build first: (cd rust && cargo build --release)"
else
  IN=5.37; OUT_AT=15.62
  if ! "$BIN" "$SRC" --keep "$IN-$OUT_AT" -o "$OUT/cut.m2ts" >"$OUT/cut.log" 2>&1; then
    bad "a cut of it" "the cut failed: $(grep -v 'Failed to open codec' "$OUT/cut.log" | tail -1)"
  else
    # The plan says how the range was split; the output has to hold every
    # picture all three parts promised.
    frames=$(ffprobe -v error -select_streams v:0 -count_frames \
      -show_entries stream=nb_read_frames -of csv=p=0 "$OUT/cut.m2ts" | head -1)
    want=$(awk "BEGIN{printf \"%d\", ($OUT_AT - $IN) * $FPS + 0.5}")
    complaints=$(ffmpeg -v error -i "$OUT/cut.m2ts" -f null - 2>&1 | wc -l)
    # How much of it came through untouched.
    #
    # Asked of the bytes rather than of the pictures. Comparing decoded
    # pictures means lining the two files up by frame number, and a frame
    # number is exactly what a piece of a recording does not have: it begins
    # mid-GOP, the decoder drops what it cannot decode, and everything after
    # that is off by however many that was. The copy path copies packets, so
    # the claim to check is that a long run of the cut's bytes appears
    # verbatim in the recording's -- which needs no alignment at all, and is
    # a stronger statement than any measure of how similar two pictures look.
    ffmpeg -v error -i "$OUT/cut.m2ts" -map 0:v:0 -c copy -f data "$OUT/cut.vc1" -y
    ffmpeg -v error -i "$SRC" -map 0:v:0 -c copy -f data "$OUT/src.vc1" -y
    verbatim=$(python3 - "$OUT/cut.vc1" "$OUT/src.vc1" <<'EOF'
import sys

cut = open(sys.argv[1], 'rb').read()
src = open(sys.argv[2], 'rb').read()
# A window from the middle of the cut, which is the copied part if any of it
# is, looked for in the recording. Then grown both ways for as long as the
# two agree.
probe = 1 << 16
mid = len(cut) // 2
at = src.find(cut[mid:mid + probe])
if at < 0:
    print(0)
    raise SystemExit
lo = 0
while mid - lo > 0 and at - lo > 0 and cut[mid - lo - 1] == src[at - lo - 1]:
    lo += 1
hi = 0
while mid + hi < len(cut) and at + hi < len(src) and cut[mid + hi] == src[at + hi]:
    hi += 1
print((lo + hi) * 100 // len(cut))
EOF
)
    if [ "$complaints" -ne 0 ]; then
      bad "a cut of it" "the decoder complained $complaints time(s) about the result"
    elif [ "$frames" -lt "$((want - 2))" ] || [ "$frames" -gt "$((want + 2))" ]; then
      bad "a cut of it" "$frames pictures, wanted about $want"
    elif [ "$verbatim" -lt 60 ]; then
      bad "a cut of it" "only $verbatim% of it is a verbatim run of the recording"
    else
      ok "a cut of it" "$frames pictures, $verbatim% of the video copied byte for byte"
    fi
  fi
fi

echo
echo "vc1: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
