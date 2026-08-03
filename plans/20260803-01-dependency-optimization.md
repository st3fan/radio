# Plan: dependency optimization — a statically linked, minimal FFmpeg

- **Date:** 2026-08-03
- **Status:** rewritten after measurement — the recommendation changed
  (was "rewrite the pipeline in pure Rust", is now "keep FFmpeg, build it
  ourselves, minimally, and link it statically")
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

The 62 packages that only Recommends adds are the ones worth staring
at: `libva2` recommends the VA-API driver stack, which brings
**`mesa-vulkan-drivers` (70 MB)**, **`libllvm19` (118 MB)**,
`mesa-libgallium` (34 MB) and `libz3-4` (26 MB). An internet radio with
no display and no GPU can end up carrying a Vulkan driver and a full
LLVM, because its audio decoder is linked against a library that can
also do hardware-accelerated video. Whether muzak actually has these
depends on how the .deb was installed; **confirming that on muzak is
still an open task** (it was not reachable by hostname from the machine
this was measured on).

### Where the 156 MB comes from

Two chains explain nearly all of it, and neither has anything to do with
audio:

```
libavcodec61 ──► libx265, libaom3, libsvtav1enc2, librav1e0.7, libvpx9,
                 libdav1d7, libjxl0.11, libcodec2-1.2, libtheora*, libxvidcore4,
                 libwebp*, libopenjp2-7        15 pkgs / 34 MB of video+image codecs
             └─► librsvg2-2 ──► libpangocairo ──► libpango ──► fontconfig
                              └─► libcairo2, libgdk-pixbuf, libglib2.0-0
                                                 33 pkgs / 33 MB of desktop graphics
libavformat61 ─► libbluray2 ──► libfontconfig1, libfreetype6
              └─► libdvdnav4, libdvdread8t64, libzmq5, librabbitmq4, libsrt1.5
```

The single largest package is `libcodec2-1.2` at 16 MB — a ham-radio
speech codec. `libavcodec61` itself is 14 MB; `libx265-215` is 9 MB.
Debian ships `libavcodec61` as one monolithic package, so **there is no
way to depend on "just the audio decoders"**. The only lever is to stop
depending on Debian's build of it.

(An earlier informal look at this suggested ~120 MB of *fonts*. That was
an artifact of `apt-cache depends --recurse` walking every branch of an
alternatives group. Resolved properly, `fontconfig-config` picks one
font package: 2 packages / 3.5 MB. The graphics stack is still there —
it is just cairo/pango/glib rather than fonts.)

## What radiod actually uses FFmpeg for

All of it lives in one file, `service/src/pipeline.rs` (216 lines),
behind the `Source` trait that `service/src/source.rs` already defines
and that the player already takes as an injected `SourceFactory`. The
four jobs:

1. **HTTP(S) fetch with reconnect** — `input_with_dictionary` with
   `reconnect`/`reconnect_streamed`/`reconnect_delay_max` options.
2. **ICY metadata** — read off the http context via `av_opt_get` on
   `icy_metadata_headers` / `icy_metadata_packet`. The *parsing* is
   already ours: `service/src/icy.rs` has no FFmpeg dependency.
3. **Demux + decode** — MP3 and AAC in practice (SomaFM).
4. **Sample format conversion** — and note what the resampler is
   actually configured to do at `pipeline.rs:110-117`: input layout and
   rate are passed as *both* the source and destination. It is not
   resampling. It converts the decoder's native layout to packed s16.

That last point was confirmed empirically: FFmpeg's fixed-point MP3
decoder emits `s16p` and its float decoder emits `fltp`, and which one
you get depends on how FFmpeg was configured. The conversion step is
load-bearing and must stay whatever else changes.

## The finding that reframes this plan

FFmpeg does not have to come from Debian. `ffmpeg-sys-next` (which
`ffmpeg-next` sits on) supports linking a **prebuilt FFmpeg** via the
`FFMPEG_DIR` environment variable, and `ffmpeg-next` exposes a `static`
feature (`static = ["ffmpeg-sys-next/static"]`) that turns the emitted
link directives into `cargo:rustc-link-lib=static=avcodec` and friends.
`FFMPEG_DIR` expects exactly the `lib/` + `include/` layout that
`make install --prefix` produces.

So we can configure FFmpeg ourselves, with only the pieces a radio
needs, and link it into the binary. Measured on the Pi against FFmpeg
8.1:

```sh
./configure --disable-everything --disable-autodetect \
  --disable-programs --disable-doc --disable-shared --enable-static --disable-debug \
  --disable-avdevice --disable-avfilter --disable-swscale --enable-swresample \
  --enable-decoder=mp3,aac,aac_latm,aac_fixed,flac,vorbis \
  --enable-demuxer=mp3,aac,mov,ogg,flac \
  --enable-parser=mpegaudio,aac,aac_latm,flac,vorbis \
  --enable-protocol=file,http,tcp
```

| | static archives | linked test binary | build time (Pi, 4 cores) | runtime deps |
|---|---|---|---|---|
| minimal, the configure line above | **3.5 MB** | **2.11 MB** | **51 s** | libc, libm |
| full-featured static (see Option C) | 26.0 MB | 18.48 MB | 10 m 27 s | libc, libm |
| today, Debian's shared libraries | — | — | — | 144 pkgs / 181 MB |

Configure reports **LGPL version 2.1 or later** — no GPL contamination
with this option set. A small C harness linked against the minimal
archives decoded a real Groove Salad capture correctly (mp3, 44100 Hz,
stereo, 18.76 seconds from a 300 KB sample) in a 2.11 MB binary whose
entire `ldd` output is `libc` and `libm`.

**156 MB of packages becomes about 2 MB inside the binary.**

## Options

**Option A — build a minimal FFmpeg ourselves and link it statically.**
*Recommended.* Keeps FFmpeg's proven decoders and its network layer;
`pipeline.rs` needs no changes at all in the first phase. Costs: we own
an FFmpeg build script and a per-architecture build step, and we have to
decide how HTTPS gets done (below). Numbers as measured above.

**Option B — replace the pipeline with a pure-Rust source.** ureq (a
dependency already) plus symphonia (already in `Cargo.lock` via
`openairplay2`) behind the existing `Source` trait, with ICY de-framing
written against `icy.rs`. This was the previous recommendation and it is
still the only route to a *zero* extra dependency, no-C-build outcome.
Against it: symphonia's AAC support is weakest exactly where SomaFM may
need it (HE-AAC v2, SBR+PS), reconnect-with-backoff becomes our code
rather than libavformat's, and pure-Rust MP3 decode on ARMv7 is an
unmeasured CPU risk. Option A eliminates all three risks for about 2 MB
of binary. Keep this in reserve.

**Option C — the `build` feature of `ffmpeg-sys-next`.** This is the
`sdl2 = { features = ["bundled"] }` analogue and it does work, including
cross-compilation (it passes `--arch`, `--target-os` and derives
`--cross-prefix` from the `cc` crate, which matches the `CC_<triple>`
variables `build-deb.sh` already exports). It also passes
`--disable-autodetect`, which by itself removes every external library —
the 34 MB of video codecs and 33 MB of graphics stack vanish by
construction. **Rejected anyway**, for three reasons found by reading
`build.rs`:

- It never passes `--disable-everything`, and there is no environment
  variable for injecting extra configure flags. So it builds *every*
  native FFmpeg decoder, demuxer, muxer and encoder: 26 MB of archives,
  an 18.5 MB binary, 10.5 minutes per architecture.
- It fetches with `git clone --depth=1 -b release/8.1` at build time — a
  229 MB download per build, against a **moving branch** rather than a
  pinned tag, so builds are not reproducible over time.
- On the native path it adds `--extra-cflags=-march=native -mtune=native`.
  Our release workflow builds **arm64 natively on `ubuntu-24.04-arm`**
  (a Neoverse-class server CPU) and ships that .deb to muzak's
  Cortex-A72. Tuning FFmpeg's hand-written assembly to the build
  runner's CPU and shipping it to an older core risks SIGILL on the
  radio.

`FFMPEG_DIR` avoids all three by giving us the configure line.

**Option D — trim the declared Depends.** Doesn't work: Debian's
`libavcodec61` is monolithic and all four libav\* names are genuinely
linked. No lever here.

## Design of the recommended route

### The HTTPS problem, and why it splits into two phases

SomaFM's streams are https (`https://ice2.somafm.com/groovesalad-128-mp3`).
With `--disable-autodetect` the minimal build has only `file, http, tcp`
— FFmpeg needs an external library for TLS. Confirmed from FFmpeg's
configure (line 7493) that OpenSSL 3.x only demands `--enable-version3`
when GPL is enabled, so `--enable-openssl` is clean for our LGPL build.

That gives two end states, and they make a natural phase boundary:

| end state | packages | size |
|---|---|---|
| today | 144 | 181 MB |
| **static FFmpeg + system OpenSSL + ca-certificates** | **12** | **37 MB** |
| **static FFmpeg + HTTPS done in Rust** | **5** | **25 MB** |

The first is reachable with essentially no Rust changes and already
removes 132 packages. The second removes the last three and is worth
doing on its own merits, because it also deletes the `av_opt_get` ICY
hack: ureq already brings rustls *and* `webpki-roots`, so the CA bundle
is in the binary and no `ca-certificates` package is needed.

Doing HTTPS in Rust means feeding libavformat through a custom
`AVIOContext`. **`ffmpeg-next` has no wrapper for this** — no
`AVIOContext`, no `avio_alloc_context` — so it is FFI we write against
`ffmpeg-sys`. That is a real cost and the riskiest code in this plan
(callbacks crossing FFI must not panic), which is exactly why it is a
separate, later phase rather than part of the first win.

### macOS must not change

`CLAUDE.md` requires `cargo test`/`clippy`/`fmt` to stay green on macOS,
where FFmpeg comes from Homebrew via pkg-config. `FFMPEG_DIR` and the
`static` feature are therefore **set only by the .deb build path**, not
in `Cargo.toml`'s default features. Day-to-day Mac development keeps
using the system FFmpeg exactly as today.

### Pinning

Fetch a released FFmpeg tarball by tag with a recorded sha256 — never a
branch. The version and the full configure line live in the build script
so the build is reproducible and reviewable.

## Phases

Each phase is a PR stacked on this plan.

### Phase 1 — build a minimal FFmpeg and link it statically (native)

New `service/build-ffmpeg.sh`: fetch the pinned, sha256-checked FFmpeg
tarball, run the configure line above plus `--enable-openssl` and
`--enable-protocol=https,tls`, `make install` into
`service/target/ffmpeg/<triple>`. `build-deb.sh` exports `FFMPEG_DIR`
before `cargo deb`; `Cargo.toml` gains the `static` feature on the .deb
path and drops `depends` to `libasound2t64, libc6, libssl3t64,
ca-certificates`. `setup-build.sh` loses the four `libav*-dev` packages.
No changes to `pipeline.rs`. Also adds `notes/dependencies.md` recording
the measurement method so this is re-checkable at each Debian release,
and confirms the Recommends question on muzak. **Result: 144 → 12
packages.**

### Phase 2 — cross builds and CI

Extend `build-ffmpeg.sh` with `--enable-cross-compile --arch --target-os
--cross-prefix` for arm64 and armhf, reusing the toolchains
`setup-build.sh` already installs. Cache the built FFmpeg in CI keyed on
(pinned version, triple, configure flags) so the ~1 minute native build
is not paid on every release. All three .debs build and are verified
with `dpkg -I` and `readelf -h`.

### Phase 3 — do HTTPS in Rust, drop the TLS dependency

Fetch with ureq, de-frame `icy-metaint` into `icy.rs`, and feed
libavformat through a custom `AVIOContext`. Deletes `format_option` and
the `av_opt_get` ICY hack. Reconnect-with-backoff moves to our code.
`Depends` becomes `libasound2t64, libc6`. **Result: 12 → 5 packages.**

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
- muzak plays every configured SomaFM stream (MP3 and AAC, including any
  HE-AAC channel), shows ICY title changes on the website, and survives a
  deliberate network drop by reconnecting.
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
  easy to satisfy in substance, but it should be made explicit: add a
  `LICENSE`, and state the FFmpeg version, configure line and license in
  the .deb's copyright file. This is the one item that is a genuine
  obligation rather than a preference.
- **Custom AVIO FFI (phase 3) is the riskiest code here.** A panic
  crossing the FFI boundary is undefined behaviour; the callbacks must
  catch and convert. This is why it is isolated in its own phase behind
  an already-shipped win.
- **CI build time.** ~1 minute per native architecture, more when
  cross-compiling; mitigated by caching, but it is new wall-clock on
  every release.
- **Which streams must work?** Everything in muzak's config. FFmpeg's
  AAC decoder handles HE-AAC v2 properly, which is the main reason
  Option A de-risks against Option B — but the actual channel list should
  still be confirmed before phase 1.
- ~~Does the AirPlay path share anything?~~ Settled: it does not.
  `airplay.rs` never mentions `ffmpeg` or `pipeline.rs`
  (`openairplay2` hands us PCM), and `FfmpegSource` is referenced only
  by `main.rs`, where it is wired in as the `SourceFactory`.
- **Binary size** grows by ~2 MB. Recorded here so the trade is explicit;
  it is overwhelmingly favourable against 156 MB of packages.
