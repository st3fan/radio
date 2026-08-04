# Plan: dependency optimization — a statically linked, minimal FFmpeg

- **Date:** 2026-08-03
- **Status:** rewritten after measurement; two open questions now settled
  by Stefan.
  - **Product:** radiod must **play any stream URL**, not just the SomaFM
    channels its own website lists. That decides the decoder route — keep
    FFmpeg, build it ourselves, minimally, and link it statically.
  - **TLS:** depend on **system OpenSSL** rather than moving HTTPS into
    Rust. 37 MB of dependencies is acceptable, and it buys back the
    riskiest code in the plan (see "The TLS decision").
- **Goal:** shrink what `radiod` drags onto the radio. Today the package
  pulls **139 extra packages / 156 MB** past a base Debian install, none
  of which a radio needs. The target is a `radiod` that depends on
  `libasound2t64`, `libc6`, `libssl3t64` and `ca-certificates` — **12
  packages / 37 MB, down from 144 / 181 MB.**

## Background: the measurement

`service/Cargo.toml` declares:

```
depends = "libavformat61, libavcodec61, libavutil59, libswresample5, libasound2t64, libc6"
```

Measured on an arm64 trixie Pi — the same distro and architecture as
muzak — by asking apt to resolve those names against an *empty* dpkg
status, so the answer is "what would a fresh machine install", not "what
is already here":

```sh
apt-get install -s --no-install-recommends -o Dir::State::status=/dev/null \
    libavformat61 libavcodec61 libavutil59 libswresample5 libasound2t64 libc6 \
  | grep '^Inst ' | awk '{print $2}' | sort -u
```

| resolution | packages | installed size |
|---|---|---|
| all six Depends, no recommends | 144 | 181 MB |
| `libasound2t64` + `libc6` alone | 5 | 25 MB |
| **attributable to the four libav\* names** | **139** | **156 MB** |
| all six Depends, **with** recommends (apt's default) | 206 | 476 MB |

Two chains explain nearly all of it, and neither has anything to do with
audio: `libavcodec61` pulls 15 video and image codec packages (34 MB,
the largest single item being `libcodec2-1.2` at 16 MB — a ham-radio
speech codec) and reaches `librsvg2-2` → pango → fontconfig, dragging in
33 packages of desktop graphics (33 MB). `libavformat61` adds Blu-ray,
DVD, ZeroMQ and RabbitMQ.

### Why the Depends line cannot simply be trimmed

The obvious idea — declare only the packages we need — does not work,
and it is worth recording why so nobody retries it. Debian's
`libavcodec.so.61` carries **39 `DT_NEEDED` entries**: libx265, libaom,
librav1e, libSvtAv1Enc, libvpx, libcodec2, librsvg, libcairo, libglib,
libva and the rest. `DT_NEEDED` is resolved by `ld.so` when the library
is mapped, so a missing one means radiod fails to *start*, not that it
fails when handed an H.265 file. (Verified: `ldd` on the shipped `.so`
reports 29 "not found" on a machine without them.) dpkg would refuse the
install anyway. The dependency list is a property of how Debian compiled
the binary, so the only lever is to link a different binary.

### The one subset that *is* declinable: Recommends

The empty-status framing above is the right way to size the libav\* cost,
but it *overstates* the Recommends layer, because it counts recommends
of base packages any real system already has. Measured again against a
**real** RPi OS trixie arm64 dpkg status — a provisioned machine that
simply lacks the av stack:

| resolution (realistic) | packages | installed size |
|---|---|---|
| radiod's six Depends, apt's default | 113 | 360 MB |
| the same with `--no-install-recommends` | 81 | 106 MB |
| **avoidable by policy alone** | **32** | **254 MB** |

254 MB is the number to care about, and four packages dominate it:
**`libllvm19` (118 MB)**, **`mesa-vulkan-drivers` (70 MB)**,
`mesa-libgallium` (34 MB) and `libz3-4` (26 MB). The doorway is
`libva2`, whose `Recommends: va-driver-all | va-driver` pulls the Mesa
stack in behind it. An internet radio with no display and no GPU can end
up carrying a Vulkan driver and a full LLVM, because its audio decoder is
linked against a library that can also do hardware-accelerated video.

A .deb cannot dictate how apt treats its dependencies' Recommends, so
this belongs in the install procedure (`notes/clean-install.md`) — see
phase 0.

This **corrects a claim in that note**, which recorded that
`libavcodec61` "has 35 hard Depends and zero Recommends —
`--no-install-recommends` would not slim the install; the closure is
structural." The observation was right and the inference wrong: apt
applies Recommends transitively across the whole closure, so it is
`libva2`'s recommends that fire, not `libavcodec61`'s own. **Confirming
what muzak actually has installed is still outstanding**; it was not
reachable by hostname from the machine this was measured on.

## What radiod uses FFmpeg for

All of it lives in one file, `service/src/pipeline.rs` (216 lines),
behind the `Source` trait that `service/src/source.rs` already defines
and that the player takes as an injected `SourceFactory`:

1. **HTTP(S) fetch with reconnect** — `input_with_dictionary` with
   `reconnect`/`reconnect_streamed`/`reconnect_delay_max`.
2. **ICY metadata** — via `av_opt_get` on `icy_metadata_headers` /
   `icy_metadata_packet`. The *parsing* is already ours in
   `service/src/icy.rs`, which has no FFmpeg dependency.
3. **Demux + decode.**
4. **Sample format conversion** — note `pipeline.rs:110-117` passes the
   same layout and rate as both source and destination. It is not
   resampling; it converts the decoder's native format to packed s16.
   This step is load-bearing and must stay: FFmpeg's fixed-point MP3
   decoder emits `s16p` while `mp3float` emits `fltp`, and which one you
   get depends on how FFmpeg was configured.

## Decision record: why FFmpeg and not a Rust decoder

The alternatives were surveyed properly rather than assumed, because
"replace FFmpeg with symphonia" was this plan's original recommendation.

**radiod's own website only ever selects MP3.** `web.rs:117-146` filters
SomaFM's channel list to `format == "mp3"`, preferring `quality ==
"highest"`. SomaFM offers all 46 channels as `mp3`, `aac`, `aacp` high
and `aacp` low, but the AAC variants are unreachable through the UI. So
if the product were "play SomaFM", a pure-Rust MP3 decoder would be
sufficient and this plan would be much smaller.

**The product is "play any stream URL", so breadth is the requirement**,
and that is exactly where the Rust decoders stop:

| option | covers | system deps | verdict |
|---|---|---|---|
| FFmpeg, static minimal | everything, incl. HE-AAC v2 | none | **chosen** |
| symphonia | MP3, AAC-LC, FLAC, Vorbis | none | no HE-AAC — see below |
| `minimp3-sys` | MP3 only | none | vendors one CC0 header; too narrow |
| `fdk-aac` | full HE-AAC v2 | none | licence caveat below |
| `libmpg123-0t64` | MP3 only | 1 pkg, 0.3 MB | too narrow |
| `libfaad2` | AAC incl. HE | 1 pkg, 0.5 MB | **GPL** — would relicense radiod |

Symphonia cannot decode HE-AAC in any version: there is no QMF
filterbank in the crate at all. In 0.5.5 (what our lockfile has) it
parses the SBR flags and then ignores them, silently decoding the AAC-LC
core — audio missing its top octave. In 0.6.0 (2026-05-15) it fails
loudly instead: `if asc.object_type != Lc || asc.sbr_present … return
unsupported_error("aac: aac too complex")`. HE-AAC is common on
low-bitrate internet radio, including SomaFM's own 64k and 32k streams.

Also noted for the record: `fdk-aac-sys` declares `license = "MIT"`, but
that covers the Rust wrapper only — the bundled source carries the
Fraunhofer FDK licence, and Debian does not ship `libfdk-aac2` in the
archive at all. Not a trap we need to walk into, but not one to inherit
from a Cargo.toml field either.

`puremp3` (v0.1.0, 2019) and `rmp3` (2021) are stale; `claxon` and
`lewton` cover only FLAC and Vorbis.

## AirPlay is unaffected — and stays pure Rust

`airplay.rs` never mentions `ffmpeg` or `pipeline.rs`; `openairplay2`
hands us PCM, and `FfmpegSource` is referenced only by `main.rs` where
it is wired in as the `SourceFactory`. So none of this work can regress
AirPlay.

There is a tempting consolidation here — one decoder for both paths —
and it should be **declined**. `openairplay2` negotiates a single format
explicitly (`session.rs:467`: AAC-LC 44.1 kHz stereo, `audioFormat` bit
`0x400000`) and decodes it with `symphonia` (`decode.rs`, the crate's
`aac` feature only). Because the format is negotiated rather than
guessed, symphonia's HE-AAC gap can never bite there. Making
`openairplay2` depend on FFmpeg would trade its deliberate pure-Rust
design for nothing.

The practical consequences for this plan are small but worth stating:
symphonia stays in the binary regardless, via `openairplay2`, so
choosing FFmpeg for the radio path costs no extra crates it would
otherwise have saved; and the two decode paths stay independent, so
phase 3's custom-AVIO work cannot affect AirPlay.

## The finding that makes this possible

FFmpeg does not have to come from Debian. `ffmpeg-sys-next` links a
**prebuilt** FFmpeg via the `FFMPEG_DIR` environment variable (falling
back to a plain `lib/` + `include/` layout, exactly what `make install
--prefix` produces), and `ffmpeg-next` exposes a `static` feature
(`static = ["ffmpeg-sys-next/static"]`) that turns the emitted link
directives into `cargo:rustc-link-lib=static=avcodec`.

So we configure FFmpeg ourselves. Measured against FFmpeg 8.1 on the Pi,
with a codec set chosen for "any stream URL" — MP3, AAC (incl. HE-AAC),
Vorbis, Opus, FLAC, ALAC and PCM; the mp3/aac/ogg/flac/mov/wav/hls/
mpegts/asf/matroska/aiff demuxers; and file/http/tcp/hls/crypto
protocols:

| | archives | linked test binary | build | runtime deps |
|---|---|---|---|---|
| **radio-realistic set (chosen)** | **4.1 MB** | **2.58 MB** | **61 s** | libc, libm |
| narrow set (mp3/aac/flac/vorbis only) | 3.5 MB | 2.11 MB | 51 s | libc, libm |
| full-featured static (see rejected options) | 26.0 MB | 18.48 MB | 10 m 27 s | libc, libm |
| today, Debian's shared libraries | — | — | — | 144 pkgs / 181 MB |

Configure reports **LGPL version 2.1 or later**. Breadth is nearly free:
the full "any stream URL" codec set costs 0.6 MB and 10 seconds more
than a mp3-only-ish build.

Both were verified end to end against live SomaFM streams with a small C
harness linked against the archives:

- `groovesalad-128-mp3` → decoder `mp3float`, 44100 Hz stereo, 18.76 s
  decoded from a 300 KB capture.
- `groovesalad-64-aac` (HE-AAC, the format symphonia refuses) → decoder
  `aac`, **44100 Hz** stereo, 24.98 s from a 200 KB capture. Full rate
  from a 64 kbps stream confirms SBR is being applied.

The harness's entire `ldd` output is `libc` and `libm`.
**156 MB of packages becomes about 2.6 MB inside the binary.**

## Rejected options

**The `build` feature of `ffmpeg-sys-next`** — the `sdl2 = { features =
["bundled"] }` analogue. It works, and it does pass
`--disable-autodetect`, which alone removes every external library.
Rejected for three reasons found by reading its `build.rs`: it never
passes `--disable-everything` and offers no hook for extra configure
flags (26 MB of archives, an 18.5 MB binary, 10.5 minutes per
architecture); it fetches with `git clone --depth=1 -b release/8.1`, a
**moving branch** rather than a pinned tag, so builds are not
reproducible; and on the native path it adds `--extra-cflags=-march=native
-mtune=native`, which matters because our release workflow builds
**arm64 natively on `ubuntu-24.04-arm`** (a Neoverse-class CPU) and
ships that .deb to muzak's Cortex-A72. `FFMPEG_DIR` avoids all three.

**Rebuilding Debian's `libavcodec61` as a fork** — works, but means
tracking a Debian package through every security update.

**Trimming the Depends line** — impossible, see `DT_NEEDED` above.

## Design

### The TLS decision

Stream URLs are https (`https://ice2.somafm.com/...`). With
`--disable-autodetect` the build has no TLS backend — the protocol list
above is deliberately `file, http, tcp, hls, crypto`. FFmpeg needs an
external library for TLS. Confirmed from FFmpeg's configure (line 7493)
that OpenSSL 3.x only demands `--enable-version3` when GPL is enabled,
so `--enable-openssl` is clean for our LGPL build.

There were two candidate end states:

| end state | packages | size |
|---|---|---|
| today | 144 | 181 MB |
| **static FFmpeg + system OpenSSL + ca-certificates — chosen** | **12** | **37 MB** |
| static FFmpeg + HTTPS done in Rust — declined | 5 | 25 MB |

**Decision: system OpenSSL.** The alternative would have saved three
packages and 12 MB by fetching with ureq (whose rustls and
`webpki-roots` put the CA bundle in the binary), but it required feeding
libavformat through a custom `AVIOContext`. `ffmpeg-next` has no wrapper
for that — no `AVIOContext`, no `avio_alloc_context` — so it meant
hand-written FFI where a panic crossing the boundary is undefined
behaviour. Trading 12 MB for deleting the riskiest code in the plan is a
good trade on a box whose real constraint is 512 MB–1 GB of RAM, not
disk.

Two consequences worth stating plainly. FFmpeg keeps doing the network,
so `pipeline.rs` needs **no changes at all** — reconnect stays
libavformat's job and the `av_opt_get` ICY path stays as it is. And
`libssl3t64` is a shared library on the system, so unlike the statically
linked FFmpeg it gets security updates from Debian without us rebuilding
— which for a TLS implementation is a feature, not a compromise.

### macOS must not change

`CLAUDE.md` requires `cargo test`/`clippy`/`fmt` to stay green on macOS,
where FFmpeg comes from Homebrew via pkg-config. `FFMPEG_DIR` and the
`static` feature are therefore set **only by the .deb build path**, not
in `Cargo.toml`'s default features. Day-to-day Mac development keeps
using the system FFmpeg exactly as today.

### Pinning

Fetch a released FFmpeg tarball by tag with a recorded sha256 — never a
branch. The version and the full configure line live in the build script
so the build is reproducible and reviewable.

## Licensing

Verified from the generated config rather than the configure banner:
`CONFIG_GPL 0`, `CONFIG_NONFREE 0`, `CONFIG_VERSION3 0`, `CONFIG_GPLV3 0`,
`CONFIG_LGPLV3 0`. Our FFmpeg is **LGPL 2.1 or later** and nothing else.
FFmpeg's GPL parts (some x86 assembly, a batch of libavfilter filters,
build/test tooling) are either disabled outright — we pass
`--disable-avfilter` — or never enabled.

**The LGPL does not require radiod to change licence.** §6 permits
linking and distributing the combined work *"under terms of your choice,
provided that the terms permit modification of the work for the
customer's own use and reverse engineering for debugging such
modifications."* MIT clears that bar trivially.

What *does* change is which §6 sub-clause we satisfy. Dynamically
linking Debian's shared libraries is §6(b), "use a suitable shared
library mechanism", satisfied automatically with Debian carrying the
source-distribution burden. Static linking takes 6(b) off the table.
Distributing via GitHub releases fits **§6(d)** — offering access to
copy from a designated place — so we offer equivalent access to the
FFmpeg source. Because `build-ffmpeg.sh` pins an exact tag with a
sha256 and records the configure line, and radiod's source is public, a
user can rebuild and relink.

Note that `libssl3t64` stays a **dynamically linked** system library, so
OpenSSL raises no equivalent obligation.

### Checklist

- [ ] `LICENSE` (MIT) at the repo root, and `license = "MIT"` in
      `service/Cargo.toml`. **This is a precondition, not a tidy-up:**
      the repo currently grants no rights at all, and §6 requires that
      our distribution terms *permit* modification for the customer's own
      use. Landing separately, ahead of this stack.
- [ ] `NOTICE.md` recording third-party material — mirroring
      openairplay2's pattern — naming FFmpeg, its version and tag, the
      configure line, and its LGPL 2.1+ status.
- [ ] The .deb ships a real `/usr/share/doc/radiod/copyright`: today
      `cargo-deb` is given only `copyright = "2026 Stefan Arentz"`. It
      must carry the MIT text, a **prominent notice that FFmpeg is
      statically linked and covered by the LGPL**, and a copy of
      `COPYING.LGPLv2.1`. Both the notice and supplying the licence text
      are unconditional §6 obligations, independent of sub-clause.
- [ ] A pointer to the exact FFmpeg source (upstream tag + sha256 +
      `build-ffmpeg.sh`) in the copyright file, satisfying §6(d).
- [ ] **`build-ffmpeg.sh` asserts `CONFIG_GPL 0` after configure** and
      fails the build otherwise. The licence is one `--enable-gpl` or
      `--enable-libx264` away from flipping to GPL v2+, which *would*
      force radiod to become GPL. Making it a build-time invariant stops
      that arriving silently with a future codec addition.

Also in the tree, unchanged by this work and non-blocking: `ffmpeg-next`
and `ffmpeg-sys-next` are WTFPL; `symphonia` (already linked via
`openairplay2`) is MPL-2.0, whose file-level copyleft obliges publishing
changes to *its* files but does not reach radiod's own code.

None of this is legal advice, but LGPL 2.1 §6 is specific and the shape
is well-trodden.

## Phases

Each phase is a PR stacked on this plan.

### Phase 0 — decline Recommends

Documentation only, and independent of everything below: record
`--no-install-recommends` in `notes/clean-install.md`, correct that
note's "the closure is structural" claim, and give the commands to audit
muzak. If muzak was installed with apt's defaults it is carrying a
Vulkan driver and LLVM right now, and reclaiming that is a larger win
than the rest of this plan combined. **32 packages / 254 MB, no code
change.**

### Phase 1 — build a minimal FFmpeg and link it statically (native)

New `service/build-ffmpeg.sh`: fetch the pinned, sha256-checked FFmpeg
tarball, run the configure line above plus `--enable-openssl` and
`--enable-protocol=https,tls`, assert `CONFIG_GPL 0` in the generated
`config.h` (failing the build otherwise — see the licensing checklist),
then `make install` into `service/target/ffmpeg/<triple>`.

`build-deb.sh` exports `FFMPEG_DIR` before `cargo deb`; `Cargo.toml`
gains the `static` feature on the .deb path and drops `depends` to
`libasound2t64, libc6, libssl3t64, ca-certificates`.

`setup-build.sh` **swaps** the four `libav*-dev` packages for
`libssl-dev` — FFmpeg needs OpenSSL's headers at build time to honour
`--enable-openssl`, and the cross list needs `libssl-dev:arm64` and
`libssl-dev:armhf` alongside it in phase 2.

No changes to `pipeline.rs`. Adds `notes/dependencies.md` recording the
measurement method so this is re-checkable at each Debian release.
**Result: 144 → 12 packages.**

### Phase 2 — cross builds and CI

Extend `build-ffmpeg.sh` with `--enable-cross-compile --arch --target-os
--cross-prefix` for arm64 and armhf, reusing the toolchains
`setup-build.sh` already installs. Cache the built FFmpeg in CI keyed on
(pinned version, triple, configure flags) so the ~1 minute build is not
paid on every release. All three .debs build and are verified with
`dpkg -I` and `readelf -h`.

### Phase 3 — licensing paperwork, cleanup and soak

`NOTICE.md` and the .deb copyright file from the licensing checklist
above — the `LICENSE` file lands separately ahead of this stack, and the
`CONFIG_GPL` guard ships with `build-ffmpeg.sh` in phase 1. Plus
`service/README.md`, and a soak on muzak before the stack merges.

*(An earlier draft had a phase here that moved HTTPS into Rust to reach
5 packages. Declined — see "The TLS decision".)*

## Acceptance criteria

- `dpkg -I radiod_*.deb` shows `Depends: libasound2t64, libc6,
  libssl3t64, ca-certificates`.
- The apt simulation from an empty status resolves to **12 packages**,
  down from 144 — and no longer reaches libva/mesa/LLVM under
  Recommends.
- `readelf -d` on the shipped binary shows no `libav*` `DT_NEEDED`
  entries — the only shared libraries are libc, libm, libasound and the
  OpenSSL pair.
- The licensing checklist above is complete, and `build-ffmpeg.sh` fails
  if `CONFIG_GPL` is ever non-zero.
- muzak plays MP3 and HE-AAC streams (including a hand-entered non-SomaFM
  URL), shows ICY title changes on the website, and survives a
  deliberate network drop by reconnecting.
- An AirPlay session still preempts the radio and resumes it afterwards,
  unchanged.
- The FFmpeg build is reproducible: pinned tag, recorded sha256, and the
  configure line under review in the repo.
- No `-march=native` reaches any shipped artifact.
- The mixer ceiling and the `volume::gain()` clamp are untouched: the
  source still produces packed s16 and the player still applies gain to
  every buffer it returns.
- `cargo test`, `cargo clippy`, `cargo fmt` green on macOS (Homebrew
  FFmpeg, unchanged) and on Linux.

## Risks and open questions

- **Licensing is a precondition, not a follow-up** — see the checklist
  above. The `LICENSE` file must exist before we ship a statically
  linked LGPL library, and the .deb copyright file must carry the notice
  and licence text.
- **muzak's actual installed state is unconfirmed** (phase 0).
- **OpenSSL is a moving dependency.** `libssl3t64` is Debian's, so it
  gets security updates for free — but a future Debian release renaming
  the package (as the `t64` transition already did once) will need the
  `depends` line revisited. `notes/dependencies.md` should say so.
- **The codec set is a judgement call.** It covers what internet radio
  actually uses, but "any stream URL" has no closed definition; adding a
  decoder later is a one-line configure change and a rebuild, which is
  the point of owning the build.
- **CI build time**: ~1 minute per native architecture, more when
  cross-compiling; mitigated by caching.
- **Binary size** grows by ~2.6 MB, recorded here so the trade is
  explicit against 156 MB of packages.
