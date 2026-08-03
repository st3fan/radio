# Plan: dependency optimization — getting FFmpeg off the radio

- **Date:** 2026-08-03
- **Status:** proposed — awaiting review
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
depends on how the .deb was installed; **confirming that on muzak is the
first task of phase 1.**

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
linking against it.

(An earlier informal look at this suggested ~120 MB of *fonts*. That was
an artifact of `apt-cache depends --recurse` walking every branch of an
alternatives group. Resolved properly, `fontconfig-config` picks one
font package: 2 packages / 3.5 MB. The graphics stack is still there —
it is just cairo/pango/glib rather than fonts.)

## What radiod actually uses FFmpeg for

All of it lives in one file, `service/src/pipeline.rs` (216 lines),
behind the `Source` trait that `service/src/source.rs` already defines
and that the player already takes as an injected `SourceFactory`. That
seam is why this is worth attempting at all. The four jobs:

1. **HTTP(S) fetch with reconnect** — `input_with_dictionary` with
   `reconnect`/`reconnect_streamed`/`reconnect_delay_max` options.
2. **ICY metadata** — read off the http context via `av_opt_get` on
   `icy_metadata_headers` / `icy_metadata_packet`. The *parsing* is
   already ours: `service/src/icy.rs` has no FFmpeg dependency.
3. **Demux + decode** — MP3 and AAC in practice (SomaFM).
4. **Sample format conversion** — and note what the resampler is
   actually configured to do at `pipeline.rs:110-117`: input layout and
   rate are passed as *both* the source and destination. It is not
   resampling. It converts planar float to packed s16 and nothing else.

So `libswresample` is doing a job worth about thirty lines of Rust, and
`libavcodec` is a 14 MB monolith standing in for two audio decoders.

## Options

**Option A — replace the FFmpeg pipeline with a pure-Rust source.**
Depends collapses to `libasound2t64, libc6`; 139 packages and 156 MB go
away, along with the whole Recommends cliff. The pieces are largely
already in the tree:

- HTTP(S) — `ureq` is already a dependency and already brings rustls.
  ICY needs the `Icy-MetaData: 1` request header and de-framing the
  `icy-metaint` byte counter out of the body: a small, well-specified
  loop feeding `icy.rs`, which already parses the strings.
- Decode — **symphonia is already in `Cargo.lock`** (`symphonia-core`,
  `symphonia-codec-aac`, `symphonia-metadata`), pulled in by
  `openairplay2`. Adding `symphonia-codec-mp3` plus the mp3/adts readers
  is additive, pure Rust, and does not touch the AirPlay path.
- Format conversion — hand-written, and easier to unit-test than the
  swresample call it replaces.

Risks, in the order I expect them to bite: symphonia's robustness on a
live never-ending icecast stream (mid-stream corruption, servers that
change bitrate); reconnect-with-backoff becoming our code rather than
libavformat's; and CPU cost on ARM, since symphonia's MP3 decoder is
pure Rust against FFmpeg's hand-tuned assembly. A Pi 4 has headroom to
spare, an ARMv7 Banana Pi may not — that is a measurement, not a guess,
and it gates the decision.

**Option B — link a minimal FFmpeg statically.** `--disable-everything
--enable-decoder=mp3,aac ...` gives a few-hundred-KB libav with no
runtime deps, and keeps FFmpeg's proven decoders and network layer. But
it means building FFmpeg for three architectures in CI and on the build
box, which is exactly the cross-compilation complexity that
`plans/20260801-12-cross-compilation.md` deliberately designed away, plus
LGPL static-linking obligations to get right. Better decoders, much
worse build story.

**Option C — trim the declared Depends.** Doesn't work: Debian's
`libavcodec61` is monolithic, and all four libav\* names are genuinely
linked. No lever here.

**Recommendation: Option A**, with Option B held as the fallback if
phase 2 shows symphonia can't keep up or can't stay stable on a live
stream. The FFmpeg pipeline stays in the tree behind a cargo feature
until Option A has run on the radio for a while, so the fallback is a
rebuild rather than a revert.

## Phases

Each phase is a PR stacked on this plan.

### Phase 1 — measure, and prove the ceiling for the decision

No production code. Adds `notes/dependencies.md` recording the
measurement method and the numbers above, so this is re-checkable at
each Debian release. Confirms on muzak what is *actually* installed (the
Recommends question above — `dpkg -l | grep -c '^ii'`, and whether
`libllvm19`/`mesa-vulkan-drivers` are present). Then the gating
measurement: a throwaway binary that decodes the real SomaFM MP3 and AAC
streams with symphonia and reports CPU per wall-clock second on this
Pi. If symphonia can't decode either stream, or costs more than roughly
a third of one ARMv7 core, we stop and take Option B.

### Phase 2 — the pure-Rust source

`HttpSource` implementing `Source`: ureq fetch, ICY de-framing into
`icy.rs`, symphonia demux+decode, s16 conversion, reconnect with
backoff. Selected by a cargo feature, defaulting to the existing FFmpeg
path so this PR changes no behaviour. Tests cover ICY de-framing against
recorded stream bytes (the framing is where the fiddly off-by-ones live)
and the format conversion.

### Phase 3 — flip the default and drop the dependency

Make the pure-Rust source the default, put `FfmpegSource` behind an
off-by-default feature, and cut `Cargo.toml`'s `depends` down to
`libasound2t64, libc6`. `service/setup-build.sh` loses the four
`libav*-dev` packages, native and cross — which also makes CI and the
cross build meaningfully simpler, since bindgen no longer needs to parse
FFmpeg's headers for the target architecture. Soak on muzak before
merge.

### Phase 4 — remove the FFmpeg path

Only after phase 3 has run on the radio for a stretch: delete
`pipeline.rs`, the `ffmpeg-next` dependency and the feature flag. Split
out so it is a separate, revertible decision.

## Acceptance criteria

- `dpkg -I radiod_*.deb` shows `Depends: libasound2t64, libc6`.
- The apt simulation from an empty status resolves to **5 packages**,
  down from 144 — and no longer reaches libva/mesa/LLVM under
  Recommends.
- muzak plays every configured SomaFM stream (MP3 and AAC), shows ICY
  title changes on the website, and survives a deliberate network drop
  by reconnecting.
- Decode CPU on the target board is measured and recorded, not assumed.
- The mixer ceiling and the `volume::gain()` clamp are untouched: the
  new source produces s16 exactly like the old one and the player still
  applies gain to every buffer it returns.
- `cargo test`, `cargo clippy`, `cargo fmt` green on macOS and Linux.

## Open questions

- **Which streams must work?** Everything in muzak's config, plus
  whatever Stefan might add later. AAC+ (HE-AAC v2 / SBR+PS) is the one
  to check specifically — symphonia's AAC support is weakest there, and
  some SomaFM AAC streams use it.
- ~~Does the AirPlay path share anything?~~ Settled while writing this:
  it does not. `airplay.rs` never mentions `ffmpeg` or `pipeline.rs`
  (`openairplay2` hands us PCM), and `FfmpegSource` is referenced only
  by `main.rs`, where it is wired in as the `SourceFactory`. Swapping the
  radio source leaves AirPlay alone.
- **Binary size.** Static symphonia decoders grow `radiod` itself. The
  trade is overwhelmingly favourable, but phase 3 should record the
  before/after so the claim is honest.
