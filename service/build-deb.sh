#!/bin/sh
# Builds radiod_<version>_<arch>.deb for amd64, arm64 or armhf (Debian's
# ARMv7 port) on Debian 13. Native when the requested arch matches the
# host; a Debian-multiarch cross build otherwise (amd64 hosts only).
# One-time setup: ./setup-build.sh [cross], rustup, cargo install cargo-deb.

set -eu

ARCH="${1:?usage: build-deb.sh <amd64|arm64|armhf>}"
cd "$(dirname "$0")"

case "$ARCH" in
amd64) TRIPLE=x86_64-unknown-linux-gnu ;;
arm64) TRIPLE=aarch64-unknown-linux-gnu ;;
armhf) TRIPLE=armv7-unknown-linux-gnueabihf ;;
*)
    echo "usage: $0 <amd64|arm64|armhf>" >&2
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

if [ "$HOST" != "amd64" ]; then
    echo "build-deb.sh: cross builds are only supported from amd64 hosts" >&2
    exit 2
fi

rustup target list --installed | grep -qx "$TRIPLE" || rustup target add "$TRIPLE"

# Debian multiarch is the sysroot: the linker and pkg-config wrapper come
# from crossbuild-essential-<arch> / pkgconf:<arch>, and bindgen needs the
# target triple plus the multiarch include dir so libclang parses the
# target's headers (never the host's layout — vital for 32-bit armhf).
case "$ARCH" in
arm64)
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    export PKG_CONFIG_aarch64_unknown_linux_gnu=aarch64-linux-gnu-pkg-config
    export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
    export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--target=aarch64-unknown-linux-gnu -I/usr/include/aarch64-linux-gnu"
    ;;
armhf)
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
    export PKG_CONFIG_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-pkg-config
    export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
    export BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_gnueabihf="--target=armv7-unknown-linux-gnueabihf -I/usr/include/arm-linux-gnueabihf"
    ;;
esac

cargo deb --target "$TRIPLE"
echo "deb: $(ls "target/$TRIPLE/debian/radiod_"*_"$ARCH".deb)"
