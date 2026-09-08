#!/usr/bin/env bash
# The sound a Blu-ray carries has to survive a cut, whatever it is written to.
#
# A broadcast recording carries AAC and nothing else, so everything else this
# program meets comes off a disc: LPCM, DTS and DTS-HD, TrueHD, E-AC-3. Each
# of them is a different problem. LPCM is in a private stream that only a
# transport stream describes, so an MP4 has to be given the same samples in a
# box it has -- and given them exactly. TrueHD's MP4 box is outside the
# standard, and libavformat will not write one unless it is told that is
# wanted. DTS and TrueHD are lossless, so nothing here re-encodes a frame of
# either: what comes out has to be the recording's own bytes.
#
# One clip per codec, cut once into each container, and the claim checked for
# each is the one that codec makes.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-bd-audio"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
[ -f "$FX/mpeg2.ts" ] || { echo "run tests/run_tests.sh first to generate fixtures" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-40s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-40s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

echo "building the clips in $OUT ..."
rm -rf "$OUT"; mkdir -p "$OUT/cut"
# The fixture's pictures with the disc's sound in place of the broadcast's,
# in Blu-ray's own 192 byte framing. `-strict -2` is for the encoders
# libavformat calls experimental; nothing here encodes with them again.
clip() { # <name> <ffmpeg audio args...>
  local name=$1; shift
  ffmpeg -hide_banner -loglevel error -y -i "$FX/mpeg2.ts" -c:v copy "$@" \
    -strict -2 -f mpegts -mpegts_m2ts_mode 1 "$OUT/$name.m2ts" || exit 2
}
clip lpcm   -c:a pcm_bluray
clip lpcm24 -c:a pcm_bluray -sample_fmt s32
clip dts    -c:a dca -ac 2
clip truehd -c:a truehd -ac 2
clip eac3   -c:a eac3

# What the disc's own index calls each of these, and so what the cut's
# programme map has to keep calling them. libavformat's mpegts muxer knows
# none of the first three: asked to write LPCM it declares "private data",
# and asked to write E-AC-3 it reaches for ATSC's 0x87 rather than Blu-ray's
# 0x84. The cut keeps the recording's own map, which is what makes these hold.
type_of_lpcm=0x80; type_of_lpcm24=0x80; type_of_dts=0x82
type_of_truehd=0x83; type_of_eac3=0x84
codec_of_lpcm=pcm_bluray; codec_of_lpcm24=pcm_bluray; codec_of_dts=dts
codec_of_truehd=truehd; codec_of_eac3=eac3
# What each becomes where the container has no box for the recording's own.
mp4_of_lpcm=pcm_s16be; mp4_of_lpcm24=pcm_s24be; mp4_of_dts=dts
mp4_of_truehd=truehd; mp4_of_eac3=eac3

echo "running tests ..."

# One line: ffprobe names a transport stream's streams once per programme
# and once on their own.
audio_codec() { ffprobe -v error -select_streams a:0 -show_entries stream=codec_name \
                  -of default=nw=1:nk=1 "$1" | head -1; }
audio_type()  { python3 tests/bd_audio.py types "$1" | sed -n "2p" | cut -d' ' -f2; }

# --- every codec, into every container ------------------------------------
# The cut has to be written at all -- an MP4 asked for LPCM used to stop the
# whole cut with "could not find tag for codec pcm_bluray" -- and the track
# in it has to be the track that went in.
for name in lpcm lpcm24 dts truehd eac3; do
  for ext in ts m2ts mp4; do
    eval "want=\$$([ $ext = mp4 ] && echo mp4_of_$name || echo codec_of_$name)"
    if "$BIN" "$OUT/$name.m2ts" --keep 1.234-3.456 -o "$OUT/cut/$name.$ext" \
         >"$OUT/cut/$name.$ext.log" 2>&1; then
      same "$name -> .$ext" "$want" "$(audio_codec "$OUT/cut/$name.$ext")"
    else
      bad "$name -> .$ext" "$(tail -1 "$OUT/cut/$name.$ext.log")"
    fi
  done
  # And a transport stream has to go on calling it what the disc called it.
  for ext in ts m2ts; do
    eval "want=\$type_of_$name"
    same "$name .$ext keeps its stream type" "$want" "$(audio_type "$OUT/cut/$name.$ext")"
  done
done

# --- LPCM into an MP4 is the same sound ------------------------------------
# The samples go through a 32 bit float on the way, whose 24 bit mantissa
# holds every value Blu-ray LPCM can carry. So "the same box, different
# samples" is not a trade this makes: nothing differs at all.
for name in lpcm lpcm24; do
  got=$(python3 tests/bd_audio.py align "$OUT/$name.m2ts" "$OUT/cut/$name.mp4" 2)
  differing=$(sed 's/.*differing=\([0-9]*\).*/\1/' <<<"$got")
  same "$name -> .mp4 is the same samples" "0" "$differing"
done

# --- LPCM is smart rendered -------------------------------------------------
# The range ends part way through a 240 sample LPCM frame, so that frame is
# some samples of the recording and then samples that were on the far side of
# the cut -- and what smart rendering does to those is silence them. Copied,
# they would be a few milliseconds of whatever came next. Everything before
# that frame is the recording's own samples, byte for byte.
#
# Where the range ends is asked of the plan rather than assumed. It is not
# the 3.456 that was asked for: a bound is written to the last whole picture
# before it, which can be up to half a frame earlier, and the split inside
# the LPCM frame moves with it.
end=$(sed -n 's/^  keep \([0-9.]*\) -> \([0-9.]*\).*/\1 \2/p' "$OUT/cut/lpcm.ts.log" | head -1)
kept_samples=$(python3 -c "a,b='$end'.split(); print(round((float(b)-float(a))*48000))")
got=$(python3 tests/bd_audio.py align "$OUT/lpcm.m2ts" "$OUT/cut/lpcm.ts" 2)
length=$(sed 's/.*length=\([0-9]*\).*/\1/' <<<"$got")
first=$(sed 's/.*first=\([0-9]*\).*/\1/' <<<"$got")
# Nothing before the last frame differs, and what does differ begins inside
# the kept stretch rather than after it: the fade out is 48 samples of raised
# cosine ending on the cut, so that the step into the silence is a slope and
# not a cliff an encoder would answer with short windows. The plan's own end
# is printed to the millisecond, which is 48 samples, so it is only good
# enough to say which side of the cut the fade is on.
if [ "$first" -gt "$((length - 288))" ] && [ "$first" -lt "$kept_samples" ]; then
  ok "lpcm .ts differs only where it is faded out" "from $first of $kept_samples kept"
else
  bad "lpcm .ts differs only where it is faded out" \
    "want $((length - 287))..$((kept_samples - 1)), got $first"
fi
kept=$(python3 tests/bd_audio.py peak "$OUT/cut/lpcm.ts" 2 "$((length - 240))" "$((first))")
gone=$(python3 tests/bd_audio.py peak "$OUT/cut/lpcm.ts" 2 "$kept_samples" "$length")
same "everything past the cut is silent" "0" "$gone"
if [ "$kept" -gt 0 ]; then ok "and what is kept is not" "peak $kept"
else bad "and what is kept is not" "peak $kept"; fi

# --- the lossless codecs arrive as they left --------------------------------
# Nothing here re-encodes a frame of DTS or TrueHD: the encoders libavformat
# has for them write the lossy core and drop what makes them lossless, so a
# "patched" frame would be a hole rather than a trim. The audio a cut of one
# holds is therefore a stretch of the recording's own, byte for byte.
es() { ffmpeg -hide_banner -loglevel error -y -i "$1" -map 0:a:0 -c copy -f data "$2"; }
for name in dts truehd; do
  es "$OUT/$name.m2ts" "$OUT/$name.src.es"
  es "$OUT/cut/$name.ts" "$OUT/$name.cut.es"
  if python3 -c 'import sys
src = open(sys.argv[1], "rb").read()
cut = open(sys.argv[2], "rb").read()
sys.exit(0 if cut and src.find(cut) >= 0 else 1)' "$OUT/$name.src.es" "$OUT/$name.cut.es"; then
    ok "$name is carried through byte for byte" "$(wc -c <"$OUT/$name.cut.es") bytes"
  else
    bad "$name is carried through byte for byte" "the cut is not a stretch of the source"
  fi
  has_note=$(grep -c "carried through byte for byte" "$OUT/cut/$name.ts.log")
  same "$name says so" "1" "$has_note"
done

# --- and TrueHD in an MP4 is written, with a warning ------------------------
same "truehd .mp4 is called out as non-standard" "1" \
  "$(grep -c "outside the standard" "$OUT/cut/truehd.mp4.log")"

# --- asking for the one thing lossless sound cannot be given ----------------
# A whole-track re-encode, or a downmix, which is a re-encode by another
# name. Neither can be done to TrueHD without taking away what it is, so both
# are declined -- and declined is not the same as failed: the cut is written,
# with the track as it was.
"$BIN" "$OUT/truehd.m2ts" --keep 1.234-3.456 --audio-mode reencode \
  -o "$OUT/cut/truehd-re.ts" >"$OUT/cut/truehd-re.log" 2>&1
same "reencode is declined, not obeyed" "truehd" "$(audio_codec "$OUT/cut/truehd-re.ts")"
same "and says why" "1" "$(grep -c "carried through as it is" "$OUT/cut/truehd-re.log")"
"$BIN" "$OUT/truehd.m2ts" --keep 1.234-3.456 --audio-channels 1 \
  -o "$OUT/cut/truehd-1ch.ts" >"$OUT/cut/truehd-1ch.log" 2>&1
same "a downmix of it is declined too" "2" \
  "$(ffprobe -v error -select_streams a:0 -show_entries stream=channels \
       -of default=nw=1:nk=1 "$OUT/cut/truehd-1ch.ts" | head -1)"
same "and says which count it kept" "1" \
  "$(grep -c "2 channels, not the 1 asked for" "$OUT/cut/truehd-1ch.log")"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
