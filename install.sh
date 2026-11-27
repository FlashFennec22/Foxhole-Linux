#!/bin/sh
# Foxhole installer. Downloads the latest release for this machine, checks it, and installs it:
#
#   curl -fsSL https://raw.githubusercontent.com/FlashFennec22/Foxhole-Linux/main/install.sh | sh
#
# Run it again to upgrade. Remove Foxhole with `foxhole uninstall`.
#
# Settings (environment variables):
#   FOXHOLE_VERSION   a release tag such as v0.2.0 (default: the latest release)
#   PREFIX            where to install (default: /usr/local)
#
# Everything is in main(), which runs on the last line, so a download cut short runs nothing.

set -eu

REPO=FlashFennec22/Foxhole-Linux

say() { printf 'foxhole: %s\n' "$*"; }
die() { printf 'foxhole: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs '$1', which isn't installed"; }

main() {
    [ "$(uname -s)" = Linux ] || die "Foxhole for Linux only runs on Linux"
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "there's no Foxhole build for $(uname -m) yet" ;;
    esac
    for tool in curl tar sha256sum systemctl; do need "$tool"; done
    systemctl is-active --quiet systemd-resolved ||
        die "Foxhole needs systemd-resolved, which isn't running on this system"

    if [ "$(id -u)" = 0 ]; then
        sudo=
    else
        need sudo
        sudo=sudo
    fi

    # FOXHOLE_BASE_URL is for testing the installer against a local copy of a release.
    if [ -n "${FOXHOLE_BASE_URL:-}" ]; then
        base=$FOXHOLE_BASE_URL
    elif [ -n "${FOXHOLE_VERSION:-}" ]; then
        base=https://github.com/$REPO/releases/download/$FOXHOLE_VERSION
    else
        base=https://github.com/$REPO/releases/latest/download
    fi
    asset=foxhole-$arch-linux.tar.gz

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading $asset"
    curl -fsSL --proto '=https,file' --tlsv1.2 -o "$tmp/$asset" "$base/$asset" ||
        die "download failed ($base/$asset)"
    curl -fsSL --proto '=https,file' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
        die "download failed ($base/SHA256SUMS)"

    # Catches a damaged or swapped download. It's no defense against someone who controls the
    # release itself: they would control this script too.
    (cd "$tmp" && grep "  $asset\$" SHA256SUMS | sha256sum -c --status -) ||
        die "$asset doesn't match its checksum in SHA256SUMS; not installing it"

    tar -xzf "$tmp/$asset" -C "$tmp"
    if [ -n "$sudo" ]; then say "installing (sudo will ask for your password)"; fi
    $sudo env PREFIX="${PREFIX:-/usr/local}" sh "$tmp/foxhole-$arch-linux/install.sh"
}

main "$@"
