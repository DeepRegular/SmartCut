#!/usr/bin/env bash
# Every suite that runs on synthetic material, one after another, with a line
# at the end for each that failed. What CI runs (.github/workflows/ci.yml).
#
#   bash tests/ci.sh              all of them
#   bash tests/ci.sh join vfr     only these, by the name between run_ and _tests
#
# Left out are the suites that only ever read real recordings from ~/media
# (aac, audio_content, broadcast, cm, pulldown, scene, ts_layout) and the VC-1
# encoder's (vc1), which wants a disc. Without the material they pass by
# skipping everything, or in cm's case fail for it, and neither says anything.
# The suites below that also read real material skip that part on their own.
#
# Built first, with the examples the suites run. A suite that builds an
# example itself does it with whatever RUSTFLAGS is set here, so a checked
# build (overflow and debug assertions on, as CI has it) stays checked.
#
# Not on a small machine all at once: the whole set at once is what took the
# development VM's host down. One or two by name there.
set -u
cd "$(dirname "$0")/.."

SUITES=(
  # First: the fixtures every other suite reuses are generated here.
  tests
  rust audio downmix surround71
  audio_codec audio_smart audio_format audio_head bilingual
  preview index proxy
  disc bdav udf dvd bd_audio
  vp9_av1 vfr demux transrate join
)
[ $# -gt 0 ] && SUITES=("$@")

(cd rust && cargo build --release --locked --bins --examples) || exit 2

# GitHub Actions folds each suite's output under its name; anywhere else the
# markers are just lines.
group() { [ -n "${GITHUB_ACTIONS:-}" ] && echo "::group::$1" || echo "=== $1"; }
endgroup() { [ -n "${GITHUB_ACTIONS:-}" ] && echo "::endgroup::"; return 0; }

failed=()
for s in "${SUITES[@]}"; do
  script="tests/run_${s}.sh"
  [ "$s" = tests ] || script="tests/run_${s}_tests.sh"
  [ -f "$script" ] || { echo "no such suite: $s" >&2; failed+=("$s"); continue; }
  group "$s"
  start=$SECONDS
  log="${TMPDIR:-/tmp}/smartcut-ci-$s.log"
  bash "$script" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  endgroup
  echo "$s: exit $rc in $((SECONDS - start)) s"
  # A panic is what a checked build is for, and a suite that counts only
  # mismatches passes over one that something downstream recovered from.
  if grep -q "panicked at" "$log"; then
    echo "$s: a panic in the output above"
    rc=1
  fi
  [ "$rc" -eq 0 ] || failed+=("$s")
done

echo
if [ ${#failed[@]} -eq 0 ]; then
  echo "all ${#SUITES[@]} suites passed"
else
  echo "failed: ${failed[*]}"
  exit 1
fi
