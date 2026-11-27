#!/bin/sh
# Builds a release tarball for one architecture, as the curl installer expects it:
#   packaging/dist.sh x86_64      ->  dist/foxhole-x86_64-linux.tar.gz
#   packaging/dist.sh aarch64     ->  dist/foxhole-aarch64-linux.tar.gz
# The binary is static (musl), so it runs on any distribution. Needs the Rust target
# (rustup target add <arch>-unknown-linux-musl) and a C compiler for it (musl-tools on Debian/Ubuntu).
# After building every architecture: (cd dist && sha256sum *.tar.gz > SHA256SUMS)
set -eu

arch=${1:?usage: $0 x86_64|aarch64}
target=$arch-unknown-linux-musl
root=$(cd "$(dirname "$0")/.." && pwd)
name=foxhole-$arch-linux

cd "$root"
cargo build --release --locked --target "$target"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
cp "target/$target/release/foxhole" packaging/install.sh packaging/uninstall.sh \
   packaging/foxhole.service packaging/foxhole.sysusers LICENSE README.md "$stage/$name/"
mkdir -p dist
# Fixed owner and order, so the tarball doesn't carry details of the machine that built it.
tar -czf "dist/$name.tar.gz" -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name "$name"
echo "dist/$name.tar.gz"
