# Radio — Project Plan

An internet radio player for a Raspberry Pi Zero W (v1) connected to studio
speakers. Two components:

1. **`radiod`** — a Rust daemon exposing an HTTP REST API on `127.0.0.1` that
   plays an audio stream (from a `.pls` playlist URL such as
   `https://somafm.com/defcon.pls`) to a preconfigured ALSA device.
2. **Website** — a PHP site on the same Pi that lists the SomaFM channels from
   `https://api.somafm.com/channels.json` and lets you pick one; picking a
   channel sends its playlist URL to the daemon.

## Hard constraints

- **Volume safety (critical):** the audio level sent to the device can *never*
  exceed a preconfigured maximum. Default maximum is **50** (on a 0–100
  scale). This is a safety invariant, not a preference — the Pi drives studio
  speakers. Every code path that produces audio must go through a single gain
  function that clamps to this maximum. Requests to set a higher volume are
  clamped, never rejected-then-forgotten.
- **Hardware:** Pi Zero W 1.0 — single-core **ARMv6** (ARM1176, 32-bit),
  512 MB RAM. Keep CPU and memory use low, keep dependencies few, no heavy
  async runtimes if we can avoid them. Note: ARMv6, so the Rust target is
  `arm-unknown-linux-gnueabihf`, *not* `armv7-...`.
- Daemon API binds `127.0.0.1` only. The PHP site talks to it server-side,
  so nothing on the LAN can reach the daemon directly.

## Component 1: `radiod` (Rust daemon)

### Architecture: FFmpeg libraries (libavformat/libavcodec) in-process

Everything runs inside the single Rust daemon. We use the FFmpeg libraries —
the same ones mpv is built on — through the **`ffmpeg-next`** crate.
libavformat handles the network streaming (HTTP/HTTPS with reconnect
support, ICY metadata, demuxing), libavcodec decodes MP3/AAC, and
libswresample converts to the sample format we hand to ALSA. These are
battle-tested C libraries, faster on an ARM1176 (no NEON) than pure-Rust
decoders.

```
playlist URL (.pls)
  → ureq GET + hand-rolled parse → stream URL     (libavformat has no .pls demuxer)
  → avformat_open_input(stream URL)               (http: icy=1, reconnect on error)
  → av_read_frame → libavcodec decode → PCM
  → libswresample → s16 interleaved
  → software gain: min(volume, max_volume) / 100, or 0 when muted   ← the clamp
  → alsa crate → device (plughw:1,0)
```

- Playback runs on a dedicated thread. HTTP handlers send commands
  (play/stop/pause/volume/mute) over a channel; shared status lives behind a
  mutex.
- **Volume cap:** decoded PCM flows through our code, so the gain is applied
  by a single, unit-tested Rust function between decoder and ALSA — no
  external process to trust. `min(volume, max_volume)` is the only path to
  the sample multiplier.
- **ICY metadata:** open the input with `icy=1` (default for http), then
  poll the `icy_metadata_packet` option on the format context
  (`av_opt_get` with `AV_OPT_SEARCH_CHILDREN`) and parse `StreamTitle='…'`.
  This is the same mechanism mpv uses. Station name comes from
  `icy_metadata_headers`.
- **Reconnects:** open with `reconnect=1`, `reconnect_streamed=1`,
  `reconnect_delay_max=…` so libavformat retries transient network errors;
  on hard failure the playback thread re-opens with backoff while state
  stays `playing`.
- "Pause" on a live stream means disconnect and close the ALSA device;
  "resume" reconnects. No buffering while paused (512 MB RAM, and live
  radio has no seek position anyway).

### Crate choices

| Concern            | Choice                            | Notes |
|--------------------|-----------------------------------|-------|
| Streaming + decode | `ffmpeg-next`                     | Safe wrapper over libav*. Maintenance-mode but kept compiling against FFmpeg 3.4–8.0, so it works with whatever Raspberry Pi OS ships. ~1.3M downloads/month. Alternative if we hit a wall: `rsmpeg` (actively developed, thinner/rawer API). |
| ALSA output        | `alsa` crate (libasound bindings) | Use `plughw:...` so ALSA converts rate/format if needed. |
| HTTP server        | `tiny_http`                       | The API is tiny; a small threaded server beats pulling in tokio/axum on a single ARMv6 core. |
| HTTP client        | `ureq`                            | Only for fetching the `.pls` (a 5-line INI we parse by hand). |
| Config             | `serde` + `toml`                  | |

### Build & runtime dependencies

- Runtime (apt, Raspberry Pi OS): `libavformat`, `libavcodec`, `libavutil`,
  `libswresample`, `libasound2`. We link dynamically against the distro
  libraries.
- Build time: FFmpeg dev headers + libclang (the sys crates run bindgen).
  Building on the Pi Zero itself (one ARMv6 core, 512 MB) is a non-starter;
  we cross-compile for `arm-unknown-linux-gnueabihf` against a Raspberry
  Pi OS sysroot, or build on a faster ARM box (e.g. a Pi 4 running 32-bit
  Raspberry Pi OS) and copy the binary. Settled in the deployment milestone.

### Configuration

TOML file, path via `--config` (default `/etc/radio/config.toml`):

```toml
listen = "127.0.0.1:8080"
audio_device = "plughw:1,0"   # ALSA device
max_volume = 50               # hard cap, defaults to 50 if omitted
```

### REST API

| Method & path    | Body                          | Effect |
|------------------|-------------------------------|--------|
| `GET  /status`   | —                             | Full status (below). |
| `POST /play`     | `{"playlist_url": "https://…/defcon.pls"}` | Resolve playlist, start playing. Switching stations is just another `/play`. |
| `POST /stop`     | —                             | Stop playback entirely. |
| `POST /pause`    | —                             | Stop pulling the stream, remember what was playing. |
| `POST /resume`   | —                             | Reconnect to the remembered stream. |
| `POST /volume`   | `{"volume": 30}`              | 0–100 requested; effective volume is clamped to `max_volume`. Response returns the effective value. |
| `POST /mute`     | —                             | Gain to 0, remember volume. |
| `POST /unmute`   | —                             | Restore volume. |

`GET /status` response:

```json
{
  "state": "playing",            // playing | paused | stopped
  "playlist_url": "https://somafm.com/defcon.pls",
  "stream_url": "https://ice2.somafm.com/defcon-128-mp3",
  "icy_title": "Nightmares on Wax - Les Nuits",
  "icy_name": "DEF CON Radio",
  "volume": 30,
  "muted": false,
  "max_volume": 50
}
```

Errors: JSON body `{"error": "…"}` with appropriate 4xx/5xx status.

## Component 2: Website (PHP)

Plain PHP, kept deliberately simple — fast to iterate on, trivial resource
footprint. Lives in `web/`.

- Fetches `https://api.somafm.com/channels.json` server-side and caches it on
  disk (e.g. 5-minute TTL) so we don't hammer SomaFM and pages stay fast on
  the Pi.
- Lists channels (title, description, listeners); each has a **Play** button
  that POSTs to the PHP backend, which forwards the channel's `.pls` URL to
  `radiod` on `127.0.0.1`. The browser never talks to the daemon directly.
- A status area showing `GET /status` (station, icy-title, volume) plus
  pause/resume, mute, and volume controls — all proxied through PHP.
- Start with functional, unstyled HTML (forms + full-page reloads are fine).
  Styling and JS niceties (auto-refreshing now-playing) come later.
- Serving: start with `php -S 0.0.0.0:8000` for development on the Pi;
  production setup is lighttpd or nginx + php-fpm (decided in the deployment
  step).

## Repository layout

```
radiod/       Rust daemon (cargo project)
web/          PHP website
notes/        long-lived notes, this plan
plans/        per-step implementation plans (YYYYMMDD-NN-slug.md)
```

## Roadmap

Each milestone gets its own `plans/YYYYMMDD-NN-slug.md` and lands via one or
more PRs (see CLAUDE.md for the workflow).

1. **Daemon skeleton** — cargo project under `radiod/`, config loading with
   defaults (`max_volume = 50`), tiny_http server on `127.0.0.1`, `/status`
   returning stubbed state, unit tests for config + volume clamping.
2. **Playback** — `.pls` fetch/parse, ffmpeg-next open/decode/resample
   pipeline, ALSA output; `/play` and `/stop` work end to end on the Pi.
3. **Volume & mute** — software gain with the hard cap applied to every
   sample buffer, `/volume`, `/mute`, `/unmute`; the clamp logic thoroughly
   unit-tested.
4. **Metadata & pause/resume** — ICY metadata (`icy_metadata_packet` →
   `icy_title`, `icy_metadata_headers` → `icy_name`) surfaced in `/status`;
   `/pause` and `/resume` (disconnect/reconnect semantics);
   reconnect-with-backoff on stream errors.
5. **Website v1** — PHP channel list from cached `channels.json`, play
   buttons, status display, basic controls. Functional, unstyled.
6. **Deployment** — cross-compilation notes (ARMv6 target) or on-device
   build, systemd unit for `radiod`, web server setup, install docs.
7. **Polish (later)** — styling the site, auto-refreshing now-playing,
   maybe favorites/presets.

## Open questions

- Exact ALSA device name on the actual Pi (`hw:1,0` vs `plughw:1,0` vs a
  named device) — confirm on hardware during milestone 2.
- Which FFmpeg version ships in the Raspberry Pi OS release on the device
  (`ffmpeg-next` compiles against 3.4–8.0, so this should only matter for
  pinning the crate feature flags) — confirm during milestone 2.
- Exact cross-compilation setup (sysroot vs. build-on-a-Pi-4) — settled in
  the deployment milestone, but worth a spike early since milestone 2 needs
  a binary running on the Pi.
