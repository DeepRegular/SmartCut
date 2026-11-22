#!/usr/bin/env bash
# Build the Flatpak bundle (see docs/technical/distribution.md).
#   ./build-flatpak.sh   -> gui/src-tauri/target/flatpak/SmartCut-<version>-x86_64.flatpak
#
# Needs flatpak and flatpak-builder, and the runtime the manifest names:
#   flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
#   flatpak install --user flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
#     org.freedesktop.Sdk.Extension.rust-stable//25.08 org.freedesktop.Sdk.Extension.llvm20//25.08
#
# flatpak-builder keeps its state (downloads, the finished modules) outside
# the tree, in $FLATPAK_STATE: the manifest copies this working tree in as a
# source, and a state directory inside it would be copied along. A second
# run rebuilds only the modules whose sources changed -- after an edit to
# SmartCut, only SmartCut.
set -euo pipefail
cd "$(dirname "$0")"

APP=dev.smartcut.app
VERSION=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)
STATE=${FLATPAK_STATE:-$HOME/.cache/smartcut-flatpak}
OUT=src-tauri/target/flatpak
mkdir -p "$STATE" "$OUT"

grep -q "<release version=\"$VERSION\"" flatpak/$APP.metainfo.xml || {
  echo "error: flatpak/$APP.metainfo.xml has no <release> for $VERSION" >&2; exit 1; }

flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --state-dir="$STATE/state" --repo="$STATE/repo" \
  "$STATE/build" flatpak/$APP.yml

BUNDLE=$OUT/SmartCut-$VERSION-x86_64.flatpak
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$STATE/repo" "$BUNDLE" $APP
echo; echo "bundle: $PWD/$BUNDLE"
