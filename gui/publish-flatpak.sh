#!/usr/bin/env bash
# Put the Flatpak repository that gui/build-flatpak.sh signed on GitHub Pages
# (see docs/technical/distribution.md):
#   ./publish-flatpak.sh   -> https://deepregular.github.io/smartcut-flatpak/
#
# The site is the repository DeepRegular/smartcut-flatpak, served from its
# main branch. Each run replaces it with one commit holding the newest
# OSTree repository and the two files that point at it:
#   smartcut.flatpakref    flatpak install --user <url of this file>
#   smartcut.flatpakrepo   flatpak remote-add --user smartcut <url of this file>
# Its history is thrown away every time, so the site stays one version big
# and a clone of it stays small; a client updating needs only the newest
# commit, which the OSTree repository carries itself.
set -euo pipefail
cd "$(dirname "$0")"

STATE=${FLATPAK_STATE:-$HOME/.cache/smartcut-flatpak}
GPG_HOME=${FLATPAK_GPG_HOME:-$HOME/.smartcut-flatpak-gpg}
KEY=E8A2ACD236581DDDF68AE7FFA0DCC65533C608DA
SITE=https://deepregular.github.io/smartcut-flatpak
REMOTE=https://github.com/DeepRegular/smartcut-flatpak.git
PAGES=$STATE/pages
VERSION=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)

[ -f "$STATE/repo/config" ] || { echo "error: no repository in $STATE/repo; run build-flatpak.sh" >&2; exit 1; }
flatpak repo --branches "$STATE/repo" | grep -q "app/dev.smartcut.app/" || {
  echo "error: $STATE/repo holds no dev.smartcut.app" >&2; exit 1; }

KEY64=$(gpg --homedir "$GPG_HOME" --export "$KEY" | base64 -w0)
rm -rf "$PAGES"
mkdir -p "$PAGES"
cp -a "$STATE/repo" "$PAGES/repo"
# Nothing on Pages is to go through Jekyll: it leaves out names that start
# with an underscore or a dot.
touch "$PAGES/.nojekyll"

cat > "$PAGES/smartcut.flatpakref" <<EOF
[Flatpak Ref]
Name=dev.smartcut.app
Branch=master
Title=SmartCut
IsRuntime=false
Url=$SITE/repo/
SuggestRemoteName=smartcut
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
Homepage=https://github.com/DeepRegular/SmartCut
GPGKey=$KEY64
EOF

cat > "$PAGES/smartcut.flatpakrepo" <<EOF
[Flatpak Repo]
Title=SmartCut
Url=$SITE/repo/
Homepage=https://github.com/DeepRegular/SmartCut
GPGKey=$KEY64
EOF

cat > "$PAGES/index.html" <<EOF
<!doctype html>
<meta charset="utf-8">
<title>SmartCut Flatpak</title>
<h1>SmartCut Flatpak repository</h1>
<p>SmartCut $VERSION. Install, and take later versions with <code>flatpak update</code>:</p>
<pre>flatpak install --user $SITE/smartcut.flatpakref</pre>
<p>Signing key fingerprint: <code>$KEY</code></p>
<p><a href="https://github.com/DeepRegular/SmartCut">SmartCut on GitHub</a></p>
EOF

cd "$PAGES"
git init -q -b main
git add -A
git -c user.name="$(git -C "$OLDPWD" config user.name)" \
    -c user.email="$(git -C "$OLDPWD" config user.email)" \
    commit -q -m "SmartCut $VERSION"
git push -q --force "$REMOTE" main
echo "published: $SITE/smartcut.flatpakref"
