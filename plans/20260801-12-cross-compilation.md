# Plan: a cross-compilation story (x86_64, arm64, armv6)

- **Date:** 2026-08-01
- **Status:** draft — presents researched options; Stefan picks the approach
- **Goal:** build `radiod` .deb packages for **amd64**, **arm64**, and
  **armv6** (Pi Zero W) plus the `Architecture: all` website .deb, from
  the Debian 13 build box, **without Docker or emulation** — and reuse the
  same scripts in a tag-triggered GitHub Actions workflow that attaches
  the .debs to a GitHub Release.

## Background

Today only the armv6 target has a story: `service/build-pi.sh`
cross-builds against a sysroot **rsynced from the running Pi** and
packages with `cargo deb --target`. That works but has two problems: it
needs a live Pi (a non-starter for CI), and it covers one of the three
targets. amd64 and arm64 have no story at all.

The hard part is not Rust — rustup targets are Tier 1 (x86_64, aarch64)
and Tier 2 with host tools (`arm-unknown-linux-gnueabihf`, which *is* the
ARMv6+VFP2 Raspbian target). The hard part is the C half: ffmpeg-sys-next
needs target FFmpeg headers/libs via **pkg-config** and **bindgen**, and
alsa-sys needs libasound — for every target, plus a linker whose startup
objects (crt*.o, libgcc.a) are safe for the target CPU.

All targets run Debian 13 (trixie) userlands — the Pi runs Raspberry Pi
OS trixie — which removes the usual glibc/soname skew problem: build box
and targets share exact library versions.

## What the research says (2025–2026)

Three independent web-research passes (tooling landscape, Debian
multiarch mechanics, GitHub Actions release patterns) converge:

1. **`cross` (cross-rs) is ruled out** — it is Docker/Podman by design,
   with no containerless mode. Its own FAQ also flags that apt-installing
   armhf libraries in its ARMv6 image produces Pi-Zero-incompatible
   ARMv7 code.
2. **`cargo-zigbuild` does not fit** — zig cc only replaces the
   gcc/binutils/glibc layer. FFmpeg/ALSA would still need a real target
   sysroot for pkg-config and bindgen, and zig's ARMv6
   `arm-unknown-linux-gnueabihf` support is documented-broken (armv6/armv7
   object mixing, rust-cross/cargo-zigbuild#103).
3. **Plain cargo + GNU toolchains + Debian multiarch is the
   community-standard container-free answer**, and Debian is built for
   it: `dpkg --add-architecture arm64`, `apt install crossbuild-essential-arm64
   libavformat-dev:arm64 libasound2-dev:arm64 …`. The libav* and alsa dev
   packages are `Multi-Arch: same` (co-installable with the amd64 ones);
   libs/.pc/headers land under `/usr/lib/aarch64-linux-gnu` and
   `/usr/include/aarch64-linux-gnu`. Trixie's `pkgconf:arm64` ships an
   `aarch64-linux-gnu-pkg-config` wrapper with the right search paths.
   The rust `pkg-config` crate and bindgen are steered per target with
   suffixed env vars (`PKG_CONFIG_LIBDIR_<triple>`,
   `BINDGEN_EXTRA_CLANG_ARGS_<triple>`), same mechanism build-pi.sh
   already uses.
4. **armv6 can never come from Debian multiarch.** Debian armhf's
   baseline is ARMv7+VFP3+Thumb-2; Raspberry Pi OS 32-bit is a separate
   ARMv6+VFP2 rebuild under the same "armhf" name. Debian's
   `gcc-arm-linux-gnueabihf` emits ARMv6 code when told to, but its
   *prebuilt* crt/libgcc objects are ARMv7 and SIGILL on the Zero — the
   exact trap build-pi.sh's `-B` flags already defuse (still an open
   rust-lang issue, #145264). The sysroot must come from **Raspbian
   trixie packages**, and the Raspbian trixie archive
   (`raspbian.raspberrypi.com/raspbian trixie`) is alive and current.
5. **A Pi-free sysroot is possible without emulation**: downloading the
   needed .debs from the Raspbian trixie repo and extracting with
   `dpkg -x` executes no target code (`debootstrap --foreign` first
   stage is the same idea; only its second stage needs qemu, and a link
   sysroot doesn't need it). Absolute symlinks inside extracted packages
   must be rewritten to relative — same fixup `--copy-unsafe-links`
   does in today's rsync.
6. **GitHub Actions**: `container: debian:trixie` on runners is standard
   and gives byte-identical toolchains/libs to the build box (gotchas:
   apt-install git before checkout, `defaults.run.shell: bash`, jobs run
   as root). Public repos get **free native arm64 runners**
   (`ubuntu-24.04-arm`, GA since Aug 2025). `cargo deb --target … --no-build`
   packages a binary built by our own scripts. Checksums: attach a
   `SHA256SUMS`; GitHub also exposes per-asset digests natively now.
   Signing individual .debs is not common practice — a GPG-signed apt
   repo (e.g. on GitHub Pages) is the future option if we ever want
   `apt upgrade` flows.
7. **Lifespan note:** trixie is expected to be the **last**
   Debian/Raspbian generation supporting ARMv6. The armv6 lane has a
   visible end-of-life horizon; the amd64/arm64 story is the durable
   part.

## Recommended approach

One script, `service/build-deb.sh <arch>` (amd64 | arm64 | armv6),
runnable identically on the build box and inside a `debian:trixie` CI
container:

- **amd64** — native `cargo build --release` + `cargo deb`. Trivial.
- **arm64** — Debian multiarch cross: `crossbuild-essential-arm64` +
  `:arm64` dev packages + per-target env vars + `cargo deb --target
  aarch64-unknown-linux-gnu`. No sysroot directory at all — the multiarch
  root *is* the sysroot. A one-time `setup-cross.sh` (idempotent, also
  used by CI) adds the architecture and installs packages.
- **armv6** — keep the **proven** build-pi.sh environment (Debian
  `gcc-arm-linux-gnueabihf` as linker, `-B` flags stealing crt/libgcc
  from the sysroot, the stubs-soft.h shim, the Raspbian pixfmt hiding),
  but replace the sysroot *source*: a new `make-rpi-sysroot.sh` assembles
  it from **Raspbian trixie repo .debs** (`apt download`-style fetch +
  `dpkg -x` + symlink relativization) instead of rsyncing a live Pi.
  The Pi stops being build infrastructure.
- **Packaging** — `cargo deb --target` with the existing **explicit
  pinned Depends** (this sidesteps the known cross-arch
  `dpkg-shlibdeps` fragility entirely; trixie package names are
  identical across all three arches). Website .deb unchanged
  (`dpkg-deb`, arch `all`), with its hardcoded version wired to the git
  tag in CI.
- **Verification gates** — `file`/`readelf -A` on every cross binary
  (armv6: **no** `Tag_CPU_arch: v7` anywhere); install-and-run on the Pi
  Zero for armv6 and on this box for amd64. arm64 has no hardware here:
  CI's native arm64 runner can at least `cargo test` the arm64 build.
- **GitHub Actions** — tag-triggered (`v*`): matrix of three jobs all in
  `debian:trixie` containers running the *same* `setup-cross.sh` +
  `build-deb.sh` as the box (amd64 and armv6 on `ubuntu-latest`, arm64
  **native** on `ubuntu-24.04-arm` — where `build-deb.sh arm64` takes
  its native path and can run tests); a fan-in job builds the website
  .deb, collects artifacts, writes `SHA256SUMS`, and creates a **draft**
  release via `softprops/action-gh-release`. Sysroot + cargo caches via
  `actions/cache` keyed on the sysroot package list. Version: Cargo.toml
  stays the source of truth; the workflow fails if the tag disagrees.

### Alternatives considered (kept as fallbacks)

- **armv6 linker**: a crosstool-NG ARMv6 toolchain built for Raspbian
  (tttapa's `armv6-rpi-linux-gnueabihf` tarballs — Docker-built but
  consumed as a plain tarball, glibc 2.31 floor, actively maintained) is
  what most guides now recommend, and removes the `-B`/specs gymnastics.
  I recommend keeping our Debian-gcc approach because it is
  hardware-proven *in this repo* and zero new third-party trust; if the
  repo-built sysroot breaks it, the tttapa toolchain is the documented
  fallback and slot-in replacement (set it as linker, keep the sysroot).
- **arm64 in CI via cross-compile** on `ubuntu-latest` (identical to the
  build box path) instead of the native arm64 runner — more uniform, but
  loses free native testing; easy to switch later since both use the
  same script.
- **sbuild/pbuilder, nix pkgsCross**: proper Debian-native or nix cross
  builds exist but bring chroot/nix machinery and still can't produce
  ARMv6; not worth it for two binaries and a tarball of PHP.

## Phases

One stack: this plan as the bottom PR, then one branch/PR per phase
stacked on top.

### Phase 1 — multiarch setup + amd64/arm64 builds

`service/setup-cross.sh` (idempotent apt/multiarch setup, documented in
service/README.md) and `service/build-deb.sh` covering amd64 (native)
and arm64 (multiarch cross). Verify: both .debs build on the box;
`readelf`/`file` checks pass; `dpkg -I` shows correct Architecture and
Depends. Also verify on the box the research flags: `pkgconf:arm64`
wrapper behavior, `Multi-Arch: same` co-installs of the libav dev set.

### Phase 2 — Pi-free armv6 sysroot

`service/make-rpi-sysroot.sh`: fetch the pinned package closure from the
Raspbian trixie archive (Raspbian keyring verified), extract with
`dpkg -x`, relativize symlinks, apply the two header fixups
(stubs-soft.h shim, vendor pixfmt hiding). Fold build-pi.sh's build/deb
targets into `build-deb.sh armv6`; retire the rsync path (or keep
`sync` as a documented escape hatch). Verify: binary runs on the actual
Pi Zero; `readelf -A` clean; .deb installs there.

### Phase 3 — GitHub release workflow

`.github/workflows/release.yml` per the recommended design above, plus
website .deb version-from-tag. Verify with a prerelease tag (e.g.
`v0.4.0-rc1`): draft release appears with 4 .debs + SHA256SUMS; install
the armv6 artifact on the Pi.

## Acceptance criteria

- `build-deb.sh <arch>` produces installable .debs for all three arches
  on the build box with no Docker, qemu, or Pi involved.
- The armv6 .deb from the *repo-built* sysroot runs on the Pi Zero W
  (the real gate — readelf is necessary, hardware is sufficient).
- A `v*` tag produces a draft GitHub Release with
  `radiod_<v>_{amd64,arm64,armhf}.deb`, `radio-website_<v>_all.deb`,
  and `SHA256SUMS`, built by the same scripts the box uses.
- CLAUDE.md / notes/plan.md updated (build environments section: the
  Docker-on-Mac cross-build path is superseded).

## Key references

- Debian multiarch cross: wiki.debian.org/CrossCompiling,
  docs.opencv.org multiarch tutorial; pkgconf personalities
  (packages.debian.org/trixie/pkgconf)
- pkg-config crate cross env vars: docs.rs/pkg-config
- ARMv6 baseline trap: wiki.debian.org/RaspberryPi,
  rust-lang/rust#145264, cross-rs FAQ, RoEdAl/rpi0-cross-compile
- ARMv6 toolchain fallback: github.com/tttapa/docker-arm-cross-toolchain
- Raspbian trixie archive: raspbian.raspberrypi.com/raspbian dists/trixie
- cargo-deb cross + `--no-build`: github.com/kornelski/cargo-deb
- arm64 runners GA: github.blog changelog 2025-08-07; container jobs:
  docs.github.com "Run jobs in a container"
- Release patterns: softprops/action-gh-release, Swatinem/rust-cache,
  dtolnay/rust-toolchain (actions-rs is dead)
