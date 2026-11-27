#!/bin/sh
# Installs (or upgrades) Foxhole and turns protection on. Run it as root, either from a source tree
#   cargo build --release && sudo packaging/install.sh
# or from an unpacked release (the curl installer at the top of the repository does that).
# PREFIX defaults to /usr/local. Remove it again with `foxhole uninstall`.
set -eu

PREFIX=${PREFIX:-/usr/local}
BINDIR=$PREFIX/bin
LIBDIR=$PREFIX/lib/foxhole
HERE=$(cd "$(dirname "$0")" && pwd)
# A release has the binary next to this script; a source tree has it under target/.
if [ -e "$HERE/foxhole" ]; then BIN=$HERE/foxhole; else BIN=$HERE/../target/release/foxhole; fi

[ "$(id -u)" = 0 ] || { echo "Run this as root (sudo $0)." >&2; exit 1; }
[ -x "$BIN" ] || { echo "$BIN not found; run 'cargo build --release' first." >&2; exit 1; }
systemctl is-active --quiet systemd-resolved || {
    echo "Foxhole needs systemd-resolved, which isn't running here." >&2; exit 1; }

install -D -m 0755 "$BIN" "$BINDIR/foxhole"
install -D -m 0755 "$HERE/uninstall.sh" "$LIBDIR/uninstall.sh"
install -D -m 0644 "$HERE/foxhole.sysusers" /usr/lib/sysusers.d/foxhole.conf
systemd-sysusers /usr/lib/sysusers.d/foxhole.conf
sed "s|@BINDIR@|$BINDIR|g" "$HERE/foxhole.service" > /etc/systemd/system/foxhole.service
chmod 0644 /etc/systemd/system/foxhole.service

# Keep an existing config.toml; otherwise write out the defaults to edit.
install -d -m 0755 /etc/foxhole
[ -e /etc/foxhole/config.toml ] || "$BINDIR/foxhole" default-config > /etc/foxhole/config.toml

systemctl daemon-reload
echo "foxhole: installed to $PREFIX; starting the service"
"$BINDIR/foxhole" on
