# Plan: dependency optimization — a statically linked, minimal FFmpeg

- **Date:** 2026-08-03
- **Status:** rewritten twice after measurement. Stefan settled the open
  product question: radiod must **play any stream URL**, not just the
  SomaFM channels its own website lists. That decides the route — keep
  FFmpeg, build it ourselves, minimally, and link it statically.
- **Goal:** shrink what `radiod` drags onto the radio. Today the package
  pulls **139 extra packages / 156 MB** past a base Debian install, none
  of which a radio needs. The target is a `radiod` whose only runtime
  dependencies are `libasound2t64` and `libc6`.

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

The mesa/LLVM cliff arrives through Recommends, not Depends:
`libavcodec61` depends on `libva2`, `libva2` recommends `va-driver-all`,
which depends on `mesa-va-drivers` and drags in **`mesa-vulkan-drivers`
(70 MB)** and **`libllvm19` (118 MB)**. Recommends are optional by
definition, so `--no-install-recommends` declines **62 packages /
295 MB** with no code change at all. A .deb cannot dictate how apt
treats its dependencies' Recommends, so this belongs in the install
procedure (`notes/clean-install.md`) — see phase 0. **Confirming what
muzak actually has installed is still outstanding**; it was not
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

### The HTTPS problem, and why it splits the work

Stream URLs are https (`https://ice2.somafm.com/...`). With
`--disable-autodetect` the build has no TLS backend — the protocol list
above is deliberately `file, http, tcp, hls, crypto`. FFmpeg needs an
external library for TLS. Confirmed from FFmpeg's configure (line 7493)
that OpenSSL 3.x only demands `--enable-version3` when GPL is enabled,
so `--enable-openssl` is clean for our LGPL build.

That gives two end states, and they make a natural phase boundary:

| end state | packages | size |
|---|---|---|
| today | 144 | 181 MB |
| **static FFmpeg + system OpenSSL + ca-certificates** | **12** | **37 MB** |
| **static FFmpeg + HTTPS done in Rust** | **5** | **25 MB** |

The first is reachable with no Rust changes and already removes 132
packages. The second removes the last three and is worth doing on its
own merits, because it also deletes the `av_opt_get` ICY hack: ureq
already brings rustls *and* `webpki-roots`, so the CA bundle is in the
binary and no `ca-certificates` package is needed.

Doing HTTPS in Rust means feeding libavformat through a custom
`AVIOContext`. **`ffmpeg-next` has no wrapper for this** — no
`AVIOContext`, no `avio_alloc_context` — so it is FFI we write against
`ffmpeg-sys`. That is the riskiest code in this plan (callbacks crossing
FFI must not panic), which is why it is a separate, later phase rather
than part of the first win. It is also genuinely optional: at 12
packages we have already captured 132 of the 139.

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

## Phases

Each phase is a PR stacked on this plan.

### Phase 0 — decline Recommends

Documentation only, and independent of everything below: record
`--no-install-recommends` in `notes/clean-install.md`, and check what
muzak actually has. If it was installed with apt's defaults it is
carrying a Vulkan driver and LLVM right now, and reclaiming that is a
larger win than the rest of this plan combined. **62 packages / 295 MB,
no code change.**

### Phase 1 — build a minimal FFmpeg and link it statically (native)

New `service/build-ffmpeg.sh`: fetch the pinned, sha256-checked FFmpeg
tarball, run the configure line above plus `--enable-openssl` and
`--enable-protocol=https,tls`, `make install` into
`service/target/ffmpeg/<triple>`. `build-deb.sh` exports `FFMPEG_DIR`
before `cargo deb`; `Cargo.toml` gains the `static` feature on the .deb
path and drops `depends` to `libasound2t64, libc6, libssl3t64,
ca-certificates`. `setup-build.sh` loses the four `libav*-dev` packages.
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

### Phase 3 — do HTTPS in Rust, drop the TLS dependency

Fetch with ureq, de-frame `icy-metaint` into `icy.rs`, and feed
libavformat through a custom `AVIOContext`. Deletes `format_option` and
the `av_opt_get` ICY hack; reconnect-with-backoff moves to our code.
`Depends` becomes `libasound2t64, libc6`. **Result: 12 → 5 packages.**
Optional — evaluate after phase 1 lands.

### Phase 4 — cleanup and soak

Documentation, a `LICENSE` file (see risks), `service/README.md`, and a
soak on muzak before the stack merges.

## Acceptance criteria

- `dpkg -I radiod_*.deb` shows `Depends: libasound2t64, libc6` after
  phase 3 (`libasound2t64, libc6, libssl3t64, ca-certificates` after
  phase 1).
- The apt simulation from an empty status resolves to **5 packages**,
  down from 144 — and no longer reaches libva/mesa/LLVM under
  Recommends.
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

- **The repo has no `LICENSE` file.** Statically linking LGPL 2.1 code
  carries a relinking obligation. The repo is public, which makes this
  easy to satisfy in substance, but it should be explicit: add a
  `LICENSE`, and state the FFmpeg version, configure line and licence in
  the .deb's copyright file. This is a genuine obligation rather than a
  preference.
- **Custom AVIO FFI (phase 3) is the riskiest code here.** A panic
  crossing the FFI boundary is undefined behaviour; the callbacks must
  catch and convert.
- **muzak's actual installed state is unconfirmed** (phase 0).
- **The codec set is a judgement call.** It covers what internet radio
  actually uses, but "any stream URL" has no closed definition; adding a
  decoder later is a one-line configure change and a rebuild, which is
  the point of owning the build.
- **CI build time**: ~1 minute per native architecture, more when
  cross-compiling; mitigated by caching.
- **Binary size** grows by ~2.6 MB, recorded here so the trade is
  explicit against 156 MB of packages.
