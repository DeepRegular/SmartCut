#!/usr/bin/env bash
# Build the AppImage and the portable tar.gz in the Ubuntu 22.04 root that
# setup.sh made, from this working tree:
#   JAMMY=/path/to/root ./build.sh
# The tree is copied in rather than mounted, so the two target/ directories
# -- built against different glibcs -- never meet.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=${JAMMY:?set JAMMY to the root setup.sh made}
rsync -a --delete --exclude target/ --exclude node_modules/ --exclude .git --exclude dist/ \
  --exclude windows-deps/ --exclude __pycache__ --exclude 'gen/schemas/' \
  "$HERE/../../" "$ROOT/build/smartcut/"
# linuxdeploy is itself an AppImage, and there is no FUSE in the root.
"$HERE/enter.sh" bash -c '. /root/.cargo/env && export APPIMAGE_EXTRACT_AND_RUN=1 &&
  cd /build/smartcut/gui && ./build-linux.sh portable'
echo "(inside $ROOT)"
