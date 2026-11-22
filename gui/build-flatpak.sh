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
#
# The repository is signed with the key in $FLATPAK_GPG_HOME (a key made for
# this alone, without a passphrase; see docs/technical/distribution.md), and
# the bundle names the repository on GitHub Pages as its origin, so an
# install from it takes later versions from `flatpak update`. Putting the
# repository on Pages is gui/publish-flatpak.sh.
set -euo pipefail
cd "$(dirname "$0")"

APP=dev.smartcut.app
VERSION=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)
STATE=${FLATPAK_STATE:-$HOME/.cache/smartcut-flatpak}
GPG_HOME=${FLATPAK_GPG_HOME:-$HOME/.smartcut-flatpak-gpg}
KEY=E8A2ACD236581DDDF68AE7FFA0DCC65533C608DA
REPO_URL=https://deepregular.github.io/smartcut-flatpak/repo/
OUT=src-tauri/target/flatpak
mkdir -p "$STATE" "$OUT"
gpg --homedir "$GPG_HOME" --list-secret-keys "$KEY" >/dev/null || {
  echo "error: no signing key $KEY in $GPG_HOME" >&2; exit 1; }

grep -q "<release version=\"$VERSION\"" flatpak/$APP.metainfo.xml || {
  echo "error: flatpak/$APP.metainfo.xml has no <release> for $VERSION" >&2; exit 1; }

flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --gpg-sign="$KEY" --gpg-homedir="$GPG_HOME" \
  --state-dir="$STATE/state" --repo="$STATE/repo" \
  "$STATE/build" flatpak/$APP.yml

# Only the newest commit is kept: a client updating needs that and nothing
# older, and Pages holds the repository whole.
flatpak build-update-repo --title=SmartCut --prune --prune-depth=0 \
  --gpg-sign="$KEY" --gpg-homedir="$GPG_HOME" "$STATE/repo"

gpg --homedir "$GPG_HOME" --export "$KEY" > "$STATE/key.gpg"
BUNDLE=$OUT/SmartCut-$VERSION-x86_64.flatpak
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  --repo-url="$REPO_URL" --gpg-keys="$STATE/key.gpg" \
  --gpg-sign="$KEY" --gpg-homedir="$GPG_HOME" \
  "$STATE/repo" "$BUNDLE" $APP
echo; echo "bundle: $PWD/$BUNDLE"
