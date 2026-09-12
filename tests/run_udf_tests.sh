#!/usr/bin/env bash
# The image a disc is handed over as, in three layers.
#
# A burner and a player do not read the recording first. They read the
# anchors, then the descriptors those point at, then the file entries, and
# only then is there a disc at all -- so an image that is wrong in any of
# those is a disc that never opens, and the recording inside it being perfect
# does not help. `tests/udf_shape.py` reports all three; this checks them.
#
# The measure is not the specification alone but what real writers do. When
# reference images are at hand -- $SMARTCUT_DISCS, which wants images written
# by a recorder and by a burner -- their shape is printed beside ours and the
# places we are the odd one out are named. They are not failures: a recorder
# writes an overwritable partition where an image file is read-only, and both
# are right. The failures are the layer-one and layer-two invariants, which
# hold for every image anyone can open.
set -u
cd "$(dirname "$0")/.."
BIN=rust/target/release/smartcut
WORK="${TMPDIR:-/tmp}/smartcut-udf"
DISCS="${SMARTCUT_DISCS:-$HOME/Documents/claude/TMPGEnc}"
SHAPE="python3 tests/udf_shape.py"

[ -x "$BIN" ] || { echo "build first: (cd rust && cargo build --release)" >&2; exit 2; }
command -v ffmpeg >/dev/null || { echo "needs ffmpeg" >&2; exit 2; }

pass=0; fail=0
ok()   { printf "  ok    %-44s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %-44s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }
same() { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "want [$2], got [$3]"; fi }
# For a comparison too long to print: the answer is a count, the difference
# is the lines that are not in both.
same_list() { # name want got what
  if [ "$2" = "$3" ]; then ok "$1" "$(wc -l <<<"$2") $4"
  else bad "$1" "$(diff <(echo "$2") <(echo "$3") | tr '\n' ' ')"; fi
}
field() { sed -n "s/^$2=//p" <<<"$1"; }

rm -rf "$WORK"; mkdir -p "$WORK"

# Twenty seconds of something to cut. What the image holds does not matter to
# the filesystem around it; that it is a real stream the writer measured does.
echo "building a disc in $WORK ..."
ffmpeg -v error -y -f lavfi -i "testsrc2=size=1440x1080:rate=30000/1001:duration=20" \
       -f lavfi -i "sine=frequency=440:duration=20" \
       -c:v mpeg2video -b:v 4M -c:a mp2 -f mpegts "$WORK/src.ts" || exit 2

# --- what we write -------------------------------------------------------
for rev in 2.50 2.60; do
  out="$WORK/disc-$rev"
  "$BIN" "$WORK/src.ts" --keep 1.0-9.0 --bdav "$out" --iso "$rev" >/dev/null 2>&1 \
    || { bad "UDF $rev: an image is written"; continue; }
  s=$($SHAPE "$out.iso") || { bad "UDF $rev: the image can be read back"; continue; }

  echo "UDF $rev"
  # Layer one: the descriptors a reader finds before it finds anything else.
  # An anchor at 256 and an anchor at the last sector; a drive that cannot
  # read one reads the other, and a reader that finds neither finds no disc.
  sectors=$(field "$s" sectors)
  same "  an anchor at each end" "256,$((sectors - 1))" "$(field "$s" anchors)"
  same "  the revision asked for" "$rev" "$(field "$s" udf_revision)"
  # 2.50 is what the metadata partition arrived in, so it is the oldest
  # reader that can be expected to open either.
  same "  the oldest reader named" "2.50" "$(field "$s" reads_at)"
  same "  the integrity is closed" "closed" "$(field "$s" integrity)"
  same "  the mirror is a cluster away" "yes" "$(field "$s" mirror_apart)"

  # Layer two: the partition, and the files inside it staying inside it.
  IFS=+ read -r start blocks <<<"$(field "$s" partition)"
  from=$(field "$s" data_from); to=$(field "$s" data_to)
  if [ "$from" -ge "$start" ] && [ "$to" -le $((start + blocks)) ]; then
    ok "  every file is inside the partition" "$from..$to in $start+$blocks"
  else
    bad "  every file is inside the partition" "$from..$to outside $start+$blocks"
  fi
  if [ $((start + blocks)) -le "$sectors" ]; then
    ok "  the partition is inside the image"
  else
    bad "  the partition is inside the image" "$start+$blocks > $sectors"
  fi
  same "  nothing is written over anything" "0" "$(field "$s" overlaps)"
  same "  no stale file identifiers" "0" "$(field "$s" deleted_ids)"
  # The length field of an allocation descriptor is thirty bits, so an extent
  # stops short of a gigabyte whatever the file is.
  longest=$(field "$s" longest_extent)
  if [ "$longest" -le 524287 ]; then
    ok "  no extent overruns its length field" "$longest blocks"
  else
    bad "  no extent overruns its length field" "$longest blocks"
  fi
  # A Blu-ray is written in 64 KB clusters and read in them; a stream that
  # begins inside one is a stream every read of which straddles two.
  same "  the streams begin on a cluster" "yes" "$(field "$s" stream_starts_aligned)"

  # Layer three: the image holds the disc it was made of, byte for byte.
  # Sizes read out of the file entries against the folder the image was made
  # from -- the one comparison that says the filesystem is not merely
  # well-formed but true.
  want=$(cd "$out" && find . -type f -printf '%P %s\n' | sed 's|^BDAV/||' | sort)
  got=$($SHAPE --tree "$out.iso" | sed -n 's|^/BDAV/\(.*\)  \([0-9]*\) bytes.*|\1 \2|p' | sort)
  same_list "  the image holds the folder" "$want" "$got" "files, each its own size"
  same "  and counts what it holds" "$(wc -l <<<"$want")" "$(field "$s" files)"
done

# --- what the discs do ---------------------------------------------------
# Only the keys where a writer has a choice. Ours is printed first; where the
# reference images agree with each other and not with us, that is worth a
# look, and where they disagree there is no single right answer to be had.
keys="anchors vds_main file_set access metadata metadata_bitmap mirror_apart
      longest_extent stream_starts_aligned stream_runs_aligned deleted_ids"
# One image per writer: what is being compared is the dialect, and a second
# disc from the same program is not a second opinion.
declare -a shapes=() writers=() paths=()
if [ -d "$DISCS" ]; then
  while IFS= read -r iso; do
    r=$($SHAPE "$iso" 2>/dev/null) || continue
    who=$(field "$r" writer)
    # Our own images among them are not another opinion either.
    [ "$who" = "*SmartCut" ] && continue
    case " ${writers[*]:-} " in *" $who "*) continue ;; esac
    writers+=("$who"); shapes+=("$r"); paths+=("$iso")
  done < <(find "$DISCS" -name '*.iso' -size +1G 2>/dev/null | sort)
fi
if [ ${#shapes[@]} -eq 0 ]; then
  echo "reference images: none under $DISCS -- skipping the comparison"
else
  echo "beside what ${#shapes[@]} other writer(s) do"
  ours=$($SHAPE "$WORK/disc-2.50.iso")
  # Named by who wrote them, never by what is on them.
  row=$(printf "  %-22s %-22s" "" "$(field "$ours" writer)")
  for r in "${shapes[@]}"; do row+=$(printf "%-22s" "$(field "$r" writer)"); done
  printf "%s\n" "$row"
  for k in $keys; do
    row=$(printf "  %-22s %-22s" "$k" "$(field "$ours" "$k")")
    for r in "${shapes[@]}"; do
      v=$(field "$r" "$k"); row+=$(printf "%-22s" "${v:--}")
    done
    printf "%s\n" "$row"
  done

  # And they all still open. A shape that looks right is not the same claim:
  # what made this worth checking is a recorder's images, every clip on which
  # ends in space the recording never reached, being refused by our reader as
  # a file with a hole in it until it learned the difference.
  echo
  for i in "${!paths[@]}"; do
    if out=$("$BIN" "${paths[$i]}" 2>&1) && grep -q '^disc' <<<"$out"; then
      # What it read, not what is on it: a disc's name is its programme's.
      ok "an image by ${writers[$i]} opens" "$(grep -oP '\d+ recording\(s\)' <<<"$out")"
    else
      bad "an image by ${writers[$i]} opens" "$(head -1 <<<"$out")"
    fi
  done
fi

echo
echo "  passed $pass, failed $fail"
[ "$fail" -eq 0 ]
