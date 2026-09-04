#!/usr/bin/env bash
# Build the Linux app on the dev VM (see DEVENV.md).
#   ./build-linux.sh   -> portable tar.gz + .deb
#
# Two packagings of one build, answering different questions.
#
# The tar.gz is self-contained: it carries the AppDir that linuxdeploy fills
# for the AppImage, so FFmpeg and WebKitGTK travel with it and it runs
# wherever the AppImage runs — glibc 2.39+ — without FUSE and without being
# installed. The .deb carries the two binaries alone and lets apt resolve the
# libraries, which is what a Debian package is supposed to do; that ties it to
# FFmpeg 7.1 (Debian 13 / Ubuntu 25.04 or newer).
#
# The program is called SmartCut; the two things you type are not. Both
# packagings name the GUI `smartcut` and the command-line cutter
# `smartcut-cli`, which is also what the Debian package is called -- a command
# and a package name are lowercase, whatever the program's name is.
set -euo pipefail
cd "$(dirname "$0")"

conf() { sed -n "s/^  \"$1\": \"\(.*\)\",\$/\1/p" src-tauri/tauri.conf.json; }
VERSION=$(conf version)
# Tauri names the bundles it makes after this, so the paths below have to read
# it rather than spell it.
PRODUCT=$(conf productName)
MAINTAINER="mevius <supernova@supersolenoid.com>"
HOMEPAGE="https://github.com/DeepRegular/SmartCut"
NAME=$PRODUCT-$VERSION-linux-x86_64

OUT=src-tauri/target/release
STAGE=$OUT/bundle/linux
APPDIR=$OUT/bundle/appimage/$PRODUCT.AppDir
CLI_BIN=../rust/target/release/smartcut
# Not $OUT/smartcut. Tauri stamps the bundle type into the binary as it packs
# each one ("UNKNOWN" -> "DEB" / "APPIMAGE"), so that copy only ever carries
# the stamp of whichever bundle was built last. Take each payload from its own
# bundle: the deb's binary from Tauri's deb, the tarball's from the AppDir.
GUI_BIN=$OUT/bundle/deb/${PRODUCT}_${VERSION}_amd64/data/usr/bin/smartcut

# ------------------------------------------------------------------ build
cargo build --release --manifest-path ../rust/Cargo.toml -p smartcut-cli
# NO_STRIP is explained in docs/developers/distribution.md. The AppImage run is also what
# produces the AppDir, which is the payload the tar.gz wants; the deb run is
# here for its correctly stamped binary, not for the package it makes.
(cd src-tauri && NO_STRIP=1 cargo tauri build --bundles deb,appimage)

rm -rf "$STAGE"

# ----------------------------------------------------------------- tar.gz
TREE=$STAGE/$NAME
mkdir -p "$TREE"
cp -a "$APPDIR" "$TREE/app"
cp "$CLI_BIN" "$TREE/app/usr/bin/smartcut-cli"

cat > "$TREE/smartcut" <<'EOF'
#!/bin/sh
# The GUI. AppRun is linuxdeploy's: it points GTK, GDK and the loader at the
# bundled copies before exec'ing the app.
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/app/AppRun" "$@"
EOF

cat > "$TREE/smartcut-cli" <<'EOF'
#!/bin/sh
# The command-line cutter. It reaches nothing but libav*, so the library path
# is the whole of the setup it needs.
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$HERE/app/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/app/usr/bin/smartcut-cli" "$@"
EOF

chmod +x "$TREE/smartcut" "$TREE/smartcut-cli"
cp ../LICENSE "$TREE/LICENSE"

cat > "$TREE/README.txt" <<EOF
SmartCut $VERSION — portable Linux build (x86_64)

    ./smartcut              放送録画を開いてカット編集する GUI
    ./smartcut [FILE]       ファイルを開いた状態で起動する
    ./smartcut-cli [FILE] --cut 5-10 -o out.ts    コマンドライン版

FFmpeg も WebKitGTK も app/ の中に入っているので、入れるものは何もない。
展開した場所からそのまま動く（必要なのは glibc 2.39 以上 = Ubuntu 24.04 /
Debian 13 / Fedora 40 以降）。

app/ の中身は AppImage 版と同じ一式で、ここでは FUSE を要らなくするために
展開した形で置いてある。ディレクトリごと移動するのは構わないが、2 つの起動
スクリプトは app/ と同じ階層に置いたままにすること。

GPL-3.0-or-later。ソースと本体は $HOMEPAGE
EOF

tar -C "$STAGE" --owner=0 --group=0 -czf "$STAGE/$NAME.tar.gz" "$NAME"

# -------------------------------------------------------------------- deb
ROOT=$STAGE/deb
install -Dm755 "$GUI_BIN" "$ROOT/usr/bin/smartcut"
install -Dm755 "$CLI_BIN" "$ROOT/usr/bin/smartcut-cli"
install -Dm644 src-tauri/icons/32x32.png      "$ROOT/usr/share/icons/hicolor/32x32/apps/smartcut.png"
install -Dm644 src-tauri/icons/128x128.png    "$ROOT/usr/share/icons/hicolor/128x128/apps/smartcut.png"
install -Dm644 src-tauri/icons/128x128@2x.png "$ROOT/usr/share/icons/hicolor/256x256/apps/smartcut.png"

install -Dm644 /dev/stdin "$ROOT/usr/share/applications/smartcut.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=SmartCut
Comment=スマートレンダリング対応の動画カットツール
Comment[en]=Cut a broadcast recording without re-encoding it
Exec=smartcut %f
Icon=smartcut
Terminal=false
Categories=AudioVideo;Video;
MimeType=video/mp2t;video/mp4;
StartupWMClass=smartcut
EOF

install -Dm644 /dev/stdin "$ROOT/usr/share/doc/smartcut/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: SmartCut
Source: $HOMEPAGE

Files: *
Copyright: 2026 mevius
License: GPL-3.0-or-later
 This program is free software: you can redistribute it and/or modify it
 under the terms of the GNU General Public License as published by the Free
 Software Foundation, either version 3 of the License, or (at your option)
 any later version.
 .
 This program is distributed in the hope that it will be useful, but WITHOUT
 ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 more details.
 .
 On Debian systems the full text of the GNU General Public License version 3
 can be found in /usr/share/common-licenses/GPL-3.
EOF

printf 'smartcut (%s) unstable; urgency=medium\n\n  * Release %s. See %s/releases\n\n -- %s  %s\n' \
  "$VERSION" "$VERSION" "$HOMEPAGE" "$MAINTAINER" "$(date -R)" \
  | gzip -9n > "$ROOT/usr/share/doc/smartcut/changelog.Debian.gz"
chmod 644 "$ROOT/usr/share/doc/smartcut/changelog.Debian.gz"

# What the two binaries actually pull in — libav* included, which is the half
# Tauri's own deb leaves out (it lists webkit2gtk and gtk and stops there).
# dpkg-shlibdeps wants to be standing in a source package, so give it one.
mkdir -p "$ROOT/debian"
printf 'Source: smartcut\n\nPackage: smartcut\nArchitecture: amd64\n' > "$ROOT/debian/control"
DEPENDS=$(cd "$ROOT" && dpkg-shlibdeps -O usr/bin/smartcut usr/bin/smartcut-cli \
          | sed 's/^shlibs:Depends=//')
rm -rf "$ROOT/debian"

mkdir -p "$ROOT/DEBIAN"
(cd "$ROOT" && find usr -type f -exec md5sum {} + | LC_ALL=C sort -k2 > DEBIAN/md5sums)
chmod 644 "$ROOT/DEBIAN/md5sums"

install -Dm644 /dev/stdin "$ROOT/DEBIAN/control" <<EOF
Package: smartcut
Version: $VERSION
Section: video
Priority: optional
Architecture: amd64
Maintainer: $MAINTAINER
Installed-Size: $(du -ks --exclude=DEBIAN "$ROOT" | cut -f1)
Depends: $DEPENDS
Homepage: $HOMEPAGE
Description: スマートレンダリング対応の動画カットツール
 カット点にかかる部分 GOP だけを再エンコードし、残りはビット単位でそのまま
 コピーする動画カットツール。MPEG-2 TS / MP4 に対応し、CM 境界の検出とシーン
 検出を備える。
 .
 GUI は smartcut、コマンドライン版は smartcut-cli。FFmpeg は同梱せず、
 システムの libav* に動的リンクする。
EOF

dpkg-deb --root-owner-group --build "$ROOT" "$STAGE/smartcut_${VERSION}_amd64.deb" >/dev/null
rm -rf "$ROOT"

echo
echo "tarball: $PWD/$STAGE/$NAME.tar.gz"
echo "deb:     $PWD/$STAGE/smartcut_${VERSION}_amd64.deb"
