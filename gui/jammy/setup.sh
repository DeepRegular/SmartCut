#!/usr/bin/env bash
# Make the Ubuntu 22.04 root the AppImage and the portable tar.gz are built in
# (see docs/technical/distribution.md), once:
#   ./setup.sh /path/to/root
#
# 22.04 because glibc cannot be bundled, so the system an AppImage is built on
# sets the oldest system it runs on, and the AppImage catalog asks for the
# oldest Ubuntu LTS still supported. 22.04 has no FFmpeg 7.1, so FFmpeg and the
# encoders SmartCut reaches through it are built here from source, at the
# versions Debian 13 ships -- the system the .deb is built on and links to.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(mkdir -p "$1" && cd "$1" && pwd)
export JAMMY=$ROOT
cd "$ROOT"

BASE=ubuntu-base-22.04.5-base-amd64.tar.gz
BASE_SHA=242cd8898b33ea806ef5f13b1076ed7c76f9f989d18384452f7166692438ff1a
if [ ! -e etc/os-release ]; then
  # A folder that is not a root yet has to be empty: the base is unpacked
  # over whatever is there, and a wrong argument ($HOME) had files replaced.
  if [ -n "$(ls -A)" ]; then
    echo "error: $ROOT is not empty and not an Ubuntu root; give an empty or new folder" >&2
    exit 1
  fi
  curl -fsSLo "../$BASE" "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/$BASE"
  echo "$BASE_SHA  ../$BASE" | sha256sum -c
  unshare --map-auto --map-root-user tar -xzf "../$BASE" -C . --numeric-owner
fi

# ------------------------------------------------------------- packages
"$HERE/enter.sh" bash -c '
set -e
echo "APT::Sandbox::User \"root\";" > /etc/apt/apt.conf.d/99sandbox
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
  build-essential pkg-config curl ca-certificates file git xz-utils bzip2 python3 rsync patchelf \
  nasm yasm cmake meson ninja-build clang libclang-dev dpkg-dev desktop-file-utils \
  libwebkit2gtk-4.1-dev libgtk-3-dev libssl-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev libjack-jackd2-dev libpulse-dev \
  libopus-dev libmp3lame-dev libvorbis-dev zlib1g-dev libbz2-dev liblzma-dev \
  gstreamer1.0-alsa gstreamer1.0-gl gstreamer1.0-gtk3 gstreamer1.0-libav gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-ugly gstreamer1.0-x \
  gstreamer1.0-pulseaudio libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
[ -x /root/.cargo/bin/cargo ] || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain 1.98.0
. /root/.cargo/env
cargo install cargo-c --locked -q
cargo install tauri-cli --version 2.11.4 --locked -q
'

# -------------------------------------------------------------- sources
mkdir -p build/src
cd build/src
get() { # URL FILE SHA256
  [ -s "$2" ] || curl -fsSLo "$2" "$1"
  echo "$3  $2" | sha256sum -c --quiet
}
get https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz ffmpeg-7.1.5.tar.xz \
    de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f
get https://bitbucket.org/multicoreware/x265_git/downloads/x265_4.1.tar.gz x265_4.1.tar.gz \
    a31699c6a89806b74b0151e5e6a7df65de4b49050482fe5ebf8a4379d7af8f29
get https://gitlab.com/AOMediaCodec/SVT-AV1/-/archive/v2.3.0/SVT-AV1-v2.3.0.tar.gz SVT-AV1-v2.3.0.tar.gz \
    ebb0b484ef4a0dc281e94342a9f73ad458496f5d3457eca7465bec943910c6c3
get https://code.videolan.org/videolan/dav1d/-/archive/1.5.1/dav1d-1.5.1.tar.gz dav1d-1.5.1.tar.gz \
    fa635e2bdb25147b1384007c83e15de44c589582bb3b9a53fc1579cb9d74b695
get https://github.com/webmproject/libvpx/archive/refs/tags/v1.15.0.tar.gz libvpx-1.15.0.tar.gz \
    e935eded7d81631a538bfae703fd1e293aad1c7fd3407ba00440c95105d2011e
get https://storage.googleapis.com/aom-releases/libaom-3.12.1.tar.gz libaom-3.12.1.tar.gz \
    9e9775180dec7dfd61a79e00bda3809d43891aee6b2e331ff7f26986207ea22e
get https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/n12.2.72.0.tar.gz nv-codec-headers-12.2.72.0.tar.gz \
    dbeaec433d93b850714760282f1d0992b1254fc3b5a6cb7d76fc1340a1e47563
get https://github.com/intel/libvpl/archive/refs/tags/v2.14.0.tar.gz libvpl-2.14.0.tar.gz \
    7c6bff1c1708d910032c2e6c44998ffff3f5fdbf06b00972bc48bf2dd9e5ac06
get https://github.com/xiph/rav1e/archive/refs/tags/v0.7.1.tar.gz rav1e-0.7.1.tar.gz \
    da7ae0df2b608e539de5d443c096e109442cdfa6c5e9b4014361211cf61d030c
# Debian's libx264-164 is this commit of the stable branch.
[ -d x264 ] || git clone -q https://code.videolan.org/videolan/x264.git x264
git -C x264 checkout -q 31e19f92f00c7003fa115047ce50978bc98c3a0d
cd ../..

# ---------------------------------------------------------------- build
cp "$HERE/deps.sh" "$HERE/ffmpeg.sh" build/
"$HERE/enter.sh" bash -c '. /root/.cargo/env && bash /build/deps.sh && bash /build/ffmpeg.sh'
echo
echo "ready: JAMMY=$ROOT $HERE/build.sh"
