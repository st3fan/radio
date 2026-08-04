# radiod's runtime dependencies

Why the package depends on what it does, and how to re-check the numbers
when Debian moves. Background and the full decision record are in
`plans/20260803-01-dependency-optimization.md`.

## What we depend on

```
Depends: libasound2t64, libc6, libssl3t64, ca-certificates
```

- **libasound2t64** — ALSA output.
- **libc6** — unavoidable.
- **libssl3t64** — FFmpeg's TLS backend, for `https://` stream URLs.
  Deliberately *not* statically linked: OpenSSL is the one component you
  want getting Debian's security updates without us rebuilding.
  `zlib1g`/`libzstd1` are not listed because `libssl3t64` already
  depends on them.
- **ca-certificates** — `pipeline.rs` sets `tls_verify=1`, and OpenSSL
  verifies against the system CA store. FFmpeg still defaults
  verification *off* through libavformat 62
  (`FF_API_NO_DEFAULT_TLS_VERIFY`), which makes https encrypted but
  unauthenticated; we turn it on, so this package is load-bearing rather
  than decorative.

FFmpeg is absent from that list because `service/build-ffmpeg.sh` builds
a minimal one from a pinned tarball and radiod links it statically —
about 4 MB of `.a` files, 2.6 MB in the binary.

## Why not Debian's libavcodec61

It is one fat build. Its `.so` carries **39 `DT_NEEDED` entries** —
libx265, libaom, librav1e, libSvtAv1Enc, libvpx, libcodec2, librsvg,
libcairo, libglib, libva — all resolved by `ld.so` at load time, so a
missing one means radiod does not *start*. The dependency list is a
property of how Debian compiled the binary; declaring a narrower
`Depends` is not an option. The only lever is to link a different
binary.

Measured on RPi OS trixie arm64, resolving radiod's old `Depends`
against an empty dpkg status ("what would a fresh machine install"):

| | packages | installed size |
|---|---|---|
| the four libav\* names + alsa + libc | 144 | 181 MB |
| alsa + libc alone | 5 | 25 MB |
| **cost of the libav\* names** | **139** | **156 MB** |

Roughly 34 MB of that is video and image codecs (the single largest
package being `libcodec2-1.2`, a 16 MB ham-radio speech codec) and
another 33 MB is desktop graphics, reached via
`libavcodec61 → librsvg2-2 → pango → fontconfig`.

Reproduce with:

```sh
apt-get install -s --no-install-recommends -o Dir::State::status=/dev/null \
    libavformat61 libavcodec61 libavutil59 libswresample5 libasound2t64 libc6 \
  | grep -c '^Inst '
```

Drop `-o Dir::State::status=/dev/null` to ask the same question of the
machine you are on. See `notes/clean-install.md` for the Recommends
trap, which is worth a further 32 packages / 254 MB and is *not* fixed
by this change — it is an install-time policy.

## Re-checking after a Debian release

1. Confirm the package names still exist (`libssl3t64` and
   `libasound2t64` both carry the 64-bit-time-t suffix from the trixie
   transition; a future release may rename them again).
2. Re-run the simulation above and update the numbers here.
3. Check `service/build-ffmpeg.sh`'s pinned FFmpeg version against
   what `ffmpeg-sys-next` expects — the crate's major.minor tracks
   FFmpeg's, so bumping `ffmpeg-next` means bumping the tarball too.

## The FFmpeg build

`service/build-ffmpeg.sh` fetches a pinned, sha256-checked release
tarball (verified once against FFmpeg's release signing key
`FCF986EA15E6E293A5644F10B4322F04D67658D8`, whose fingerprint is
published on <https://ffmpeg.org/download.html>) and configures it with
`--disable-everything --disable-autodetect` plus only the codecs,
demuxers, parsers and protocols an internet radio needs. It installs
into `service/target/ffmpeg/<triple>`; `build-deb.sh` puts that prefix
first on `PKG_CONFIG_PATH`, which is all `ffmpeg-sys-next` needs — it
probes with pkg-config, and because the prefix contains no `.so` the
link is static. `PKG_CONFIG_PATH` rather than `PKG_CONFIG_LIBDIR` so the
system `.pc` files are still visible; the `alsa` crate needs libasound's.

Two things the script enforces:

- **The licence is a build-time invariant.** It asserts `CONFIG_GPL`,
  `CONFIG_NONFREE` and `CONFIG_VERSION3` are all `0` and fails
  otherwise. radiod is MIT and links FFmpeg statically, which LGPL 2.1
  permits; enabling a GPL component would make FFmpeg GPL v2+ and force
  radiod to become GPL too.
- **A stamp file** records the version, checksum and configure line, so
  re-running is a no-op until one of them changes.

One non-obvious flag: the `udp` protocol is enabled even though radiod
never uses it. `tls_openssl.c` references `ff_udp_set_remote_addr` and
`ff_udp_get_last_recv_addr` for its DTLS path, and without the udp
protocol those objects are never compiled, so linking radiod fails on
undefined symbols.

### Cross builds

`build-ffmpeg.sh <arch>` cross-compiles through Debian's multiarch
toolchains (`--enable-cross-compile --cross-prefix --arch --target-os`),
using the same prefix-per-triple layout, so `build-deb.sh` points
pkg-config at `target/ffmpeg/<triple>` whichever architecture it is
building.

**Which host can build which target is a question of what is installed,
not of the host's architecture.** `setup-build.sh cross` works out the
targets for the host it runs on:

| host | native | cross targets |
|---|---|---|
| amd64 | amd64 | arm64, armhf |
| arm64 | arm64 | armhf |

So an arm64 Raspberry Pi can produce every .deb except amd64's. Both
`build-ffmpeg.sh` and `build-deb.sh` check for the specific
`<triplet>-gcc` and point at `./setup-build.sh cross` when it is missing,
rather than refusing by host architecture.

No `-mcpu`/`-mtune` is passed anywhere. The cross compiler's defaults are
Debian's baseline for the port (armhf is ARMv7-A with VFPv3-D16), which
is what we want: the .deb is built on one machine and runs on another, so
anything tuned to the builder is a latent SIGILL. This is also why
`ffmpeg-sys-next`'s own `build` feature is not used — it passes
`-march=native -mtune=native` on native builds.
