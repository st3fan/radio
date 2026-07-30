#!/bin/sh
# Cross-compiles radiod for the Pi Zero W 1 (ARMv6) against a sysroot taken
# from the Pi itself, so link-time libraries match the runtime ones exactly.
# Run this on a Debian machine (the Debian PC); see service/README.md.
#
# One-time setup on this machine:
#   rustup target add arm-unknown-linux-gnueabihf
#   sudo apt install gcc-arm-linux-gnueabihf clang rsync
# One-time setup on the Pi (headers + .pc files for the sysroot):
#   sudo apt install libavformat-dev libavcodec-dev libavutil-dev \
#       libswresample-dev libasound2-dev
#
# Usage:
#   ./build-pi.sh sync <pi-host>   (re)create the sysroot from the Pi
#   ./build-pi.sh build            cross-compile the release binary
#
# The sysroot lives in $RADIO_PI_SYSROOT (default ~/pi-sysroot). Re-run
# `sync` after upgrading packages on the Pi.

set -eu

SYSROOT="${RADIO_PI_SYSROOT:-$HOME/pi-sysroot}"
TARGET=arm-unknown-linux-gnueabihf
cd "$(dirname "$0")"

case "${1:-build}" in
sync)
    host="${2:?usage: build-pi.sh sync <pi-host>}"
    mkdir -p "$SYSROOT/usr/lib" "$SYSROOT/usr/share"
    # --copy-unsafe-links turns absolute symlinks (e.g. libfoo.so ->
    # /usr/lib/...) into real files so they resolve inside the sysroot.
    rsync -a --delete --copy-unsafe-links "$host:/usr/include" "$SYSROOT/usr/"
    rsync -a --delete --copy-unsafe-links "$host:/usr/lib/arm-linux-gnueabihf" "$SYSROOT/usr/lib/"
    rsync -a --delete --copy-unsafe-links "$host:/usr/share/pkgconfig" "$SYSROOT/usr/share/"
    echo "sysroot ready in $SYSROOT"
    ;;

build)
    if [ ! -d "$SYSROOT/usr/include" ]; then
        echo "no sysroot in $SYSROOT — run: $0 sync <pi-host>" >&2
        exit 1
    fi

    # Rust: ARMv6 target, cross linker, and the sysroot for shared libs.
    export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
    export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C link-arg=--sysroot=$SYSROOT"

    # C code built by build scripts (the cc crate): Debian's cross gcc
    # defaults to ARMv7, which the Zero's ARM1176 cannot execute — force
    # ARMv6 + VFP to match the Rust target.
    export CC_arm_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
    export CFLAGS_arm_unknown_linux_gnueabihf="-march=armv6 -mfpu=vfp -mfloat-abi=hard -marm --sysroot=$SYSROOT"

    # pkg-config: resolve libav*/alsa strictly inside the sysroot.
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
    export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/arm-linux-gnueabihf/pkgconfig:$SYSROOT/usr/share/pkgconfig"

    # bindgen (ffmpeg-sys-next): parse the Pi's headers, not the host's.
    export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=$SYSROOT -I$SYSROOT/usr/include -I$SYSROOT/usr/include/arm-linux-gnueabihf"

    cargo build --release --target "$TARGET"
    echo "binary: $(pwd)/target/$TARGET/release/radiod"
    ;;

*)
    echo "usage: $0 [sync <pi-host> | build]" >&2
    exit 2
    ;;
esac
