# Plan: build story for amd64 + arm64, with release-driven CI .debs

- **Date:** 2026-08-01
- **Status:** rewritten after review — Stefan chose the two-target option
- **Goal:** build `radiod` .debs for **amd64**, **arm64** and **armhf**
  (Debian's ARMv7 port) plus the `Architecture: all` website .deb, both
  **locally on the Debian 13 build box** and **in CI on public GitHub
  runners**. A GitHub workflow builds the .debs and attaches them to a
  GitHub Release; **only releases produce .deb builds**.

## Decision record: why two targets

The first version of this plan covered a third target, ARMv6 for the
Pi Zero W. Research showed that lane carries essentially all the
complexity: ARMv6 hard-float is not a Debian architecture (Debian armhf
is ARMv7-baseline), so it needs a Raspbian-derived sysroot, an
ARMv6-safe linker path, Docker/emulation for sysroot assembly, and
hardware verification — and trixie is the last Debian/Raspbian
generation supporting ARMv6 at all. Decision: retire the Pi Zero W in
favor of an arm64-capable board and drop the ARMv6 lane entirely.

**Update (during implementation): the Zero is retired.** The radio will
run on either a new arm64 Pi or an ARMv7 Banana Pi (BPI-M2 Zero,
Allwinner H3). That adds **armhf** — Debian's ARMv7 hard-float port, a
first-class Debian architecture — as a third target, and deletes the
legacy `service/build-pi.sh` ARMv6 path in this same stack.

With Debian-native targets only, everything is first-class distro
machinery: **no Docker, no emulation, no sysroots, no third-party
toolchains** — locally or in CI.

**armhf naming decision:** the .deb keeps the standard `_armhf` name
and `Architecture: armhf` field (dpkg requires the field anyway; a
deviating filename would just disagree with its own metadata). The
name-collision risk — an ARMv7 armhf .deb silently installing on
ARMv6 Raspbian hardware, where dpkg's architecture check cannot tell
them apart — is handled by a **preinst guard**: the package refuses to
install when `uname -m` reports pre-ARMv7 hardware (`armv6*` and
older). amd64/arm64 installs pass through the same check untouched.

## Design

### Local builds (the build box, Debian 13 x86_64)

- **`service/setup-build.sh`** — idempotent one-time setup, also usable
  inside a CI container: `dpkg --add-architecture arm64`, `apt install
  crossbuild-essential-arm64 pkgconf:arm64 libavformat-dev:arm64
  libavcodec-dev:arm64 libswresample-dev:arm64 libasound2-dev:arm64
  clang cargo-deb …` (exact list settled in phase 1). The libav*/alsa
  dev packages are `Multi-Arch: same`, co-installable with the native
  ones already on the box.
- **`service/build-deb.sh <amd64|arm64>`** — one script, used verbatim
  on the box and in CI:
  - target arch == host arch → plain `cargo build --release` +
    `cargo deb` (this is the only path CI ever takes);
  - arm64 on the x86_64 box → multiarch cross: rustup target
    `aarch64-unknown-linux-gnu`, linker `aarch64-linux-gnu-gcc`,
    `PKG_CONFIG_aarch64_unknown_linux_gnu=aarch64-linux-gnu-pkg-config`
    (trixie's pkgconf personality supplies the multiarch paths),
    `BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu` with
    `--target` + multiarch include dir, `CC_`/`CFLAGS_` equivalents,
    then `cargo deb --target aarch64-unknown-linux-gnu` (binutils
    cross-`strip` configured, or `--no-strip`).
  - The .deb keeps the existing **explicit pinned Depends** (identical
    package names on both arches in trixie), sidestepping cross-arch
    `dpkg-shlibdeps` entirely.
- **Verification on the box:** `dpkg -I` (Architecture/Depends),
  `file`/`readelf -h` on the binaries; the amd64 .deb installs and runs
  here. arm64 execution is CI's job (native runner) plus eventually the
  new board.

### CI builds (public GitHub runners only — confirmed viable)

Both architectures build **natively inside `debian:trixie` containers**
on GitHub-hosted runners, so CI contains zero cross-compilation and the
container userland is byte-identical to the build box:

- amd64: `runs-on: ubuntu-latest`, `container: debian:trixie`
- arm64: `runs-on: ubuntu-24.04-arm` (free for public repos, GA since
  Aug 2025), `container: debian:trixie` (arm64 image variant)

No self-hosted runners needed. Fallback if the arm64 runner label ever
regresses: run that job on `ubuntu-latest` with the *same*
`build-deb.sh arm64` cross path used on the box — still public runners.

Container gotchas (from research, encoded in the workflow): `apt-get
install git ca-certificates` before `actions/checkout` (else checkout
falls back to a tarball without `.git`), `defaults.run.shell: bash`,
jobs run as root — all routine.

### The release workflow (last phase)

- **Trigger: `on: release: types: [published]`** — creating/publishing
  a GitHub Release (from a `v*` tag) is the only thing that builds
  .debs. No per-push or per-PR .deb builds.
- Three jobs:
  1. `radiod-amd64` and 2. `radiod-arm64` — matrix legs as above; each
     runs `setup-build.sh` (native mode), `cargo test` (free
     confidence, notably the first-ever native arm64 test run), then
     `build-deb.sh`, and uploads the .deb as an artifact.
  3. `release-assets` (fan-in, after both) — builds the website .deb
     with `dpkg-deb` (its version taken from the tag instead of the
     script's hardcoded constant), downloads the artifacts, asserts
     tag == Cargo.toml version (fail loudly on mismatch), generates
     `SHA256SUMS`, and attaches everything to the triggering release
     with `gh release upload` (the `gh` CLI works fine in container
     jobs; `softprops/action-gh-release` is the alternative).
- Caching: `Swatinem/rust-cache` per job; toolchain via
  `dtolnay/rust-toolchain` (actions-rs is dead) or trixie's own rustc
  if its version satisfies rust-version in Cargo.toml — decide in
  phase 2 (the crate currently pins rust-version 1.97; trixie ships an
  older rustc, so rustup in the container is the likely answer).
- Artifacts per release: `radiod_<v>_amd64.deb`, `radiod_<v>_arm64.deb`,
  `radio-website_<v>_all.deb`, `SHA256SUMS`.

## Phases

One stack: this plan as the bottom PR, then one branch/PR per phase.

### Phase 1 — local builds: setup + build-deb for amd64/arm64

`setup-build.sh`, `build-deb.sh`, `.cargo` / env wiring for the arm64
cross path, service/README.md documentation. Verify on the box: both
.debs build; amd64 installs and runs; `dpkg -I` correct on both; the
flagged research items are confirmed in passing (`pkgconf:arm64`
wrapper, `Multi-Arch: same` co-installs, cargo-deb cross strip).

### Phase 2 — release workflow

`.github/workflows/release.yml` per the design above, plus the
website-version-from-tag change to `deploy/build-website-deb.sh`.
Verify with a prerelease (e.g. `v0.4.0-rc1` marked pre-release):
release gets all four assets; install the amd64 .deb on the box;
`cargo test` green on the arm64 runner. Delete the prerelease after.

### Phase 3 — armhf (ARMv7) target + ARMv6 retirement

Added when the Zero was retired mid-implementation. The armhf lane is
the arm64 lane with different names: `:armhf` multiarch packages +
`crossbuild-essential-armhf` in `setup-build.sh`; an
`armv7-unknown-linux-gnueabihf` case in `build-deb.sh` (linker
`arm-linux-gnueabihf-gcc`, `arm-linux-gnueabihf-pkg-config`, bindgen
`--target` + `-I/usr/include/arm-linux-gnueabihf`); a third workflow
matrix leg that **cross-compiles on `ubuntu-latest`** (no 32-bit ARM
GitHub runners exist — the arm64 runners' cores dropped 32-bit
execution), skipping the duplicate host `cargo test`. Plus:

- the preinst guard (see naming decision above), shipped for all
  arches via the existing `deploy/debian` maintainer scripts;
- delete `service/build-pi.sh` and the ARMv6 sections/mentions in
  service/README.md, README.md, CLAUDE.md and notes/plan.md;
- watch for the ffmpeg-sys host-probe `stubs-soft.h` gotcha recurring
  on 32-bit cross builds (known one-line fix if it does).

Verify: armhf .deb builds on the box; `readelf -A` shows an ARMv7
binary; the preinst guard rejects a simulated pre-ARMv7 machine and
passes on amd64 (install the amd64 .deb again); hardware verification
happens when the Banana Pi arrives.

## Acceptance criteria

- `build-deb.sh` produces installable .debs for amd64, arm64 and armhf
  on the build box, no Docker/emulation involved.
- Publishing a GitHub Release is the only trigger that produces .debs,
  entirely on public GitHub-hosted runners, and attaches
  amd64 + arm64 + armhf + website .debs plus `SHA256SUMS` to it.
- The same `build-deb.sh` runs unmodified in both places.
- `cargo test` passes natively on arm64 in CI.
- The armhf .deb refuses to install on pre-ARMv7 hardware (preinst
  guard) and installs cleanly elsewhere.
- `build-pi.sh` and all ARMv6 documentation are gone; service/README.md,
  README.md, CLAUDE.md and notes/plan.md describe only the new story.

## Key references (from the research round)

- Debian multiarch cross: wiki.debian.org/CrossCompiling; pkgconf
  personalities: packages.debian.org/trixie/pkgconf
- pkg-config crate cross env vars: docs.rs/pkg-config; bindgen:
  BINDGEN_EXTRA_CLANG_ARGS_<triple>
- cargo-deb cross + --no-build: github.com/kornelski/cargo-deb
- arm64 public runners GA: github.blog changelog 2025-08-07
- Container jobs: docs.github.com "Run jobs in a container";
  checkout-needs-git: actions/checkout#238
- Release tooling: cli.github.com (gh release upload),
  softprops/action-gh-release, Swatinem/rust-cache,
  dtolnay/rust-toolchain
