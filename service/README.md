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

```
curl http://127.0.0.1:8080/status
curl -X POST http://127.0.0.1:8080/play -d '{"playlist_url": "https://somafm.com/defcon.pls"}'
curl -X POST http://127.0.0.1:8080/stop
curl -X POST http://127.0.0.1:8080/volume -d '{"volume": 30}'
curl -X POST http://127.0.0.1:8080/mute
curl -X POST http://127.0.0.1:8080/unmute
```

Volume requests take 0–100 as a **percentage of `max_volume`**: 100 means
"as loud as the cap allows", never louder. With `max_volume = 30`, a request
of 100 yields an effective device volume of 30. Responses and `/status`
always show the effective value.

## Checks

Run before considering any change done:

```
cargo test && cargo clippy --all-targets && cargo fmt --check
```
