#!/usr/bin/env bash
# Writing the sound as something other than what the recording carried.
#
# A whole-track re-encode is the one mode with nothing to copy, which is what
# makes a choice of codec possible at all: the other two splice the
# recording's own frames back in, and a frame cannot be spliced into a codec
# it is not in. So naming one asks for the whole track, the way a downmix
# does, and the engine says so and does it.
#
# Four codecs, and each has a different thing to prove:
#
#   AAC   is what the broadcast carried, so the interesting case is asking
#         for it from something that did not.
#   AC-3  needs a registration descriptor in the programme map, or a receiver
#         will not look at the stream type at all.
#   DTS   is behind libavcodec's "experimental" flag, and an encoder that is
#         never told that is understood refuses to open.
#   LPCM  is the awkward one. Only a transport stream can declare it, and
#         only one that has registered itself as HDMV -- write the same bytes
#         without that and every reader calls the track "bin_data" and
#         decodes nothing.
#
# The check for each is the same three questions, asked of each container:
# is the track the codec that was asked for, is the sound still the sound,
# and -- in a transport stream, whose map this program writes itself -- does
# the map say what the track actually is.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
OUT="${TMPDIR:-/tmp}/smartcut-audio-codec"
mkdir -p "$OUT"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
python3 -c "import numpy" 2>/dev/null || { echo "  SKIP  numpy not installed"; exit 0; }

# The same 5.1 fixture the downmix suite uses, and for the same reason: every
# channel carries a tone of its own, so "the sound survived" is a reading of
# the spectrum rather than a guess from the metadata. FL=400 FR=600 FC=200
# LFE=800 (which no encoder keeps -- an LFE channel is low-passed near 120 Hz)
# BL=1000 BR=1200.
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
ok()   { printf "  ok    %-44s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-44s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

# One line: ffprobe names a transport stream's streams once per programme and
# once on their own.
field() { # <file> <field>
  ffprobe -v error -select_streams a:0 -show_entries "stream=$2" \
    -of default=nw=1:nk=1 "$1" | head -1
}
# The stream type the output's own programme map gives the sound, which is
# the thing a player reads before it reads a byte of the track.
audio_type() { python3 tests/bd_audio.py types "$1" | sed -n "2p" | cut -d' ' -f2; }

# Not called `cut`: `cut` is a program this file also uses, and a function
# of that name is the one that would be found.
render() { # <name> <ext> <args...>
  local name=$1 ext=$2; shift 2
  "$BIN" "$FX/surround.ts" --cut 5-10 --audio-mode reencode \
    -o "$OUT/$name.$ext" "$@" >"$OUT/$name.$ext.log" 2>&1
}

echo "running audio codec tests ..."

# --- every codec, into every container -------------------------------------
# What each is written as, which is the codec asked for except where the
# container has no box for it: LPCM is Blu-ray's private stream in a
# transport stream and plain big-endian PCM everywhere else.
codec_of_aac=aac;  codec_of_ac3=ac3;  codec_of_dts=dts;  codec_of_lpcm=pcm_bluray
other_of_aac=aac;  other_of_ac3=ac3;  other_of_dts=dts;  other_of_lpcm=pcm_s16be
# And what the map has to call it. 0x0F is ADTS AAC, 0x81 AC-3, 0x82 DTS,
# 0x80 HDMV LPCM.
type_of_aac=0x0f;  type_of_ac3=0x81;  type_of_dts=0x82;  type_of_lpcm=0x80

for name in aac ac3 dts lpcm; do
  for ext in ts mp4 mkv mov; do
    eval "want=\$$([ "$ext" = ts ] && echo codec_of_$name || echo other_of_$name)"
    if render "$name" "$ext" --audio-codec "$name"; then
      same "$name -> .$ext" "$want" "$(field "$OUT/$name.$ext" codec_name)"
    else
      bad "$name -> .$ext" "$(tail -1 "$OUT/$name.$ext.log")"
    fi
  done
  eval "want=\$type_of_$name"
  same "$name .ts is declared 0x$(printf %02x $((want)))" "$want" \
    "$(audio_type "$OUT/$name.ts")"
  # Six channels in, six channels out, each still carrying its own tone. The
  # ADTS check the downmix suite makes is AAC's alone, so it is not asked
  # here: what is asked is the sound.
  echo "$name: the channels"
  if python3 tests/downmix.py "$OUT/$name.ts" 6 "$TONES" >"$OUT/$name.tones" 2>&1; then
    ok "$name keeps every channel where it was"
  else
    bad "$name keeps every channel where it was" "see $OUT/$name.tones"
    cat "$OUT/$name.tones"
  fi
done

# --- LPCM in a transport stream is readable at all --------------------------
# The muxer's own map declares it as private data of no stated kind, and a
# player handed that finds a stream it cannot name. The map this program
# writes says HDMV LPCM, and says HDMV in the programme's own descriptors,
# which is what makes 0x80 mean LPCM rather than one of the other things
# 0x80 has meant. Without both, this reads back as "bin_data".
same "lpcm .ts registers the programme as HDMV" "HDMV" \
  "$(python3 tests/bd_audio.py registrations "$OUT/lpcm.ts" | head -1)"

# --- the rate each codec is given, when nobody says ------------------------
# Following the recording's own rate says nothing once the codec is not the
# recording's: 384 kbit/s is what this fixture's AAC cost, and it is neither
# what the same programme is worth as AC-3 nor a rate DTS has at all. So the
# codec's own figure for the channel count is used instead.
same "ac3 at 5.1 is given 448 kbit/s"  "448000"  "$(field "$OUT/ac3.ts" bit_rate)"
same "dts at 5.1 is given 1536 kbit/s" "1536000" "$(field "$OUT/dts.ts" bit_rate)"
# And an explicit rate is taken as given, whatever the codec would have
# chosen for itself.
render ac3rate ts --audio-codec ac3 --audio-bitrate 256k
same "an explicit rate is obeyed" "256000" "$(field "$OUT/ac3rate.ts" bit_rate)"

# --- except where the encoder will not open at it ---------------------------
# A DTS frame carries a fixed number of samples and has to be long enough to
# describe every channel in it, which puts a floor under the bitrate that
# moves with the channels and with the rate: 5.1 at 48 kHz is not written
# under about 670 kbit/s. Asked for less, the cut used to stop where the
# encoder was opened. It writes what the codec is ordinarily carried at
# instead, and says so on the way past. The window never gets here -- it is
# told which rungs will open before anyone chooses one (`writable_sound`).
render dtsfloor ts --audio-codec dts --audio-bitrate 384k
same "a rate under the codec's floor is raised" "1536000" \
  "$(field "$OUT/dtsfloor.ts" bit_rate)"
same "and says so" "1" \
  "$(grep -c "is not written at that rate with 6 channels" "$OUT/dtsfloor.ts.log")"

# --- a channel count the codec has no arrangement for -----------------------
# DTS is written mono, stereo, quad, 5.0 or 5.1 and in no other count, and
# `avcodec_open2` handed anything else refuses with nothing in its message
# about channels. Said here instead -- and the window, told the same thing by
# `writable_sound`, greys the codec out rather than reaching this at all.
if render dts3 ts --audio-codec dts --audio-channels 3; then
  bad "a count the codec cannot arrange is refused" "the cut ran"
else
  same "a count the codec cannot arrange is refused" "1" \
    "$(grep -c "is not written with 3 channels" "$OUT/dts3.ts.log")"
fi

# --- and LPCM's, which is not a rate but a size ----------------------------
# Nobody chooses it: channels times bit depth times the sample rate. The
# depth is the recording's own, and a lossy recording has none -- it decodes
# to a 32 bit float, and the LPCM encoder handed one used to reach for the
# widest width it lists and write a broadcast out at 24 bits: half again the
# size, and not one sample better. 6 x 16 x 48000 is what it should be.
same "lpcm from a broadcast is 16 bit"  "4608000" "$(field "$OUT/lpcm.ts" bit_rate)"
same "and the same width in an mp4"     "4608000" "$(field "$OUT/lpcm.mp4" bit_rate)"
# Blu-ray's LPCM writes its channels in pairs and pads an odd count with a
# silent one, which is why a mono track costs two channels' worth of bytes in
# a transport stream and one channel's worth everywhere else. The window's
# figure for a greyed-out bitrate has to account for it, so it is measured
# here rather than assumed.
render mono ts --audio-codec lpcm --audio-channels 1
same "a mono lpcm .ts is a padded pair" "1536000" "$(field "$OUT/mono.ts" bit_rate)"
render mono mp4 --audio-codec lpcm --audio-channels 1
same "and an mp4 pads nothing"          "768000"  "$(field "$OUT/mono.mp4" bit_rate)"

# --- naming a codec asks for the whole track -------------------------------
# There is no copying a frame into a codec it is not in, so a codec named
# under smart rendering is a whole-track re-encode -- the same answer a
# downmix gets, and said out loud the same way.
"$BIN" "$FX/surround.ts" --cut 5-10 --audio-codec ac3 -o "$OUT/smart.ts" \
  >"$OUT/smart.log" 2>&1
same "a codec named under smart rendering" "ac3" "$(field "$OUT/smart.ts" codec_name)"
same "and says why" "1" "$(grep -c "the whole track is re-encoded rather than smart" "$OUT/smart.log")"

# --- asking for the recording's own codec changes nothing ------------------
# `--audio-codec aac` on an AAC recording is not a conversion, so it must not
# turn smart rendering into a whole-track re-encode behind the caller's back.
"$BIN" "$FX/surround.ts" --cut 5-10 --audio-codec aac -o "$OUT/noop.ts" \
  >"$OUT/noop.log" 2>&1
same "the recording's own codec is not a conversion" "0" \
  "$(grep -c "the whole track is re-encoded" "$OUT/noop.log")"

# --- a codec that is not offered -------------------------------------------
if "$BIN" "$FX/surround.ts" --cut 5-10 --audio-codec mp3 -o "$OUT/nope.ts" \
     >"$OUT/nope.log" 2>&1; then
  bad "an unknown codec is refused" "the cut ran"
else
  same "an unknown codec is refused" "1" "$(grep -c -- "--audio-codec wants" "$OUT/nope.log")"
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
