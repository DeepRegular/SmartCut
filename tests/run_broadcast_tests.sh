#!/usr/bin/env bash
# What a cut of a broadcast recording still says about the broadcast.
#
# A recording is not only a picture and a sound. It carries the subtitles the
# broadcaster sent, sometimes a second language, and a running account of
# itself -- which service this is, what is on now, what follows, and what
# time it is. A cut that drops all of that is a file a recorder's library
# view cannot say anything about, and until this suite existed a cut dropped
# every bit of it: libavformat writes its own tables from the streams and
# stops there.
#
# So the checks here are of the things no frame comparison can see. The
# captions are compared byte for byte and range by range; the tables are
# compared against the recording's own.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
MEDIA="${SMARTCUT_MEDIA:-$HOME/media}"
WORK="${TMPDIR:-/tmp}/smartcut-broadcast"
mkdir -p "$WORK"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-26s %s\n" "$1" "$2"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-26s %s\n" "$1" "$2"; fail=$((fail+1)); }
skip() { printf "  SKIP  %-26s %s\n" "$1" "$2"; }

# The tables repeat every few hundred milliseconds, so the opening seconds
# say everything about them -- but not about the captions, which are the
# whole file. Hence two readers: a slice for the tables, the file for the
# rest.
tables() { head -c 20000000 "$1" > "$WORK/slice.ts"; python3 tests/broadcast.py "$WORK/slice.ts"; }
field()  { echo "$1" | sed -n "s/^$2=//p"; }

echo "録画が自分自身について言っていることが、カットにも残るか"

# What every cut has to get right whatever shape its tables are written in:
# the service it is of, and the map of the streams it actually carries.
common() {
  a=$1; b=$2; why=""
  for k in transport_stream_id service_id pmt_pid program_info; do
    av=$(field "$a" "$k"); bv=$(field "$b" "$k")
    [ "$av" = "$bv" ] || why="$why $k($av→$bv)"
  done
  # Every stream the cut carries has to be described the way the recording
  # described it. The ones it cannot carry are not looked for: a cut has no
  # data broadcast in it, and a map that listed one would be lying.
  echo "$b" | sed -n 's/^stream\.\([0-9a-f]*\)=.*/\1/p' | while read -r pid; do
    av=$(field "$a" "stream.$pid"); bv=$(field "$b" "stream.$pid")
    [ "$av" = "$bv" ] || echo "stream.$pid($av→$bv)"
  done > "$WORK/streams.bad"
  [ -s "$WORK/streams.bad" ] && why="$why $(tr '\n' ' ' < "$WORK/streams.bad")"
  # Whatever the programme description still names has to be a stream the
  # cut's own map lists, or the file is telling a player to go looking for
  # something that is not there.
  have=$(field "$b" components)
  for tag in $(field "$b" event_refs | fold -w2); do
    case "$have" in *"$tag"*) ;; *) why="$why 番組情報が $tag を名乗る(なし)" ;; esac
  done
  # And the programme itself, wherever the file chose to say it: the service
  # name, the descriptors and the times have to be the recording's own bytes.
  for k in service_descriptor event_descriptors event_time; do
    av=$(field "$a" "$k"); bv=$(field "$b" "$k")
    [ -z "$av" ] && continue
    [ "$av" = "$bv" ] || why="$why $k($av→$bv)"
  done
  echo "$why"
}

for name in atx.ts full_ntv.ts terrestrial_nhke.ts animax_kisekoi_01.ts; do
  src="$MEDIA/$name"
  if [ ! -f "$src" ]; then skip "$name" "no $name"; continue; fi
  a=$(tables "$src")

  # --- the default: a partial transport stream ---------------------------
  out="$WORK/out.ts"
  "$BIN" "$src" --keep 20.0-80.0 --keep 120.0-180.0 -o "$out" >/dev/null 2>&1
  if [ ! -s "$out" ]; then bad "$name 部分TS" "出力が空"; continue; fi
  b=$(tables "$out")
  why=$(common "$a" "$b")
  [ "$(field "$b" sit)" = "1" ] || why="$why SITなし"
  [ "$(field "$b" sit.well_formed)" = "1" ] || why="$why SITの長さが合わない"
  [ "$(field "$b" sit.service_id)" = "$(field "$a" service_id)" ] || why="$why SITのサービスが違う"
  # A partial transport stream carries the one table instead of the four it
  # replaces, so finding any of them is the failure, not the absence.
  [ -z "$(field "$b" sdt.service)" ] || why="$why SDTが残っている"
  [ -z "$(field "$b" eit.0)" ] || why="$why EITが残っている"
  [ -z "$(field "$b" clock)" ] || why="$why TOTが残っている"
  peak=$(field "$b" sit.peak_rate)
  [ -n "$peak" ] && [ "$peak" -gt 1000000 ] || why="$why 伝送レートが不当($peak)"
  if [ -z "$why" ]; then
    ok "$name 部分TS" "SIT v$(field "$b" sit.version) / $((peak / 1000)) kbps / 番組情報 $(field "$b" event_descriptors)"
  else
    bad "$name 部分TS" "録画と違う:$why"
  fi

  # And the captions, which are the one non-audio stream a cut can move.
  cap=$(python3 tests/captions.py "$src" "$out" 20.0-80.0 120.0-180.0)
  if [ "$(field "$cap" ok)" = "1" ]; then
    ok "$name の字幕" "$(field "$cap" cut) 個そのまま、ずれ幅 $(field "$cap" worst_spread_us) us"
  elif [ "$(field "$cap" source)" = "0" ]; then
    skip "$name の字幕" "この録画に字幕はありません"
  else
    bad "$name の字幕" "$(field "$cap" why)"
  fi
  rm -f "$out"

  # --- and the broadcast's own tables, which --tables broadcast keeps -----
  # Written over the first one rather than beside it: two cuts of a
  # broadcast recording are a third of a gigabyte, and a test suite that
  # needs both at once fails on a small /tmp for a reason that is not a bug.
  "$BIN" "$src" --keep 20.0-80.0 --keep 120.0-180.0 --tables broadcast -o "$out" >/dev/null 2>&1
  if [ ! -s "$out" ]; then bad "$name 放送テーブル" "出力が空"; else
    c=$(tables "$out")
    why=$(common "$a" "$c")
    [ "$(field "$a" original_network_id)" = "$(field "$c" original_network_id)" ] \
      || why="$why original_network_id"
    [ "$(field "$a" sdt.service)" = "$(field "$c" sdt.service)" ] || why="$why sdt.service"
    # The programme on now and the one after, hashed: the sections have to be
    # the recording's own and not something written here. eit.1 is the
    # programme that followed, which is not in this file at all -- it names
    # its own streams and is right to.
    for k in eit.0 eit.1; do
      av=$(field "$a" "$k"); cv=$(field "$c" "$k")
      [ -z "$av" ] && continue
      [ "$av" = "$cv" ] || why="$why $k"
    done
    [ -n "$(field "$c" clock)" ] || why="$why 時刻なし"
    [ "$(field "$c" sit)" = "0" ] || why="$why SITが混ざっている"
    if [ -z "$why" ]; then
      ok "$name 放送テーブル" "サービス $(field "$c" service_id) / 番組情報 $(field "$c" eit.0) / 時刻あり"
    else
      bad "$name 放送テーブル" "録画と違う:$why"
    fi
  fi
  rm -f "$out"

done

# --- 音声多重 -------------------------------------------------------------
#
# No recording to hand carries two sound tracks, so one is built: a real
# broadcast recording with a tone welded on beside its own audio. What that
# tests is the part that has no shortcut -- two tracks cut independently,
# each on its own frame grid, each with its own drift -- and the tone is
# there because a track that came out of the wrong place is audible in a way
# a hash does not describe.
echo
echo "音声多重放送を読み、両方の音声を書き出せるか"
BASE="$MEDIA/atx.ts"
DUAL="$WORK/dual.ts"
if [ ! -f "$BASE" ]; then
  skip "音声多重" "no atx.ts"
elif [ ! -f "$DUAL" ] && ! ffmpeg -hide_banner -loglevel error -y -t 200 -i "$BASE" \
       -f lavfi -t 200 -i "sine=frequency=1000:sample_rate=48000:duration=200" \
       -map 0:v:0 -map 0:a:0 -map 1:0 -map 0:s:0 \
       -c:v copy -c:a:0 copy -c:a:1 aac -b:a:1 128k -c:s copy \
       -metadata:s:a:0 language=jpn -metadata:s:a:1 language=eng \
       -streamid 0:4111 -streamid 1:4175 -streamid 2:4176 -streamid 3:4623 \
       -mpegts_pmt_start_pid 1039 -mpegts_service_id 333 "$DUAL" 2>/dev/null; then
  skip "音声多重" "副音声つきの素材を作れませんでした"
else
  out="$WORK/dual-out.ts"
  "$BIN" "$DUAL" --cut 60.0-90.0 -o "$out" >/dev/null 2>&1
  count() { ffprobe -v error -select_streams "$1" -show_entries packet=pts_time \
              -of csv=p=0 "$2" 2>/dev/null | wc -l; }
  # `sort -u`, because ffprobe lists a transport stream's streams once per
  # programme as well as once outright, and a broadcast recording's PAT names
  # more programmes than the one this file holds.
  tracks() { ffprobe -v error -select_streams a -show_entries stream="$1" \
               -of csv=p=0 "$2" 2>/dev/null | tr -d ',' | sed '/^$/d' | sort -u; }
  pids=$(tracks id "$out" | tr '\n' ' ')
  n0=$(count a:0 "$out"); n1=$(count a:1 "$out")
  if [ -z "$n1" ] || [ "$n1" = "0" ]; then
    bad "両方の音声" "2 本目が出力にありません"
  elif [ "$n0" != "$n1" ]; then
    bad "両方の音声" "本数が違う: $n0 と $n1"
  else
    ok "両方の音声" "各 $n0 パケット、PID $pids"
  fi

  # And the same recording with the second track switched off, which is what
  # the editor's track menu sends.
  "$BIN" "$DUAL" --cut 60.0-90.0 --drop-stream 2 -o "$WORK/dropped.ts" >/dev/null 2>&1
  left=$(tracks index "$WORK/dropped.ts" | wc -l)
  if [ "$left" = "1" ]; then
    ok "1 本を外す" "--drop-stream 2 で音声 1 本"
  else
    bad "1 本を外す" "音声が $left 本残りました"
  fi
  rm -f "$out" "$WORK/dropped.ts"
fi

echo "=== $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
