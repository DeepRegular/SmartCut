#!/usr/bin/env bash
# Run a command inside the Ubuntu 22.04 root that builds the AppImage:
#   JAMMY=/path/to/root ./enter.sh bash -lc '...'
# A user namespace maps us to root (and the subuid range to everyone else), so
# apt and dpkg work unmodified and nothing is installed on the host. Needs
# util-linux 2.38+ (--map-auto) and an /etc/subuid entry for the caller.
set -euo pipefail
ROOT=${JAMMY:?set JAMMY to the root setup.sh made}
# Handed in as an argument, not pasted into the script: a space in it split it.
exec unshare --map-auto --map-root-user --mount --pid --fork --kill-child bash -c '
  set -e
  R=$1; shift
  mount --rbind /dev "$R/dev"
  mount -t proc proc "$R/proc"
  mount --rbind /sys "$R/sys"
  cp /etc/resolv.conf "$R/etc/resolv.conf"
  exec /usr/sbin/chroot "$R" /usr/bin/env -i HOME=/root TERM=dumb LANG=C.UTF-8 \
    PATH=/root/.cargo/bin:/opt/ff/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    PKG_CONFIG_PATH=/opt/ff/lib/pkgconfig LD_LIBRARY_PATH=/opt/ff/lib "$@"
' bash "$ROOT" "$@"
