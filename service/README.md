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

Debian (integration testing, real audio):

```
apt install clang pkg-config libavformat-dev libavcodec-dev \
    libavutil-dev libswresample-dev libasound2-dev
```

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

## Cross-compiling for the Pi Zero W

The Pi Zero W 1 is ARMv6 (`arm-unknown-linux-gnueabihf`) — stock Debian
armhf binaries are ARMv7 and will not run on it, and building on the Zero
itself is a non-starter. Builds happen on the Debian PC against a sysroot
copied from the Pi, so the linked libav*/libasound sonames match the Pi's
exactly:

```
# once: rustup target add arm-unknown-linux-gnueabihf
#       sudo apt install gcc-arm-linux-gnueabihf clang rsync
#       (on the Pi) sudo apt install libavformat-dev libavcodec-dev \
#           libavutil-dev libswresample-dev libasound2-dev libgcc-14-dev

./build-pi.sh sync <pi-host>    # copy the sysroot (repeat after Pi upgrades)
./build-pi.sh build             # target/arm-unknown-linux-gnueabihf/release/radiod
```

Gotchas the script handles (each discovered the hard way against a real
Pi Zero running Raspbian trixie):

- Debian's cross gcc emits ARMv7 code by default, so C compiled by build
  scripts gets `-march=armv6 -mfpu=vfp` forced to match the Rust target.
- The cross gcc's *companion* crt/libgcc objects are ARMv7/Thumb-2 and
  SIGILL on the Zero before `main()`; linking uses the Pi's own (`-B`
  prefixes into the sysroot, hence `libgcc-14-dev` on the Pi, minus the
  Pi's LTO plugin which the host linker cannot load).
- Raspbian's FFmpeg carries vendor pixel formats (SAND/RPI4) unknown to
  ffmpeg-next's exhaustive matches; `sync` hides them from bindgen (they
  sit at the enum tail, so no other value shifts — and an audio daemon
  never touches video pixel formats).
- ffmpeg-sys' host-compiled version probe needs an empty `stubs-soft.h`
  shim, trixie's merged-/usr needs a `lib -> usr/lib` symlink plus the
  loader compat path, and the kernel UAPI headers live under
  `/usr/lib/linux/uapi` on trixie.

## Checks

Run before considering any change done:

```
cargo test && cargo clippy --all-targets && cargo fmt --check
```
