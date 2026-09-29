#!/usr/bin/env bash
# FFmpeg 7.1.5 for the Ubuntu 22.04 build of SmartCut.
#
# Every native codec, container, protocol and bitstream filter stays in --
# SmartCut picks decoders from the input and muxers from the output's
# extension, so a trimmed list would turn into files that no longer open.
# The external libraries are only those SmartCut names or needs:
#   encoders  libx264 libx265 libsvtav1 librav1e libaom libvpx
#             libmp3lame libopus libvorbis (audio re-encodes keep the codec)
#   decoder   libdav1d (the native AV1 decoder only works through hwaccel)
#   hardware  nvenc (headers only, loads the driver at run time), QSV via libvpl
# Filters and devices are left as FFmpeg builds them by default: ffmpeg-next's
# default features link libavfilter and libavdevice although nothing uses them.
set -euo pipefail
P=/opt/ff
J=${J:-8}
export PKG_CONFIG_PATH=$P/lib/pkgconfig
cd /build/src/work
# `;` rather than `&&`, so that set -e stops at a failed make.
rm -rf ffmpeg-7.1.5; tar xf ../ffmpeg-7.1.5.tar.xz; cd ffmpeg-7.1.5
./configure --prefix=$P --libdir=$P/lib \
  --enable-gpl --enable-shared --disable-static --disable-doc \
  --extra-cflags="-I$P/include" --extra-ldflags="-L$P/lib" \
  --enable-libx264 --enable-libx265 --enable-libsvtav1 --enable-librav1e \
  --enable-libaom --enable-libvpx --enable-libdav1d \
  --enable-libmp3lame --enable-libopus --enable-libvorbis \
  --enable-nvenc --enable-ffnvcodec --enable-libvpl \
  --disable-vaapi --disable-vdpau --disable-xlib --disable-libxcb --disable-sdl2 \
  --disable-libdrm --disable-opencl --disable-vulkan --disable-cuda-llvm
make -j$J; make install
echo; echo "ffmpeg done"
$P/bin/ffmpeg -hide_banner -version | head -1
for l in $P/lib/libav*.so.?? $P/lib/libsw*.so.?; do echo "== $(basename $l)"; readelf -d $l | awk -F'[][]' '/NEEDED/{printf "%s ", $2}'; echo; done
