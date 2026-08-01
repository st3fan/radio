#!/bin/sh
# Installs the system packages needed to build radiod .debs on Debian 13
# (trixie) — the build box or a debian:trixie CI container. Idempotent.
#
# Usage:
#   ./setup-build.sh           native build dependencies only (what CI uses)
#   ./setup-build.sh cross     also the arm64 multiarch cross toolchain
#                              (amd64 hosts only — the build box)
#
# Rust itself (rustup/cargo, cargo-deb) is deliberately not installed here:
# it is per-user, not system, state. See service/README.md.

set -eu

SUDO=""
[ "$(id -u)" -eq 0 ] || SUDO="sudo"

NATIVE_PACKAGES="build-essential pkg-config clang git ca-certificates curl
    libavformat-dev libavcodec-dev libavutil-dev libswresample-dev
    libasound2-dev"

# The libav*/libasound dev packages are Multi-Arch: same, so the arm64
# copies co-install next to the native ones. pkgconf:arm64 provides the
# aarch64-linux-gnu-pkg-config wrapper whose personality points at the
# arm64 multiarch paths.
CROSS_PACKAGES="crossbuild-essential-arm64 binutils-aarch64-linux-gnu
    pkgconf:arm64
    libavformat-dev:arm64 libavcodec-dev:arm64 libavutil-dev:arm64
    libswresample-dev:arm64 libasound2-dev:arm64"

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
    dpkg --print-foreign-architectures | grep -qx arm64 || \
        $SUDO dpkg --add-architecture arm64
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
