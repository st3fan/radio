# radiod

The Radio daemon: an HTTP REST API on `127.0.0.1` that plays an internet
radio stream (from a `.pls` playlist URL) to an audio sink. Streaming,
demuxing and decoding are done in-process by the FFmpeg libraries via the
`ffmpeg-next` crate.

See `../notes/plan.md` for the overall design and `../CLAUDE.md` for the
ways of working (including the max-volume safety invariant).

## Build prerequisites

The `ffmpeg-sys-next` build script needs the FFmpeg headers/libraries,
`pkg-config`, and libclang (for bindgen).

macOS (primary development machine):

```
brew install ffmpeg pkg-config
```

(libclang comes with the Xcode command line tools.)

Debian (integration testing, real audio) — two steps, because radiod does
**not** use Debian's libavcodec. It links its own minimal FFmpeg instead,
which keeps ~140 packages off the radio (see `notes/dependencies.md`):

```
./setup-build.sh          # system packages: clang, pkg-config, libssl-dev, libasound2-dev
./build-ffmpeg.sh <arch>  # builds the pinned minimal FFmpeg into target/ffmpeg/
```

`<arch>` is `amd64`, `arm64` or `armhf`. **The second step is not
optional**: without it `cargo build` fails with `unable to find library
-lavcodec`. It only needs re-running when `build-ffmpeg.sh` itself
changes — a stamp file makes it a no-op otherwise.

You do not need to set `PKG_CONFIG_PATH`. `.cargo/config.toml` points it
at `target/ffmpeg/host`, a symlink `build-ffmpeg.sh` maintains to the
native build, and it defers to the environment if you have already set
one. macOS is unaffected: that directory does not exist there, so
pkg-config finds Homebrew's FFmpeg as before.

## Running

```
cargo run -- --sink wav:/tmp/out.wav      # macOS dev: listen by playing the WAV
cargo run -- --config radio.toml          # explicit config file
cargo run                                 # Linux: plays to the configured ALSA device
```

Flags: `--config <path>` (default `/etc/radio/config.toml`, optional),
`--sink alsa|null|wav:<path>` (default `alsa` on Linux, `null` elsewhere),
`-v`/`--version`.

On the Debian PC, find the right ALSA device with `aplay -l` and set
`audio_device` accordingly (`plughw:<card>,<device>` — the `plughw` prefix
lets ALSA convert sample rates/formats the hardware does not do natively).

Config file (all fields optional):

```toml
listen = "127.0.0.1:8080"     # must be loopback
audio_device = "plughw:1,0"   # ALSA device
max_volume = 50               # hard cap; the volume can never exceed this
initial_volume = 50           # percentage of max_volume (50 = half the cap)
```

## API

Full reference: `../notes/api.md`. Quick start:

```
curl http://127.0.0.1:8080/status
curl -X POST http://127.0.0.1:8080/play -d '{"playlist_url": "https://somafm.com/defcon.pls"}'
curl -X POST http://127.0.0.1:8080/stop
curl -X POST http://127.0.0.1:8080/pause
curl -X POST http://127.0.0.1:8080/resume
curl -X POST http://127.0.0.1:8080/volume -d '{"volume": 30}'
curl -X POST http://127.0.0.1:8080/mute
curl -X POST http://127.0.0.1:8080/unmute
```

Volume requests take 0–100 as a **percentage of `max_volume`**: 100 means
"as loud as the cap allows", never louder. With `max_volume = 30`, a request
of 100 yields an effective device volume of 30. Responses and `/status`
always show the effective value.

## Building .deb packages (amd64, arm64, armhf)

On Debian 13 — the build box or a `debian:trixie` CI container:

```
./setup-build.sh cross          # once; "cross" adds the arm64 + armhf
                                # multiarch toolchains (omit for native-only)
cargo install cargo-deb         # once, per user

./build-deb.sh amd64            # native -> target/debian/
./build-deb.sh arm64            # multiarch cross on the amd64 box; native in CI
./build-deb.sh armhf            # multiarch cross (ARMv7 — the Banana Pi)
```

The cross builds are plain Debian multiarch: the foreign-arch dev
packages co-install next to the native ones (`Multi-Arch: same`), the
`pkgconf:<arch>` wrappers serve the right `.pc` paths, and
`build-deb.sh` exports the per-target linker/pkg-config/bindgen
variables. No Docker, no emulation, no sysroot directory. The GitHub
release workflow (see `.github/workflows/`) runs the same script.

Note on armhf: this is **Debian's ARMv7 port** — it does not run on the
ARMv6 Raspbian world (Pi Zero W, Pi 1), and since dpkg cannot tell
those two "armhf"s apart, the package carries a preinst guard that
refuses pre-ARMv7 hardware. The old ARMv6 build path was removed when
the Pi Zero W was retired (history: `plans/20260801-12-*.md` and
`git log -- service/build-pi.sh`).

## Checks

Run before considering any change done:

```
cargo test && cargo clippy --all-targets && cargo fmt --check
```
