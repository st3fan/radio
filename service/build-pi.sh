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
    # trixie is merged-usr: the libc linker script refers to /lib/... paths,
    # which must resolve inside the sysroot; the dynamic loader also lives
    # at a compat path directly under /lib.
    [ -e "$SYSROOT/lib" ] || ln -s usr/lib "$SYSROOT/lib"
    [ -e "$SYSROOT/usr/lib/ld-linux-armhf.so.3" ] || \
        ln -s arm-linux-gnueabihf/ld-linux-armhf.so.3 "$SYSROOT/usr/lib/ld-linux-armhf.so.3"
    # --copy-unsafe-links turns absolute symlinks (e.g. libfoo.so ->
    # /usr/lib/...) into real files so they resolve inside the sysroot.
    rsync -a --delete --copy-unsafe-links "$host:/usr/include" "$SYSROOT/usr/"
    rsync -a --delete --copy-unsafe-links "$host:/usr/lib/arm-linux-gnueabihf" "$SYSROOT/usr/lib/"
    rsync -a --delete --copy-unsafe-links "$host:/usr/share/pkgconfig" "$SYSROOT/usr/share/"
    # trixie's linux-libc-dev keeps the kernel UAPI headers under
    # /usr/lib/linux/uapi and symlinks <asm/...> into it.
    rsync -a --delete --copy-unsafe-links "$host:/usr/lib/linux" "$SYSROOT/usr/lib/" 2>/dev/null || true
    # The Pi's own crt/libgcc objects (needs libgcc-<N>-dev on the Pi).
    # Debian's cross gcc ships ARMv7/Thumb-2 startup objects and division
    # helpers that SIGILL on the Zero; linking must use the Pi's instead.
    mkdir -p "$SYSROOT/usr/lib/gcc"
    # (excluding the Pi's LTO plugin, which the host linker would try to
    # dlopen and choke on — it is 32-bit ARM code)
    rsync -a --delete --copy-unsafe-links --exclude 'liblto_plugin*' \
        "$host:/usr/lib/gcc/arm-linux-gnueabihf" "$SYSROOT/usr/lib/gcc/"
    # ffmpeg-sys' build script compiles a version-probe FOR THE HOST but
    # with the sysroot's -I flags; the host compiler then reaches the arm
    # glibc stubs dispatcher and asks for the soft-float variant, which a
    # hard-float sysroot does not have. An empty shim satisfies it — the
    # probe only reads FFmpeg version macros.
    touch "$SYSROOT/usr/include/arm-linux-gnueabihf/gnu/stubs-soft.h"
    # Raspbian's FFmpeg carries vendor patches adding Broadcom pixel
    # formats (SAND/RPI4) that upstream ffmpeg-next's exhaustive matches
    # do not know. They sit at the tail of the enum, right before
    # AV_PIX_FMT_NB, so hiding them from bindgen changes no other value —
    # and an audio-only daemon never touches video pixel formats.
    sed -i -E 's#^([[:space:]]*)(AV_PIX_FMT_(SAND128|SAND64_10|SAND64_16|RPI4_8|RPI4_10),.*)#\1// \2 // hidden for cross-build (Raspbian vendor format)#' \
        "$SYSROOT/usr/include/arm-linux-gnueabihf/libavutil/pixfmt.h"
    echo "sysroot ready in $SYSROOT"
    ;;

build)
    if [ ! -d "$SYSROOT/usr/include" ]; then
        echo "no sysroot in $SYSROOT — run: $0 sync <pi-host>" >&2
        exit 1
    fi

    # Rust: ARMv6 target, cross linker, and the sysroot for shared libs.
    # The -B prefixes make the linker take crt*.o and libgcc.a from the
    # Pi's sysroot instead of the cross gcc's ARMv7/Thumb-2 companions,
    # which would SIGILL on the Zero before main() even runs.
    gccdir=$(ls -d "$SYSROOT"/usr/lib/gcc/arm-linux-gnueabihf/* | head -1)
    export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
    export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C link-arg=--sysroot=$SYSROOT -C link-arg=-B$SYSROOT/usr/lib/arm-linux-gnueabihf -C link-arg=-B$gccdir"

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
    echo "binary: ${CARGO_TARGET_DIR:-$(pwd)/target}/$TARGET/release/radiod"
    ;;

*)
    echo "usage: $0 [sync <pi-host> | build]" >&2
    exit 2
    ;;
esac
