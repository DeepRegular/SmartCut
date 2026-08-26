#!/usr/bin/env bash
# Cross-build the Windows app on the Linux dev VM (see DEVENV.md).
#   ./build-windows.sh   -> NSIS installer + portable zip
#
# Nothing in the Rust code is Linux-specific — it all goes through libav — so
# the only Windows-shaped pieces are the FFmpeg import libraries the linker
# needs and the DLLs the app opens at run time. One prebuilt package holds both.
set -euo pipefail
cd "$(dirname "$0")"

TARGET=x86_64-pc-windows-msvc
# Same 7.1 branch as the system FFmpeg the Linux build links against, so both
# platforms run the same decoder. Upstream stopped shipping 7.1 for Windows once
# 8.1 landed; gyan keeps the older releases on GitHub.
FFMPEG_VER=7.1.1
DEPS="$HOME/win-deps"
FF="$DEPS/ffmpeg-$FFMPEG_VER-full_build-shared"
# avfilter loads postproc, so it ships too even though no import table names it.
DLLS=(avcodec-61 avdevice-61 avfilter-10 avformat-61 avutil-59 postproc-58 swresample-5 swscale-8)

if [ ! -d "$FF" ]; then
  echo "fetching FFmpeg $FFMPEG_VER (Windows shared build)"
  mkdir -p "$DEPS"
  curl -fL -o "$FF.7z" \
    "https://github.com/GyanD/codexffmpeg/releases/download/$FFMPEG_VER/ffmpeg-$FFMPEG_VER-full_build-shared.7z"
  7z x -y -o"$DEPS" "$FF.7z" >/dev/null
fi

# tauri.windows.conf.json lists these as bundle resources, which on Windows
# means "install next to the exe" — where the loader looks for them.
mkdir -p src-tauri/windows-deps
for d in "${DLLS[@]}"; do cp -u "$FF/bin/$d.dll" src-tauri/windows-deps/; done

export FFMPEG_DIR="$FF"       # ffmpeg-sys-next links $FFMPEG_DIR/lib/*.lib
export XWIN_ACCEPT_LICENSE=1  # cargo-xwin fetches the MSVC headers on first run

cd src-tauri
cargo tauri build --runner cargo-xwin --target "$TARGET" --bundles nsis

# The same payload as a folder, for anyone who would rather not install.
OUT="target/$TARGET/release"
STAGE="$OUT/bundle/portable"
rm -rf "$STAGE" && mkdir -p "$STAGE/smartcut"
cp "$OUT/smartcut.exe" "$STAGE/smartcut/"
for d in "${DLLS[@]}"; do cp "$FF/bin/$d.dll" "$STAGE/smartcut/"; done
(cd "$STAGE" && 7z a -tzip -mx=9 smartcut-portable-x64.zip smartcut >/dev/null)

echo
echo "installer: $PWD/$OUT/bundle/nsis/"
echo "portable:  $PWD/$STAGE/smartcut-portable-x64.zip"
