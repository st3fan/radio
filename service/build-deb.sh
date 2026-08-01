#!/bin/sh
# Builds radiod_<version>_<arch>.deb for amd64 or arm64 on Debian 13.
# Native when the requested arch matches the host (the only path CI takes);
# a Debian-multiarch cross build for arm64 on the amd64 build box.
# One-time setup: ./setup-build.sh [cross], rustup, cargo install cargo-deb.

set -eu

ARCH="${1:?usage: build-deb.sh <amd64|arm64>}"
cd "$(dirname "$0")"

case "$ARCH" in
amd64) TRIPLE=x86_64-unknown-linux-gnu ;;
arm64) TRIPLE=aarch64-unknown-linux-gnu ;;
*)
    echo "usage: $0 <amd64|arm64>" >&2
    exit 2
    ;;
esac

command -v cargo-deb >/dev/null || {
    echo "build-deb.sh: cargo-deb not found — run: cargo install cargo-deb" >&2
    exit 1
}

HOST=$(dpkg --print-architecture)

if [ "$ARCH" = "$HOST" ]; then
    cargo deb
    echo "deb: $(ls target/debian/radiod_*_"$ARCH".deb)"
    exit 0
fi

if [ "$HOST" != "amd64" ] || [ "$ARCH" != "arm64" ]; then
    echo "build-deb.sh: only amd64 -> arm64 cross builds are supported" >&2
    exit 2
fi

rustup target list --installed | grep -qx "$TRIPLE" || rustup target add "$TRIPLE"

# Debian multiarch is the sysroot: the linker and pkg-config wrapper come
# from crossbuild-essential-arm64 / pkgconf:arm64, and bindgen needs the
# target triple plus the multiarch include dir so libclang parses the
# arm64 headers (never the host's layout).
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export PKG_CONFIG_aarch64_unknown_linux_gnu=aarch64-linux-gnu-pkg-config
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--target=aarch64-unknown-linux-gnu -I/usr/include/aarch64-linux-gnu"

cargo deb --target "$TRIPLE"
echo "deb: $(ls "target/$TRIPLE/debian/radiod_"*_"$ARCH".deb)"
