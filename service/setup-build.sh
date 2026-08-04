#!/bin/sh
# Installs the system packages needed to build radiod .debs on Debian 13
# (trixie) — the build box or a debian:trixie CI container. Idempotent.
#
# Usage:
#   ./setup-build.sh           native build dependencies only (what the
#                              native CI jobs use)
#   ./setup-build.sh cross     also the arm64 + armhf multiarch cross
#                              toolchains (amd64 hosts: the build box and
#                              the armhf CI job)
#
# Rust itself (rustup/cargo, cargo-deb) is deliberately not installed here:
# it is per-user, not system, state. See service/README.md.

set -eu

SUDO=""
[ "$(id -u)" -eq 0 ] || SUDO="sudo"

# No libav*-dev: build-ffmpeg.sh builds the minimal FFmpeg radiod links
# statically, from a pinned tarball. libssl-dev is what that build needs
# for --enable-openssl (https). The tarball is gzip, which tar handles
# without an extra package.
#
# nasm assembles FFmpeg's hand-written x86 SIMD. It is only used when the
# target is x86, but it is listed unconditionally because the alternative
# (--disable-x86asm) would ship a measurably slower amd64 build, and
# because an arm64 host cross-compiling to amd64 needs it too. Nothing on
# an ARM target uses it — those paths go through gas — which is why this
# only ever surfaces on the amd64 leg.
NATIVE_PACKAGES="build-essential pkg-config clang git ca-certificates curl
    nasm libssl-dev libasound2-dev"

# The libssl/libasound dev packages are Multi-Arch: same, so the foreign
# copies co-install next to the native ones. pkgconf:<arch> provides the
# <triplet>-pkg-config wrapper whose personality points at that arch's
# multiarch paths. armhf is Debian's ARMv7 port (the Banana Pi — NOT the
# ARMv6 Raspbian world of the retired Pi Zero W).
cross_packages_for() {
    case "$1" in
    arm64) echo "crossbuild-essential-arm64 binutils-aarch64-linux-gnu
        pkgconf:arm64 libssl-dev:arm64 libasound2-dev:arm64" ;;
    armhf) echo "crossbuild-essential-armhf binutils-arm-linux-gnueabihf
        pkgconf:armhf libssl-dev:armhf libasound2-dev:armhf" ;;
    esac
}

# Which targets a host can cross to. An amd64 box covers both ARM ports;
# an arm64 Pi covers armhf (and builds arm64 natively), which is enough to
# produce every .deb except amd64's from a Raspberry Pi.
cross_targets_for() {
    case "$1" in
    amd64) echo "arm64 armhf" ;;
    arm64) echo "armhf" ;;
    *) return 1 ;;
    esac
}

case "${1:-native}" in
native)
    $SUDO apt-get update
    # shellcheck disable=SC2086
    $SUDO apt-get install -y $NATIVE_PACKAGES
    ;;
cross)
    host=$(dpkg --print-architecture)
    targets=$(cross_targets_for "$host") || {
        echo "setup-build.sh: no cross targets known for host $host" >&2
        exit 2
    }
    packages=$NATIVE_PACKAGES
    for arch in $targets; do
        dpkg --print-foreign-architectures | grep -qx "$arch" ||
            $SUDO dpkg --add-architecture "$arch"
        packages="$packages $(cross_packages_for "$arch")"
    done
    $SUDO apt-get update
    # shellcheck disable=SC2086
    $SUDO apt-get install -y $packages
    echo "setup-build.sh: cross targets for $host: $targets"
    ;;
*)
    echo "usage: $0 [native|cross]" >&2
    exit 2
    ;;
esac

echo "setup-build.sh: done"
