#!/usr/bin/env bash
# Sign a release's files for the updater and write its manifest
# (see gui/src-tauri/src/update.rs and docs/technical/distribution.md).
#
#   ./sign-update.sh <folder holding the release's files>
#
# Writes, into that folder, a `<file>.minisig` beside each of the four files
# the updater fetches and `update.json` listing them. All of them are uploaded
# with the release: the manifest is read as
# releases/latest/download/update.json, so it has to be on every release
# marked Latest, the files beside it.
#
# The key is the one whose public half is gui/src-tauri/update-key.pub. Its
# secret half is not in the repository: SMARTCUT_UPDATE_KEY says where it is
# (default ~/.smartcut-update-key/smartcut.key). Lose it and every installed
# copy has to be updated by hand once, to a build carrying a new public key.
#
# The trusted comment -- which the signature covers -- names the version and
# the file, and the updater refuses a signature whose comment is anything
# else. A manifest pointing at an older release's signed file, or at one
# packaging's file for another, is refused for that reason.
set -euo pipefail
cd "$(dirname "$0")"

DIR=${1:?usage: $0 <release folder>}
KEY=${SMARTCUT_UPDATE_KEY:-$HOME/.smartcut-update-key/smartcut.key}
PUB=src-tauri/update-key.pub
VERSION=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json)

[ -f "$KEY" ] || { echo "no secret key at $KEY" >&2; exit 1; }
command -v rsign >/dev/null || { echo "rsign not found (cargo install rsign2)" >&2; exit 1; }

# The manifest's key for each packaging, and the file's name as the builds
# name it.
declare -A FILES=(
  [windows-setup]="SmartCut_${VERSION}_x64-setup.exe"
  [windows-portable]="smartcut-portable-x64-${VERSION}.zip"
  [linux-tar]="SmartCut-${VERSION}-linux-x86_64.tar.gz"
  [appimage]="SmartCut_${VERSION}_amd64.AppImage"
)

entries=()
for kind in windows-setup windows-portable linux-tar appimage; do
  name=${FILES[$kind]}
  file=$DIR/$name
  [ -f "$file" ] || { echo "missing: $file" >&2; exit 1; }
  rm -f "$file.minisig"
  rsign sign -W -s "$KEY" -p "$PUB" -x "$file.minisig" \
    -t "SmartCut $VERSION $name" "$file" >/dev/null
  # Checked with the public key the program carries, as the program will.
  rsign verify -p "$PUB" -x "$file.minisig" "$file" >/dev/null
  size=$(stat -c %s "$file")
  entries+=("    \"$kind\": {\"name\": \"$name\", \"size\": $size}")
  echo "signed $name ($size bytes)"
done

{
  echo "{"
  echo "  \"version\": \"$VERSION\","
  echo "  \"files\": {"
  for i in "${!entries[@]}"; do
    sep=,; [ "$i" -eq $((${#entries[@]} - 1)) ] && sep=
    echo "${entries[$i]}$sep"
  done
  echo "  }"
  echo "}"
} > "$DIR/update.json"
echo "wrote $DIR/update.json"
