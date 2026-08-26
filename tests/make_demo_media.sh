#!/usr/bin/env bash
# Build the clip used for the screenshots in README.md, from nothing but
# ffmpeg's own sources -- no recorded material is involved.
#
# It is shaped like a Japanese HD broadcast recording so that the commercial
# detector has something real to bite on: 1440x1080 interlaced MPEG-2 at
# 29.97 fps with ADTS-style AAC, a station logo that is present during the
# programme and absent during the commercials, and a one-second silence at
# every junction of the 15-second commercial grid.
#
#   0:00-1:00  programme A   logo
#   1:00-2:00  4 spots x 15s no logo   <- block 1
#   2:00-2:45  programme B   logo
#   2:45-3:15  2 spots x 15s no logo   <- block 2
#   3:15-3:45  programme C   logo
#
# `--detect-cm --logo` finds both blocks, 90.09 s against a ground truth of
# 90.00 s.
set -euo pipefail
cd "$(dirname "$0")"
OUT=$(realpath -m "${1:-demo_broadcast.ts}")
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
cd "$TMP"
W=1440; H=1080; FPS=30000/1001
FONT=$(fc-match -f '%{file}' 'DejaVu Sans:bold' 2>/dev/null || echo /usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf)
echo "font: $FONT"
mkdir -p seg
: > list.txt

# palette: program = calm, CM = loud
PROG=(123a5c 14524a 3a2d5e 1d4f6b 4a2f3d 16455c 2e5136 45325c 1b3e60 5c3a1f)
CMC=(c0392b d35400 8e44ad c0392b 16a085 d35400)

n=0
mkseg() { # $1=dur $2=bgcolor $3=text $4=sub $5=logo(1/0)
  local f=seg/$(printf '%03d' $n).ts
  local vf="drawtext=fontfile=$FONT:text='$3':fontcolor=white@0.92:fontsize=110:x=(w-text_w)/2:y=(h-text_h)/2-40"
  vf="$vf,drawtext=fontfile=$FONT:text='$4':fontcolor=white@0.55:fontsize=46:x=(w-text_w)/2:y=(h-text_h)/2+90"
  if [ "$5" = 1 ]; then
    vf="$vf,drawtext=fontfile=$FONT:text='SYNTH TV':fontcolor=white@0.80:fontsize=44:box=1:boxcolor=black@0.35:boxborderw=18:x=w-text_w-70:y=54"
  fi
  ffmpeg -v error -y -f lavfi -i "color=c=0x$2:s=${W}x${H}:r=$FPS:d=$1" \
    -vf "$vf" -c:v mpeg2video -b:v 6M -minrate 6M -maxrate 6M -bufsize 3M \
    -g 15 -bf 2 -sc_threshold 1000000000 -aspect 16:9 -pix_fmt yuv420p \
    -flags +ilme+ildct -top 1 -f mpegts "$f"
  echo "file '$f'" >> list.txt
  n=$((n+1))
}

# --- program A : 60s, 12 scenes of 5s ---------------------------------------
for i in $(seq 0 11); do
  mkseg 5 "${PROG[$((i%10))]}" "PROGRAM" "part A - scene $((i+1))" 1
done
# --- CM block 1 : 60s = 4 spots of 15s --------------------------------------
for i in $(seq 0 3); do
  mkseg 15 "${CMC[$((i%6))]}" "CM" "spot $((i+1)) of 4  -  15s" 0
done
# --- program B : 45s ---------------------------------------------------------
for i in $(seq 0 8); do
  mkseg 5 "${PROG[$(((i+3)%10))]}" "PROGRAM" "part B - scene $((i+1))" 1
done
# --- CM block 2 : 30s = 2 spots ---------------------------------------------
for i in $(seq 0 1); do
  mkseg 15 "${CMC[$(((i+4)%6))]}" "CM" "spot $((i+1)) of 2  -  15s" 0
done
# --- program C : 30s ---------------------------------------------------------
for i in $(seq 0 5); do
  mkseg 5 "${PROG[$(((i+6)%10))]}" "PROGRAM" "part C - scene $((i+1))" 1
done

ffmpeg -v error -y -f concat -safe 0 -i list.txt -c copy -f mpegts video.ts

# --- audio: tone, silenced 1.0s at every CM junction (15s grid) --------------
DUR=225
SIL="between(t,59.5,60.5)+between(t,74.5,75.5)+between(t,89.5,90.5)+between(t,104.5,105.5)+between(t,119.5,120.5)+between(t,164.5,165.5)+between(t,179.5,180.5)+between(t,194.5,195.5)"
# a couple of ordinary in-programme pauses, deliberately short and off-grid
SIL="$SIL+between(t,31.2,31.5)+between(t,143.8,144.1)+between(t,206.4,206.7)"
TONE="0.28*sin(2*PI*(320+90*sin(2*PI*t/6.5))*t)"
ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs='if(gt($SIL,0),0,$TONE)':s=48000:d=$DUR" \
  -c:a aac -b:a 192k -ar 48000 -ac 2 audio.aac

ffmpeg -v error -y -i video.ts -i audio.aac -map 0:v -map 1:a -c copy -shortest \
  -muxrate 12M -f mpegts "$OUT"
ls -lh "$OUT"
ffprobe -v error -show_entries format=duration -show_entries stream=codec_name,width,height,field_order,r_frame_rate -of default=nw=1 "$OUT"
