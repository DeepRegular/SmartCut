#!/usr/bin/env bash
# Build the encoders, and the AV1 decoder, that SmartCut reaches through
# FFmpeg -- at the versions Debian 13 ships, the system the .deb links to --
# into /opt/ff. Run inside the root (setup.sh does).
set -euo pipefail
P=/opt/ff
J=${J:-8}
export PKG_CONFIG_PATH=$P/lib/pkgconfig
cd /build/src
mkdir -p work && cd work

step() { echo; echo "===== $*"; }

step x264
rm -rf x264 && cp -a ../x264 x264 && cd x264
./configure --prefix=$P --enable-shared --disable-cli --enable-pic
make -j$J && make install && cd ..

step x265 4.1
rm -rf x265_4.1 && tar xf ../x265_4.1.tar.gz && mkdir -p x265_4.1/build/b && cd x265_4.1/build/b
cmake ../../source -G Ninja -DCMAKE_INSTALL_PREFIX=$P -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_SHARED=ON -DENABLE_CLI=OFF -DCMAKE_INSTALL_LIBDIR=lib
ninja -j$J && ninja install && cd ../../..

step SVT-AV1 2.3.0
rm -rf SVT-AV1-v2.3.0 && tar xf ../SVT-AV1-v2.3.0.tar.gz && mkdir -p SVT-AV1-v2.3.0/b && cd SVT-AV1-v2.3.0/b
cmake .. -G Ninja -DCMAKE_INSTALL_PREFIX=$P -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
  -DBUILD_APPS=OFF -DBUILD_DEC=OFF -DBUILD_TESTING=OFF -DCMAKE_INSTALL_LIBDIR=lib
ninja -j$J && ninja install && cd ../..

step dav1d 1.5.1
rm -rf dav1d-1.5.1 && tar xf ../dav1d-1.5.1.tar.gz && cd dav1d-1.5.1
meson setup b --prefix=$P --libdir=lib --buildtype=release -Denable_tools=false -Denable_tests=false
ninja -C b -j$J && ninja -C b install && cd ..

step libvpx 1.15.0
rm -rf libvpx-1.15.0 && tar xf ../libvpx-1.15.0.tar.gz && cd libvpx-1.15.0
./configure --prefix=$P --enable-shared --disable-static --enable-pic --enable-vp9-highbitdepth \
  --disable-examples --disable-tools --disable-docs --disable-unit-tests
make -j$J && make install && cd ..

step aom 3.12.1
rm -rf libaom-3.12.1 && tar xf ../libaom-3.12.1.tar.gz && mkdir -p libaom-3.12.1/b && cd libaom-3.12.1/b
cmake .. -G Ninja -DCMAKE_INSTALL_PREFIX=$P -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
  -DENABLE_DOCS=OFF -DENABLE_EXAMPLES=OFF -DENABLE_TESTS=OFF -DENABLE_TOOLS=OFF -DCMAKE_INSTALL_LIBDIR=lib
ninja -j$J && ninja install && cd ../..

step nv-codec-headers 12.2.72.0
rm -rf nv-codec-headers-n12.2.72.0 && tar xf ../nv-codec-headers-12.2.72.0.tar.gz && cd nv-codec-headers-n12.2.72.0
make PREFIX=$P install && cd ..

step libvpl 2.14.0
rm -rf libvpl-2.14.0 && tar xf ../libvpl-2.14.0.tar.gz && mkdir -p libvpl-2.14.0/b && cd libvpl-2.14.0/b
cmake .. -G Ninja -DCMAKE_INSTALL_PREFIX=$P -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
  -DBUILD_TESTS=OFF -DBUILD_EXAMPLES=OFF -DINSTALL_EXAMPLES=OFF -DBUILD_TOOLS=OFF -DCMAKE_INSTALL_LIBDIR=lib
ninja -j$J && ninja install && cd ../..

step rav1e 0.7.1
rm -rf rav1e-0.7.1 && tar xf ../rav1e-0.7.1.tar.gz && cd rav1e-0.7.1
cargo cinstall --release --prefix=$P --libdir=$P/lib --library-type=cdylib -j$J && cd ..

echo; echo "deps done"; ls $P/lib/*.so.* | xargs -n1 basename
