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
# for --enable-openssl (https); xz-utils unpacks the tarball.
NATIVE_PACKAGES="build-essential pkg-config clang git ca-certificates curl
    xz-utils libssl-dev libasound2-dev"

# The libssl/libasound dev packages are Multi-Arch: same, so the foreign
# copies co-install next to the native ones. pkgconf:<arch> provides the
# <triplet>-pkg-config wrapper whose personality points at that arch's
# multiarch paths. armhf is Debian's ARMv7 port (the Banana Pi — NOT the
# ARMv6 Raspbian world of the retired Pi Zero W).
CROSS_PACKAGES="crossbuild-essential-arm64 binutils-aarch64-linux-gnu
    pkgconf:arm64
    libssl-dev:arm64 libasound2-dev:arm64
    crossbuild-essential-armhf binutils-arm-linux-gnueabihf
    pkgconf:armhf
    libssl-dev:armhf libasound2-dev:armhf"

case "${1:-native}" in
native)
    $SUDO apt-get update
    # shellcheck disable=SC2086
    $SUDO apt-get install -y $NATIVE_PACKAGES
    ;;
cross)
    host=$(dpkg --print-architecture)
    if [ "$host" != "amd64" ]; then
        echo "setup-build.sh: cross setup is for amd64 hosts (this is $host)" >&2
        exit 2
    fi
    for arch in arm64 armhf; do
        dpkg --print-foreign-architectures | grep -qx "$arch" || \
            $SUDO dpkg --add-architecture "$arch"
    done
    $SUDO apt-get update
    # shellcheck disable=SC2086
    $SUDO apt-get install -y $NATIVE_PACKAGES $CROSS_PACKAGES
    ;;
*)
    echo "usage: $0 [native|cross]" >&2
    exit 2
    ;;
esac

echo "setup-build.sh: done"
