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
    libavutil-dev libswresample-dev
```

## Running

```
cargo run -- --sink wav:/tmp/out.wav      # macOS dev: listen by playing the WAV
cargo run -- --config radio.toml          # explicit config file
```

Flags: `--config <path>` (default `/etc/radio/config.toml`, optional),
`--sink null|wav:<path>` (default `null` until the ALSA sink lands),
`-v`/`--version`.

Config file (all fields optional):

```toml
listen = "127.0.0.1:8080"     # must be loopback
audio_device = "plughw:1,0"   # ALSA device (used from milestone 2 phase 3)
max_volume = 50               # hard cap; the volume can never exceed this
initial_volume = 25
```

## API

```
curl http://127.0.0.1:8080/status
curl -X POST http://127.0.0.1:8080/play -d '{"playlist_url": "https://somafm.com/defcon.pls"}'
curl -X POST http://127.0.0.1:8080/stop
```

## Checks

Run before considering any change done:

```
cargo test && cargo clippy --all-targets && cargo fmt --check
```
