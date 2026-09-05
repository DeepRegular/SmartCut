#!/usr/bin/env bash
# The other two things a sample is: how often it is taken, and how wide it is.
#
# Both are settings of the same kind as a downmix. A sample at 44.1 kHz does
# not sit on the grid a 48 kHz recording's frames sit on, and a 16 bit sample
# is not a 24 bit one, so neither can be spliced in among the recording's own
# frames -- asking for either asks for the whole track, and the engine says
# so and re-encodes.
#
# What each has to prove is different, though.
#
#   The rate  has to reach three places at once: the samples themselves, the
#             stream's declaration of what they are, and -- for AAC in a
#             transport stream -- the ADTS header on every frame, which is
#             what a decoder reads before it reads a sample. A resample that
#             misses the third plays the whole track at the wrong speed.
#             And not every codec speaks every rate: AC-3 has three, Blu-ray
#             LPCM has three others, and a rate one of them does not have has
#             to come out as the nearest it does, out loud.
#
#   The width has nowhere to go in a lossy codec at all -- the encoder takes a
#             float and spends a bitrate -- so it is honoured where samples
#             are written down and declined out loud everywhere else. Where it
#             is honoured it decides the size of the file outright: channels
#             times width times the rate, and nothing else.
#
# The tones are how "the sound survived" is asked: each channel of the fixture
# carries one of its own, and a resample that dropped, doubled or slid the
# samples shows up as a peak that moved. A resample is the one operation here
# that could move a frequency, which is exactly why it is measured this way.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-audio-format"
mkdir -p "$OUT"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
python3 -c "import numpy" 2>/dev/null || { echo "  SKIP  numpy not installed"; exit 0; }

# The same 5.1 fixture the downmix and codec suites use. FL=400 FR=600 FC=200
# LFE=800 (which no encoder keeps) BL=1000 BR=1200, at 48 kHz, which is the
# rate a broadcast has and the one there is something to move away from.
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

TONES="400;600;200;;1000;1200"

pass=0; fail=0
ok()   { printf "  ok    %-46s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-46s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

# One line: ffprobe names a transport stream's streams once per programme and
# once on their own.
field() { # <file> <field>
  ffprobe -v error -select_streams a:0 -show_entries "stream=$2" \
    -of default=nw=1:nk=1 "$1" | head -1
}

render() { # <name> <ext> <args...>
  local name=$1 ext=$2; shift 2
  "$BIN" "$FX/surround.ts" --cut 5-10 --audio-mode reencode \
    -o "$OUT/$name.$ext" "$@" >"$OUT/$name.$ext.log" 2>&1
}

# Every channel still carrying its own tone and nothing else, read at whatever
# rate the file now claims -- which is the point: the tones are a fact about
# the sound, and a resample that got the arithmetic wrong moves them.
tones() { # <name> <file> <channels>
  if python3 tests/downmix.py "$2" "$3" "$TONES" >"$2.tones" 2>&1; then
    ok "$1"
  else
    bad "$1" "see $2.tones"
    cat "$2.tones"
  fi
}

echo "running audio format tests ..."

# --- the rate reaches all three places -------------------------------------
render r441 ts --audio-samplerate 44100
same "44.1 kHz into a .ts"                  "44100" "$(field "$OUT/r441.ts" sample_rate)"
tones "and every tone is where it was"      "$OUT/r441.ts" 6
# The header, which is what a transport stream's decoder reads first. A track
# resampled to 44.1 whose frames still announce 48 plays 9% fast.
ffmpeg -v error -i "$OUT/r441.ts" -map 0:a:0 -c copy -f adts -y "$OUT/r441.aac"
if python3 tests/aac_frames.py "$OUT/r441.aac" --hz 44100 >"$OUT/r441.hdr" 2>&1; then
  ok "and every ADTS header says so"
else
  bad "and every ADTS header says so" "see $OUT/r441.hdr"; cat "$OUT/r441.hdr"
fi
render r32 mp4 --audio-samplerate 32000
same "32 kHz into an .mp4"                  "32000" "$(field "$OUT/r32.mp4" sample_rate)"
tones "and every tone is where it was"      "$OUT/r32.mp4" 6

# --- a rate the codec does not have ----------------------------------------
# AC-3 is written at 32, 44.1 or 48 kHz and nothing else; Blu-ray LPCM at 48,
# 96 or 192. An encoder handed a rate it does not list refuses to open at all,
# so the rate has to be taken to the nearest one it does -- and said.
render ac96 ts --audio-codec ac3 --audio-samplerate 96000
same "96 kHz of AC-3 comes back as 48"      "48000" "$(field "$OUT/ac96.ts" sample_rate)"
same "and says which rate it settled on"    "1" \
  "$(grep -c "the nearest it can be" "$OUT/ac96.ts.log")"
render pl441 ts --audio-codec lpcm --audio-samplerate 44100
same "44.1 kHz of Blu-ray LPCM likewise"    "48000" "$(field "$OUT/pl441.ts" sample_rate)"

# --- a rate asks for the whole track ---------------------------------------
# The same answer a downmix gets, for the same reason, said the same way.
"$BIN" "$FX/surround.ts" --cut 5-10 --audio-samplerate 44100 -o "$OUT/smart.ts" \
  >"$OUT/smart.log" 2>&1
same "a rate asked under smart rendering"   "44100" "$(field "$OUT/smart.ts" sample_rate)"
same "and says why"                         "1" \
  "$(grep -c "the whole track is re-encoded rather than smart" "$OUT/smart.log")"
# And the recording's own rate is not a resample, so it must not turn smart
# rendering into a whole-track re-encode behind the caller's back.
"$BIN" "$FX/surround.ts" --cut 5-10 --audio-samplerate 48000 -o "$OUT/noop.ts" \
  >"$OUT/noop.log" 2>&1
same "the recording's own rate changes nothing" "0" \
  "$(grep -c "the whole track is re-encoded" "$OUT/noop.log")"

# --- the width, where samples are written down -----------------------------
# Nobody chooses an uncompressed track's bitrate; it is channels times width
# times the rate, so the width is readable straight off the size.
# 6 x 24 x 48000 and 6 x 16 x 48000.
render b24 ts --audio-codec lpcm --audio-bits 24
same "24 bit LPCM in a .ts"                 "6912000" "$(field "$OUT/b24.ts" bit_rate)"
render b16 ts --audio-codec lpcm --audio-bits 16
same "16 bit LPCM in a .ts"                 "4608000" "$(field "$OUT/b16.ts" bit_rate)"
# In an MP4 the width is the codec: there is no one box for PCM of any width.
render b24 mp4 --audio-codec lpcm --audio-bits 24
same "24 bit LPCM in an .mp4 is pcm_s24be"  "pcm_s24be" "$(field "$OUT/b24.mp4" codec_name)"
render b16 mp4 --audio-codec lpcm --audio-bits 16
same "16 bit LPCM in an .mp4 is pcm_s16be"  "pcm_s16be" "$(field "$OUT/b16.mp4" codec_name)"
tones "and the sound survives either width" "$OUT/b24.mp4" 6

# --- a width and a rate together -------------------------------------------
# Both at once, where the codec can speak both: 6 x 24 x 44100. Big-endian
# PCM lists no rates of its own, so 44.1 is a rate it takes.
render both mp4 --audio-codec lpcm --audio-bits 24 --audio-samplerate 44100
same "24 bit at 44.1 kHz is the sum of both" "6350400" "$(field "$OUT/both.mp4" bit_rate)"
same "and the stream says the rate"          "44100"   "$(field "$OUT/both.mp4" sample_rate)"
tones "and the tones are still the tones"    "$OUT/both.mp4" 6

# --- a width asked of a codec that has no room for one ---------------------
# AAC takes a float and spends a bitrate. How many bits the sound had before
# it is not a number the format has anywhere to put, so the setting is
# declined out loud rather than quietly ignored.
render wide ts --audio-codec aac --audio-bits 24
same "a width asked of AAC is declined"     "1" \
  "$(grep -c "does not carry samples but a description of them" "$OUT/wide.ts.log")"
same "and the track is still AAC"           "aac" "$(field "$OUT/wide.ts" codec_name)"

# --- sync through a resample -----------------------------------------------
# A resample is a filter with a delay, and a delay unaccounted for is the
# whole track sliding against the pictures. The click fixture answers that at
# the sample: every 0.5 s an impulse, read back off the cut timeline.
#
# In MP4, as the impulse suite's own fixture is: `audio_sync.py` reads the
# clicks off a timeline that starts at zero, and a transport stream's does not.
if [ ! -f "$FX/clicks.mp4" ]; then
  echo "generating the click fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=30" \
    -f lavfi -i "aevalsrc=if(lt(mod(t\,0.5)\,0.002)\,0.9\,0):s=48000:d=30" -shortest \
    -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -bf 3 -b:v 1500k \
    -profile:v high -level 3.1 -pix_fmt yuv420p \
    -c:a aac -b:a 192k "$FX/clicks.mp4" || exit 2
fi
sync_check() { # <name> <out> <ranges> <args...>
  local name=$1 out="$OUT/$2" ranges=$3; shift 3
  rm -f "$out"
  if ! "$BIN" "$FX/clicks.mp4" "$@" -o "$out" >/dev/null 2>&1; then
    bad "$name" "the cutter failed"; return
  fi
  local res
  # A resample is a whole-track re-encode however it was asked for, so the
  # boundaries are sample-accurate and measured against that bar.
  res=$(OUT="$out" SRC="$FX/clicks.mp4" RANGES="$ranges" SMARTCUT_AUDIO=reencode \
        python3 tests/audio_sync.py)
  if [[ "$res" == OK* ]]; then ok "$name" "${res#OK|}"; else bad "$name" "${res#BAD|}"; fi
}
sync_check "sync at 44.1 kHz, cut middle"  s441.mp4 "0.0-8.0,20.0-30.0" \
  --cut 8.0-20.0 --audio-samplerate 44100
sync_check "sync at 32 kHz, three ranges"  s32.mp4 "1.3-5.7,9.1-14.3,21.7-27.9" \
  --keep 1.3-5.7 --keep 9.1-14.3 --keep 21.7-27.9 --audio-samplerate 32000

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
