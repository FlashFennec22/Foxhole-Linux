#!/bin/sh
# Turns protection off and removes Foxhole. Keeps /etc/foxhole/config.toml unless --purge is given.
# Installed as $PREFIX/lib/foxhole/uninstall.sh; `foxhole uninstall [--purge]` runs it.
set -eu

PREFIX=${PREFIX:-/usr/local}
[ "$(id -u)" = 0 ] || { echo "Run this as root (sudo $0)." >&2; exit 1; }

# Stopping the service gives DNS back to the network; dns-restore covers a stray `foxhole run`.
systemctl disable --now foxhole.service 2>/dev/null || true
if [ -x "$PREFIX/bin/foxhole" ]; then "$PREFIX/bin/foxhole" dns-restore || true; fi

rm -f /etc/systemd/system/foxhole.service "$PREFIX/bin/foxhole" /usr/lib/sysusers.d/foxhole.conf
rm -rf "$PREFIX/lib/foxhole" /var/lib/foxhole /var/lib/private/foxhole
systemctl daemon-reload
if [ "${1:-}" = --purge ]; then
    rm -rf /etc/foxhole
    userdel foxhole 2>/dev/null || true
fi
echo "Foxhole is removed. DNS is back to your network's own servers."
