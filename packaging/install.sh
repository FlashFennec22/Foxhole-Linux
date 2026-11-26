#!/bin/sh
# Installs (or upgrades) Foxhole from a release build and turns protection on:
#   cargo build --release && sudo packaging/install.sh
# PREFIX defaults to /usr/local. Remove it again with packaging/uninstall.sh.
set -eu

PREFIX=${PREFIX:-/usr/local}
BINDIR=$PREFIX/bin
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=$HERE/../target/release/foxhole

[ "$(id -u)" = 0 ] || { echo "Run this as root (sudo $0)." >&2; exit 1; }
[ -x "$BIN" ] || { echo "$BIN not found; run 'cargo build --release' first." >&2; exit 1; }
systemctl is-active --quiet systemd-resolved || {
    echo "Foxhole needs systemd-resolved, which isn't running here." >&2; exit 1; }

install -D -m 0755 "$BIN" "$BINDIR/foxhole"
install -D -m 0644 "$HERE/foxhole.sysusers" /usr/lib/sysusers.d/foxhole.conf
systemd-sysusers /usr/lib/sysusers.d/foxhole.conf
sed "s|@BINDIR@|$BINDIR|g" "$HERE/foxhole.service" > /etc/systemd/system/foxhole.service
chmod 0644 /etc/systemd/system/foxhole.service

# Keep an existing config.toml; otherwise write out the defaults to edit.
install -d -m 0755 /etc/foxhole
[ -e /etc/foxhole/config.toml ] || "$BINDIR/foxhole" default-config > /etc/foxhole/config.toml

systemctl daemon-reload
"$BINDIR/foxhole" on
