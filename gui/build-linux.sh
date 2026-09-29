#!/usr/bin/env bash
# Build the Linux app (see docs/technical/distribution.md).
#   ./build-linux.sh portable   -> AppImage + portable tar.gz
#   ./build-linux.sh deb        -> .deb
#   ./build-linux.sh            -> all three
# All three are built in the Ubuntu 22.04 root (gui/jammy/build.sh). What a
# Linux build carries decides nothing about glibc: that comes from the system
# it was built on, and the AppImage catalog requires the oldest Ubuntu LTS
# still supported. No FFmpeg 7.1 comes with 22.04, so one is built there from
# source into $FF (gui/jammy/ffmpeg.sh), with the encoders it reaches.
#
# The tar.gz is self-contained: it carries the AppDir that linuxdeploy fills
# for the AppImage, so FFmpeg and WebKitGTK travel with it and it runs
# wherever the AppImage runs — glibc 2.35+ — without FUSE and without being
# installed. The .deb carries that FFmpeg and those encoders, in
# /usr/lib/smartcut where nothing else looks, and lets apt resolve everything
# else -- WebKitGTK, GTK, ALSA, the audio codecs Ubuntu 22.04 already has.
# It used to link the system's own FFmpeg 7.1 instead, which only Debian 13 and
# Ubuntu 25.04 have: an Ubuntu 22.04 or 24.04 user could not install it.
#
# The program is called SmartCut; the two things you type are not. Both
# packagings name the GUI `smartcut` and the command-line cutter
# `smartcut-cli`, which is also what the Debian package is called -- a command
# and a package name are lowercase, whatever the program's name is.
set -euo pipefail
cd "$(dirname "$0")"

case "${1:-all}" in
  portable) BUNDLES=appimage ;;
  deb)      BUNDLES=deb ;;
  all)      BUNDLES=deb,appimage ;;
  *)        echo "usage: $0 [portable|deb]" >&2; exit 2 ;;
esac
want() { [[ ,$BUNDLES, == *,$1,* ]]; }

conf() { sed -n "s/^  \"$1\": \"\(.*\)\",\$/\1/p" src-tauri/tauri.conf.json; }
VERSION=$(conf version)
# Tauri names the bundles it makes after this, so the paths below have to read
# it rather than spell it.
PRODUCT=$(conf productName)
MAINTAINER="mevius <supernova@supersolenoid.com>"
HOMEPAGE="https://github.com/DeepRegular/SmartCut"
NAME=$PRODUCT-$VERSION-linux-x86_64

OUT=src-tauri/target/release
# Where the FFmpeg the .deb carries was installed (gui/jammy/ffmpeg.sh).
FF=${FF:-/opt/ff}
STAGE=$OUT/bundle/linux
APPDIR=$OUT/bundle/appimage/$PRODUCT.AppDir
CLI_BIN=../rust/target/release/smartcut
# Not $OUT/smartcut. Tauri stamps the bundle type into the binary as it packs
# each one ("UNKNOWN" -> "DEB" / "APPIMAGE"), so that copy only ever carries
# the stamp of whichever bundle was built last. Take each payload from its own
# bundle: the deb's binary from Tauri's deb, the tarball's from the AppDir.
GUI_BIN=$OUT/bundle/deb/${PRODUCT}_${VERSION}_amd64/data/usr/bin/smartcut

# ------------------------------------------------------------------ build
# Tauri saves the tools it downloads with mode 0770, and one of them -- the
# AppRun it hands linuxdeploy -- goes into the AppImage as AppRun.wrapped,
# mode and all. squashfs stores it owned by root, so a user who mounts the
# image without the AppImage runtime (firejail --appimage, which is how the
# AppImage catalog tests it) cannot execute the app. The runtime's own FUSE
# mount presents every file as the caller's and hid this. Tauri downloads a
# tool only when it is missing, so fixing the cached copy is enough; a fresh
# cache is caught by the check after the build.
TAURI_CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/tauri
chmod -f go+rx "$TAURI_CACHE"/AppRun-* || true

cargo build --release --manifest-path ../rust/Cargo.toml -p smartcut-cli
# NO_STRIP is explained in docs/technical/distribution.md. The AppImage run is also what
# produces the AppDir, which is the payload the tar.gz wants; the deb run is
# here for its correctly stamped binary, not for the package it makes.
(cd src-tauri && NO_STRIP=1 cargo tauri build --bundles $BUNDLES)

rm -rf "$STAGE"
mkdir -p "$STAGE"

if want appimage; then
# Anything its owner may run, everyone must be able to run: see above.
LOCKED=$(find "$APPDIR" -type f -perm -u+x ! -perm -o+rx)
if [ -n "$LOCKED" ]; then
  echo "error: only the owner can run these (Tauri's download cache was fresh;" >&2
  echo "       it is fixed now, so run this script again):" >&2
  echo "$LOCKED" >&2
  chmod -f go+rx "$TAURI_CACHE"/AppRun-* || true
  exit 1
fi

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
展開した場所からそのまま動く（必要なのは glibc 2.35 以上 = Ubuntu 22.04 /
Debian 12 / Fedora 36 以降）。

app/ の中身は AppImage 版と同じ一式で、ここでは FUSE を要らなくするために
展開した形で置いてある。ディレクトリごと移動するのは構わないが、2 つの起動
スクリプトは app/ と同じ階層に置いたままにすること。

GPL-3.0-or-later。ソースと本体は $HOMEPAGE
EOF

# The same library, three times, under three names. linuxdeploy follows what
# `ldd` says and copies each name it is given, and a library's -dev package
# carries `libfoo.so` and `libfoo.so.0` as symlinks to `libfoo.so.0.1.2` --
# dereferenced on the way in, so librsvg arrives as 6.2 MB three times over.
# The loader opens exactly one of them, by SONAME; the other two are names.
#
# Put the names back as symlinks. It is 19.5 MB of the 527 and gzip cannot
# see the repeat for itself: its window is 32 KB and the copies are megabytes
# apart. The AppImage never had this -- squashfs notices a duplicate block --
# which is half of why it was the smaller of the two.
#
# Only under `usr/lib` and only above 64 KB, so that this is about libraries:
# the copyright files under `usr/share/doc` are duplicates too, and a package's
# licence is the last file to replace with a pointer to another package's.
find "$TREE/app/usr/lib" -type f -size +64k -print0 | xargs -0 md5sum | sort | awk '
  { h = $1; sub(/^[^ ]*  /, ""); f = $0
    if (!(h in keep)) { keep[h] = f; next }
    # The longest name is the real file, which is how the system has it.
    if (length(f) > length(keep[h])) { dups[h] = dups[h] "\n" keep[h]; keep[h] = f }
    else { dups[h] = dups[h] "\n" f } }
  END { for (h in dups) { n = split(dups[h], a, "\n")
          for (i = 1; i <= n; i++) if (a[i] != "") print keep[h] "\t" a[i] } }' |
while IFS=$'\t' read -r real dup; do
  ln -sf "$(basename "$real")" "$dup"
done

# -9 rather than the default 6: a minute of processor for another 0.9 MB,
# paid once here and saved by everyone who downloads it.
tar -C "$STAGE" --owner=0 --group=0 -I 'gzip -9' -cf "$STAGE/$NAME.tar.gz" "$NAME"
fi

want deb || { echo; echo "appimage: $PWD/$OUT/bundle/appimage/${PRODUCT}_${VERSION}_amd64.AppImage"
               echo "tarball:  $PWD/$STAGE/$NAME.tar.gz"; exit 0; }

# -------------------------------------------------------------------- deb
ROOT=$STAGE/deb
PRIVATE=usr/lib/smartcut
install -Dm755 "$GUI_BIN" "$ROOT/usr/bin/smartcut"
install -Dm755 "$CLI_BIN" "$ROOT/usr/bin/smartcut-cli"

# FFmpeg and its encoders: every library under $FF that the two binaries reach,
# however indirectly, and nothing the system has. Each goes in under the name
# the loader asks for (its SONAME), finds the others beside it, and the two
# binaries look there first.
mkdir -p "$ROOT/$PRIVATE"
LD_LIBRARY_PATH=$FF/lib ldd "$ROOT/usr/bin/smartcut" "$ROOT/usr/bin/smartcut-cli" |
  awk -v ff="$FF/lib/" 'index($3, ff) == 1 { print $1 "\t" $3 }' | sort -u |
while IFS=$'\t' read -r soname path; do
  install -m644 "$(readlink -f "$path")" "$ROOT/$PRIVATE/$soname"
  patchelf --set-rpath '$ORIGIN' "$ROOT/$PRIVATE/$soname"
done
[ -e "$ROOT/$PRIVATE/libavcodec.so.61" ] || { echo "error: no FFmpeg under $FF/lib" >&2; exit 1; }
patchelf --set-rpath "/$PRIVATE" "$ROOT/usr/bin/smartcut" "$ROOT/usr/bin/smartcut-cli"
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

# What the two binaries and the libraries beside them pull in from the system,
# which is more than Tauri's own deb says (it lists webkit2gtk and gtk and
# stops there). The versions are the ones Ubuntu 22.04 has, so any later
# system satisfies them; the libraries this package carries have no package
# to name, which is what --ignore-missing-info is for, and the check below
# makes sure nothing else slipped through that way.
# dpkg-shlibdeps wants to be standing in a source package, so give it one.
mkdir -p "$ROOT/debian" "$ROOT/DEBIAN"
printf 'Source: smartcut\n\nPackage: smartcut\nArchitecture: amd64\n' > "$ROOT/debian/control"
DEPENDS=$(cd "$ROOT" && dpkg-shlibdeps --ignore-missing-info -l"$PRIVATE" -O \
          usr/bin/smartcut usr/bin/smartcut-cli "$PRIVATE"/*.so.* \
          | sed 's/^shlibs:Depends=//')
rm -rf "$ROOT/debian"
# Every library the payload asks for by name is either in it or from a package
# it names. Only what these files ask for themselves: what a system library
# pulls in behind them is that library's package's to depend on.
MISSING=$(for f in "$ROOT"/usr/bin/* "$ROOT/$PRIVATE"/*.so.*; do
    LD_LIBRARY_PATH="$ROOT/$PRIVATE" ldd "$f" > "$ROOT/ldd.txt"
    readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' |
    while read -r soname; do
      [ -e "$ROOT/$PRIVATE/$soname" ] && continue
      # The loader itself: libc6's, and not a library ldd resolves.
      [[ $soname == ld-linux* ]] && continue
      lib=$(awk -v n="$soname" '$1 == n { print $3 }' "$ROOT/ldd.txt")
      pkg=$( { dpkg -S "$lib" 2>/dev/null || dpkg -S "$(readlink -f "$lib")" 2>/dev/null; } |
             head -1 | cut -d: -f1) || true
      # The patterns open with `(`: bash 5.1, which 22.04 has, cannot parse a
      # bare `)` inside $( ).
      case ", $DEPENDS," in (*", $pkg "*|*", $pkg,"*|*"| $pkg "*|*"| $pkg,"*) ;; (*) echo "$soname ($pkg)" ;; esac
    done
  done | sort -u)
rm -f "$ROOT/ldd.txt"
if [ -n "$MISSING" ]; then
  echo "error: needed but neither carried nor depended on:" >&2
  echo "$MISSING" >&2
  exit 1
fi

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
 GUI は smartcut、コマンドライン版は smartcut-cli。FFmpeg 7.1 とエンコーダーは
 /usr/lib/smartcut に同梱しているので、Ubuntu 22.04 以降で動く。
EOF

dpkg-deb --root-owner-group --build "$ROOT" "$STAGE/smartcut_${VERSION}_amd64.deb" >/dev/null
rm -rf "$ROOT"

echo
want appimage && echo "tarball: $PWD/$STAGE/$NAME.tar.gz"
echo "deb:     $PWD/$STAGE/smartcut_${VERSION}_amd64.deb"
