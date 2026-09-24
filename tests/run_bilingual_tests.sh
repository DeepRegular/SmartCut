#!/usr/bin/env bash
# A recording whose second sound track begins where the programme does.
#
# A Japanese broadcast sends a programme in two languages as two sound tracks
# on their own pids, and a programme with commentary for a viewer who cannot
# see the picture the same way. Neither arrives until the programme starts,
# and a recorder starts before that -- so the head of the file holds the end
# of whatever was on before, with one sound track in it.
#
# libavformat probes the head and stops after five megabytes, which on a
# broadcast is about two and a half seconds. The second track is then a
# stream it has listed and cannot describe: no sample rate, no channel count.
# Believed, that description loses the track -- it is not listed, not named,
# and not carried into the cut.
#
# Measured over 400 broadcast recordings: 37 carry two sound tracks, and 22
# of those announce the second only past where the probe stops.
#
# The fixture is that shape in miniature. Beside it, the same recording with
# both tracks there from the first frame, which must read exactly as it did
# before -- a deeper probe is not allowed to change what an ordinary
# recording says about itself.
#
# What the fixture cannot reproduce is the other half of the same fault: a
# *map* that names one track and is replaced by one that names two. A
# synthetic recording carries three streams and libavformat has described all
# three within a second, so it stops reading long before the second map --
# where a broadcast, with its carousels and its crawl, is still reading at
# thirty megabytes. That half is tested in `si.rs`, against a map written
# by hand.
set -u
cd "$(dirname "$0")/.."
BIN=${BIN:-rust/target/release/smartcut}
FX="${TMPDIR:-/tmp}/smartcut-fixtures"
mkdir -p "$FX"
[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-46s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-46s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi; }

# Sixteen megabits a second, which is about what a Japanese broadcast is
# carried at. The rate is the point: libavformat stops probing at five
# megabytes *or* five seconds of stream, whichever comes first, so a fixture
# written at a tenth of a broadcast's rate would reach the five seconds long
# before the seven megabytes and no probe could ever find the second track.
if [ ! -f "$FX/late_sound.ts" ] || [ ! -f "$FX/both_tracks.ts" ]; then
  echo "generating the two-track fixture ..."
  [ -f "$FX/two_tracks.ts" ] || ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=1280x720:rate=30:duration=12" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=12" \
    -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=12" \
    -map 0:v -map 1:a -map 2:a \
    -c:v mpeg2video -g 15 -keyint_min 15 -b:v 16000k -minrate 16000k \
    -maxrate 16000k -bufsize 4000k \
    -c:a aac -b:a 160k -f mpegts "$FX/two_tracks.ts" || exit 2
  # The programme's second sound track begins seven megabytes in, which is
  # where the programme does. Named by the map from the first frame -- a
  # recorder keeps the map it was given -- so libavformat lists a stream and
  # cannot say what is in it, and a reader that believes the description
  # throws the track away.
  python3 tests/late_audio.py "$FX/two_tracks.ts" "$FX/late_sound.ts" \
    --hide-data 7000000 || exit 2
  # And the same recording with the track there from the start, which is what
  # the deeper probe must not change.
  python3 tests/late_audio.py "$FX/two_tracks.ts" "$FX/both_tracks.ts" || exit 2
fi

# How many sound tracks the cutter lists, and what it calls the main one.
tracks_of() { "$BIN" "$1" 2>/dev/null | grep -c "^audio"; }
main_pid()  { "$BIN" "$1" 2>/dev/null | sed -n 's/^audio.*main.*pid 0x\([0-9a-f]*\).*/\1/p'; }

# What libavformat alone makes of it, at the depth it probes to unasked. The
# fixture is only a fixture if the shallow read really does lose the track.
shallow=$(ffprobe -v error -probesize 5000000 -select_streams a \
  -show_entries stream=channels -of default=nw=1:nk=1 "$FX/late_sound.ts" \
  2>/dev/null | sort -u | tr '\n' ' ')
case "$shallow" in
  *0*) ok "the shallow probe cannot describe the second track" "channels: $shallow" ;;
  *)   bad "the shallow probe cannot describe the second track" "channels: $shallow" ;;
esac

echo
echo "a recording whose second sound track starts where the programme does"
same "both tracks are listed" "2" "$(tracks_of "$FX/late_sound.ts")"
same "and the first is the main sound" "0101" "$(main_pid "$FX/late_sound.ts")"

echo
echo "and the same recording with nothing hidden"
same "both tracks are listed" "2" "$(tracks_of "$FX/both_tracks.ts")"
same "and the first is the main sound" "0101" "$(main_pid "$FX/both_tracks.ts")"

# What the cut carries. A track that is listed but not written is no better
# than one that was never found.
echo
echo "and a cut of it carries both"
if "$BIN" "$FX/late_sound.ts" --keep 2.0-8.0 -o "$FX/late_cut.ts" >/dev/null 2>&1; then
  kept=$(ffprobe -v error -select_streams a -show_entries stream=id \
    -of default=nw=1:nk=1 "$FX/late_cut.ts" 2>/dev/null | sort -u | wc -l)
  same "as many tracks as went in" "2" "$kept"
else
  bad "the cutter failed on the late-sound recording"
fi

# --- the other way round ----------------------------------------------------
#
# The programme before this one had two sound tracks, and this one has one:
# the second is there for the first seconds of the recording and never again.
# Every read that looks for it past that point -- the four places a track's
# shape is sampled at when the recording is opened, the frames around each
# seam of a cut -- found nothing and went on to the end of the file looking,
# because with every other stream thrown away libavformat does not come back
# until it finds one. A broadcast of two and a half hours was read some thirty
# times over while it was being added to the list, and a cut of the programme
# alone failed at the end for a track with nothing in it.
if [ ! -f "$FX/early_sound.ts" ]; then
  echo "generating the early-sound fixture ..."
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=6" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=6" \
    -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=6" \
    -map 0:v -map 1:a -map 2:a -c:v mpeg2video -g 15 -b:v 4000k -c:a aac -b:a 128k \
    -streamid 0:256 -streamid 1:257 -streamid 2:258 \
    -f mpegts "$FX/early_head.ts" || exit 2
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=90" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=90" \
    -map 0:v -map 1:a -c:v mpeg2video -g 15 -b:v 4000k -c:a aac -b:a 128k \
    -streamid 0:256 -streamid 1:257 -output_ts_offset 6 \
    -f mpegts "$FX/early_body.ts" || exit 2
  cat "$FX/early_head.ts" "$FX/early_body.ts" > "$FX/early_sound.ts"
fi

# What reading it costs, in bytes read. Counted against the same recording
# without its first six seconds, which has one track from start to end: the
# count includes the program loading its own libraries, which on a file this
# small is most of it, and that is the same either way. So is the deeper
# probe a recording with a second track is opened with -- thirty-two
# megabytes a time, which on this fixture is six times over and on a real
# recording nothing. The fault read the file some twenty-eight times more.
read_bytes() {
  "$@" >/dev/null 2>&1 &
  local p=$! r=0 now
  while kill -0 "$p" 2>/dev/null; do
    now=$(awk '/^rchar/{print $2}' "/proc/$p/io" 2>/dev/null) && [ -n "$now" ] && r=$now
    sleep 0.05
  done
  echo "$r"
}
echo
echo "a recording whose second sound track belongs to the programme before it"
size=$(stat -c%s "$FX/early_sound.ts")
bytes=$(read_bytes "$BIN" "$FX/early_sound.ts")
plain=$(read_bytes "$BIN" "$FX/early_body.ts")
more=$(( (bytes - plain) / size ))
if [ "$more" -le 10 ]; then
  ok "opening it does not read it over and over" "+${more}x"
else
  bad "opening it does not read it over and over" "+${more}x the file"
fi
if "$BIN" "$FX/early_sound.ts" --keep 20-40 --keep 60-80 -o "$FX/early_cut.ts" >/dev/null 2>&1; then
  kept=$(ffprobe -v error -select_streams a -show_entries stream=id \
    -of default=nw=1:nk=1 "$FX/early_cut.ts" 2>/dev/null | sort -u | wc -l)
  same "a cut of the programme writes its one track" "1" "$kept"
else
  bad "a cut of the programme writes its one track" "the cut failed"
fi
if "$BIN" "$FX/early_sound.ts" --keep 2-20 -o "$FX/early_cut2.ts" >/dev/null 2>&1; then
  kept=$(ffprobe -v error -select_streams a -show_entries stream=id \
    -of default=nw=1:nk=1 "$FX/early_cut2.ts" 2>/dev/null | sort -u | wc -l)
  same "a cut that reaches back into it keeps both" "2" "$kept"
else
  bad "a cut that reaches back into it keeps both" "the cut failed"
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
