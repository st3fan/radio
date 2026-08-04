#!/bin/sh
# Builds the minimal, statically linked FFmpeg that radiod links against,
# into service/target/ffmpeg/<triple>. Debian's libavcodec61 is one fat
# build: it drags ~140 packages onto the radio (video codecs, Mesa, LLVM
# via Recommends) because its .so has 39 DT_NEEDED entries. Building our
# own with --disable-everything plus the codecs an internet radio needs
# turns that into ~4 MB of .a files and two runtime deps.
#
# The result is consumed via pkg-config, not FFMPEG_DIR: ffmpeg-sys-next
# probes with pkg_config's static mode, which reads Libs.private out of
# the generated .pc files and so emits -lssl/-lcrypto for us. FFMPEG_DIR
# would link the av* libraries only, leaving OpenSSL's symbols undefined.
# build-deb.sh sets PKG_CONFIG_PATH/PKG_CONFIG_LIBDIR accordingly.
#
# Re-running is cheap: a stamp file records the version, checksum and
# configure line, and the build is skipped when nothing has changed.

set -eu

# Pinned, never a branch — `release/8.1` moves under us and would make
# builds unreproducible. Verified once against FFmpeg's release signing
# key FCF986EA15E6E293A5644F10B4322F04D67658D8, whose fingerprint is
# published on https://ffmpeg.org/download.html; the checksum below is
# what pins it from here on. ffmpeg-sys-next's major.minor tracks
# FFmpeg's, so 8.1.x is what the 8.1.0 crate expects.
FFMPEG_VERSION=8.1.2
FFMPEG_SHA256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c

ARCH="${1:?usage: build-ffmpeg.sh <amd64|arm64|armhf>}"
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

HOST=$(dpkg --print-architecture)
if [ "$ARCH" != "$HOST" ]; then
    echo "build-ffmpeg.sh: cross builds are not wired up yet (host is $HOST)" >&2
    exit 2
fi

# What a radio actually has to decode. Deliberately wider than SomaFM —
# radiod plays any stream URL, so this covers the formats internet radio
# uses in practice (MP3, AAC including HE-AAC, Vorbis, Opus, FLAC, ALAC)
# rather than only the ones our own channel list offers. Adding one later
# is a one-line change here plus a rebuild.
#
# --disable-autodetect is what keeps the external codec libraries out; it
# also means TLS has to be asked for by name, hence --enable-openssl and
# the https/tls protocols.
#
# udp is in that list because tls_openssl.c references ff_udp_set_remote_addr
# and ff_udp_get_last_recv_addr for its DTLS path; without the udp protocol
# those objects are never compiled and linking radiod fails on undefined
# symbols. We do not otherwise need udp.
CONFIGURE_FLAGS="--disable-everything --disable-autodetect
    --disable-programs --disable-doc --disable-debug
    --disable-shared --enable-static --enable-pic
    --disable-avdevice --disable-avfilter --disable-swscale --enable-swresample
    --enable-decoder=mp3,mp3float,aac,aac_latm,aac_fixed,vorbis,opus,flac,alac
    --enable-decoder=pcm_s16le,pcm_s16be,pcm_u8,wavpack
    --enable-demuxer=mp3,aac,ogg,flac,mov,wav,hls,mpegts,asf,matroska,aiff
    --enable-parser=mpegaudio,aac,aac_latm,vorbis,opus,flac
    --enable-protocol=file,http,https,tcp,tls,udp,hls,crypto,httpproxy
    --enable-openssl"

PREFIX="$PWD/target/ffmpeg/$TRIPLE"
WORK="$PWD/target/ffmpeg/src"
TARBALL="$WORK/ffmpeg-$FFMPEG_VERSION.tar.xz"
SRC="$WORK/ffmpeg-$FFMPEG_VERSION"

# Any change to the version, the checksum or the flags invalidates the
# tree — otherwise a stale build silently survives an edit to this file.
STAMP=$(printf '%s\n%s\n%s\n' "$FFMPEG_VERSION" "$FFMPEG_SHA256" "$CONFIGURE_FLAGS" |
    sha256sum | cut -d' ' -f1)

if [ "$(cat "$PREFIX/.stamp" 2>/dev/null || true)" = "$STAMP" ]; then
    echo "build-ffmpeg.sh: $PREFIX is up to date"
    exit 0
fi

mkdir -p "$WORK"

if [ ! -f "$TARBALL" ]; then
    echo "build-ffmpeg.sh: fetching ffmpeg-$FFMPEG_VERSION"
    # ffmpeg.org resets connections often enough to fail a CI run (seen as
    # curl 35 mid-transfer), and a release must not hinge on one flaky
    # download. --retry-all-errors is what makes curl retry those; plain
    # --retry only covers a narrower set of transient failures.
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 3 \
        --connect-timeout 20 -o "$TARBALL.part" \
        "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
    mv "$TARBALL.part" "$TARBALL"
fi

echo "$FFMPEG_SHA256  $TARBALL" | sha256sum -c - >/dev/null || {
    echo "build-ffmpeg.sh: checksum mismatch for $TARBALL — refusing to build" >&2
    echo "  delete it and re-run if you believe the download was corrupt" >&2
    exit 1
}

rm -rf "$SRC" "$PREFIX"
tar -xf "$TARBALL" -C "$WORK"

# FFmpeg's own build is noisy with upstream warnings, so it goes to a log
# — but on failure the log is what you need, so print the tail of it.
LOG="$WORK/build.log"
: >"$LOG"
run() {
    if ! "$@" >>"$LOG" 2>&1; then
        echo "build-ffmpeg.sh: '$*' failed. Last 40 lines of $LOG:" >&2
        tail -40 "$LOG" >&2
        exit 1
    fi
}

# shellcheck disable=SC2086
(cd "$SRC" && run ./configure --prefix="$PREFIX" $CONFIGURE_FLAGS)

# The licence is a build-time invariant, not a matter of trust: radiod is
# MIT and links this statically, which LGPL 2.1 permits. Enabling a GPL
# component (--enable-gpl, or an external encoder like x264) would make
# FFmpeg GPL v2+ and force radiod to become GPL too. Fail loudly rather
# than let that arrive with a future codec addition.
for symbol in CONFIG_GPL CONFIG_NONFREE CONFIG_VERSION3; do
    value=$(awk -v s="$symbol" '$2 == s { print $3 }' "$SRC/config.h")
    if [ "$value" != "0" ]; then
        echo "build-ffmpeg.sh: $symbol is $value, expected 0." >&2
        echo "  This build would not be LGPL 2.1 and radiod could not stay MIT." >&2
        exit 1
    fi
done

run make -C "$SRC" -j"$(nproc)"
run make -C "$SRC" install

printf '%s\n' "$STAMP" >"$PREFIX/.stamp"

# A stable name for "the one a plain `cargo build` should link against".
# .cargo/config.toml points PKG_CONFIG_PATH at target/ffmpeg/host, so once
# this has been run, cargo test/clippy just work on Linux without anyone
# having to know about PKG_CONFIG_PATH. Cross builds do not touch it —
# build-deb.sh sets the variable explicitly for those.
if [ "$ARCH" = "$HOST" ]; then
    ln -sfn "$TRIPLE" "$PWD/target/ffmpeg/host"
fi

echo "build-ffmpeg.sh: installed FFmpeg $FFMPEG_VERSION (LGPL 2.1) into $PREFIX"
