#!/usr/bin/env bash
# Put a .deb into the APT repository on GitHub Pages
# (see docs/technical/distribution.md):
#   FLATPAK_GPG_HOME=... ./publish-apt.sh smartcut_<version>_amd64.deb
#     -> https://deepregular.github.io/smartcut-apt/
#
# The .deb is the one gui/jammy/build.sh made; this only lays it out as a
# repository, signs it and pushes it. The site is the repository
# DeepRegular/smartcut-apt, served from its main branch, and holds one suite
# with the newest version only:
#   dists/stable/{Release,InRelease,Release.gpg}
#   dists/stable/main/binary-amd64/Packages{,.gz}
#   pool/main/s/smartcut/smartcut_<version>_amd64.deb
#   smartcut.gpg       the signing key, for /usr/share/keyrings
#   smartcut.sources   the deb822 source to put in /etc/apt/sources.list.d
# Each run replaces the site with one commit, as publish-flatpak.sh does, so
# it stays one version big. It is signed with the same key as the Flatpak
# repository; its folder comes from FLATPAK_GPG_HOME.
set -euo pipefail

DEB=$(readlink -f "${1:?usage: $0 smartcut_<version>_amd64.deb}")
GPG_HOME=${FLATPAK_GPG_HOME:?set FLATPAK_GPG_HOME to the folder holding the signing key}
KEY=E8A2ACD236581DDDF68AE7FFA0DCC65533C608DA
SITE=https://deepregular.github.io/smartcut-apt
REMOTE=https://github.com/DeepRegular/smartcut-apt.git
STATE=${APT_STATE:-$HOME/.cache/smartcut-apt}
PAGES=$STATE/pages
cd "$(dirname "$0")"

[ "$(dpkg-deb -f "$DEB" Package)" = smartcut ] || { echo "error: $DEB is not the smartcut package" >&2; exit 1; }
VERSION=$(dpkg-deb -f "$DEB" Version)
ARCH=$(dpkg-deb -f "$DEB" Architecture)
[ "$ARCH" = amd64 ] || { echo "error: $DEB is for $ARCH, the repository is amd64 only" >&2; exit 1; }
gpg --homedir "$GPG_HOME" --list-secret-keys "$KEY" >/dev/null || {
  echo "error: no signing key $KEY in $GPG_HOME" >&2; exit 1; }

rm -rf "$PAGES"
mkdir -p "$PAGES/pool/main/s/smartcut" "$PAGES/dists/stable/main/binary-amd64"
cp "$DEB" "$PAGES/pool/main/s/smartcut/"
cd "$PAGES"
# Filename: in Packages is relative to the site's root, so it is run there.
apt-ftparchive packages pool > dists/stable/main/binary-amd64/Packages
gzip -9nk dists/stable/main/binary-amd64/Packages
apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=SmartCut \
  -o APT::FTPArchive::Release::Label=SmartCut \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o APT::FTPArchive::Release::Architectures=amd64 \
  -o APT::FTPArchive::Release::Components=main \
  -o "APT::FTPArchive::Release::Description=SmartCut $VERSION" \
  release dists/stable > dists/stable/Release.new
mv dists/stable/Release.new dists/stable/Release
gpg --homedir "$GPG_HOME" --batch --yes --local-user "$KEY" --digest-algo SHA512 \
  --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --homedir "$GPG_HOME" --batch --yes --local-user "$KEY" --digest-algo SHA512 \
  --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
gpg --homedir "$GPG_HOME" --export "$KEY" > smartcut.gpg

cat > smartcut.sources <<EOF
Types: deb
URIs: $SITE
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/smartcut.gpg
EOF
touch .nojekyll
cat > index.html <<EOF
<!doctype html>
<meta charset="utf-8">
<title>SmartCut APT repository</title>
<h1>SmartCut APT repository</h1>
<p>SmartCut $VERSION, for Ubuntu 22.04 and later and Debian 13 and later (amd64).
Add the repository once, and <code>apt upgrade</code> takes later versions:</p>
<pre>sudo curl -fsSLo /usr/share/keyrings/smartcut.gpg $SITE/smartcut.gpg
sudo curl -fsSLo /etc/apt/sources.list.d/smartcut.sources $SITE/smartcut.sources
sudo apt update
sudo apt install smartcut</pre>
<p>Signing key fingerprint: <code>$KEY</code></p>
<p><a href="https://github.com/DeepRegular/SmartCut">SmartCut on GitHub</a></p>
EOF

git init -q -b main
git add -A
git -c user.name="$(git -C "$OLDPWD" config user.name)" \
    -c user.email="$(git -C "$OLDPWD" config user.email)" \
    commit -q -m "SmartCut $VERSION"
git push -q --force "$REMOTE" main
echo "published: $SITE (smartcut $VERSION)"
