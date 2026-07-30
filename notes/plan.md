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
  function bounded by this maximum. User-facing volume values are a
  percentage of `max_volume` (100 = the cap), so exceeding it is impossible
  by construction.
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
  → software gain: effective volume / 100, or 0 when muted   ← the bound
  → alsa crate → the configured device (plughw:0,0 on the office Pi)
```

- Playback runs on a dedicated thread. HTTP handlers send commands
  (play/stop/pause/volume/mute) over a channel; shared status lives behind a
  mutex.
- **Volume cap:** decoded PCM flows through our code, so the gain is applied
  by a single, unit-tested Rust function between decoder and ALSA — no
  external process to trust. `volume::effective_volume()` (mapping a 0–100
  percentage onto `[0, max_volume]`) is the only path to the sample
  multiplier.
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
| Streaming + decode | `ffmpeg-next`                     | Safe wrapper over libav*. Maintenance-mode but kept compiling against FFmpeg 3.4–8.0, so it works with whatever Raspberry Pi OS ships (trixie: 7.1). Features trimmed to `codec`/`format`/`software-resampling` so libavfilter/libavdevice are not needed on the Pi. Alternative if we hit a wall: `rsmpeg`. |
| ALSA output        | `alsa` crate (libasound bindings) | Use `plughw:...` so ALSA converts rate/format if needed. Behind an `AudioSink` trait: ALSA is Linux-only, and a dev sink keeps macOS builds/tests green and makes the gain clamp assertable in tests. |
| HTTP server        | `tiny_http`                       | The API is tiny; a small threaded server beats pulling in tokio/axum on a single ARMv6 core. |
| HTTP client        | `ureq`                            | Only for fetching the `.pls` (a 5-line INI we parse by hand). |
| Config             | `serde` + `toml`                  | |
| Packaging          | `cargo-deb` (daemon), `dpkg-deb` (website) | See "Packaging & deployment" below. |

### Build & runtime dependencies

- Runtime: the distro's shared libraries (trixie: `libavformat61`,
  `libavcodec61`, `libavutil59`, `libswresample5`, `libasound2t64`) —
  linked dynamically and declared as .deb Depends, so `apt install` pulls
  them.
- Build time: FFmpeg dev headers + libclang (the sys crates run bindgen).
  Building on the Pi Zero itself (one ARMv6 core, 512 MB) is a non-starter.
  `service/build-pi.sh` cross-compiles for `arm-unknown-linux-gnueabihf`
  against a **sysroot rsynced from the Pi itself** (exact soname/symbol
  match), from any Debian environment — the Debian PC or a Docker container
  (milestone 6 was built from a container on the Mac). The script encodes
  several hardware-won gotchas (ARMv7 leakage from the cross toolchain,
  Raspbian's vendor pixel formats, trixie sysroot quirks) — documented in
  `service/README.md`.
- Environments (details in CLAUDE.md): primary development on macOS
  (everything except the ALSA sink builds and tests there); a Debian PC —
  attached to the actual speakers — for real-ALSA integration testing;
  the Pi Zero only ever runs the release packages.

### Packaging & deployment

Both components ship as Debian packages; distribution is scp +
`apt install ./<deb>` (see the top-level README for the commands):

- **`radiod_<version>_armhf.deb`** (cargo-deb, via `build-pi.sh deb`):
  `/usr/bin/radiod`, a systemd unit (dedicated `radio` system user in the
  `audio` group, `Restart=always`, hardening) enabled and started on
  install, `/etc/radio/config.toml` as a conffile so upgrades never
  clobber local edits, and Depends pinned to the Pi's package names.
- **`radio-website_<version>_all.deb`** (dpkg-deb, via
  `deploy/build-website-deb.sh`): the site under `/var/www/radio`
  (`lib/` outside the docroot), a lighttpd conf-available snippet riding
  Debian's own version-independent php-fpm wiring, and a php-fpm pool
  override (`pm = static`, `pm.max_children = 2` — 512 MB shared with the
  daemon) installed per PHP version at postinst time.

Measured on the Pi Zero: ~7% CPU and ~30 MB RSS while playing; page loads
in ~0.3 s; everything comes back by itself after a reboot.

### Configuration

TOML file, path via `--config` (default `/etc/radio/config.toml`; the
radiod package ships a documented example there as a conffile). All fields
optional:

```toml
listen = "127.0.0.1:8080"     # must be loopback
audio_device = "plughw:0,0"   # ALSA device (aplay -l)
max_volume = 50               # hard cap, defaults to 50 if omitted
initial_volume = 50           # startup volume, a percentage of max_volume
```

### REST API

The full reference (exact status codes, edge cases, field lifecycles) is
`notes/api.md`; this table is the design summary.

| Method & path    | Body                          | Effect |
|------------------|-------------------------------|--------|
| `GET  /status`   | —                             | Full status (below). |
| `POST /play`     | `{"playlist_url": "https://…/defcon.pls"}` | Resolve playlist, start playing. Switching stations is just another `/play`; playing the already-playing playlist is a no-op, and `/play` for the currently-paused playlist resumes it. |
| `POST /stop`     | —                             | Stop playback entirely. |
| `POST /pause`    | —                             | Disconnect, remember what was playing. 409 when stopped; no-op when already paused. |
| `POST /resume`   | —                             | Reconnect to the remembered stream. 409 when stopped; no-op when already playing. |
| `POST /volume`   | `{"volume": 30}`              | 0–100, a percentage of `max_volume` (100 = the cap; with `max_volume = 30`, request 100 → effective 30). Response returns the effective value. |
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
footprint. Lives in `website/`.

- Fetches `https://api.somafm.com/channels.json` server-side and caches it on
  disk (e.g. 5-minute TTL) so we don't hammer SomaFM and pages stay fast on
  the Pi.
- Lists channels (artwork, title, genre, description, listeners, sorted by
  listeners) with a **Play** button per channel. Play forms submit channel
  *ids*; the server resolves the `.pls` URL from its own cached channel
  data, so the browser never feeds URLs to the daemon — and never talks to
  the daemon at all.
- A status area showing `GET /status` (state, station, icy-title, volume)
  plus pause/resume, stop, mute, and volume controls — all proxied through
  PHP, POST-redirect-GET throughout, all external data escaped.
- Functional, unstyled HTML (forms + full-page reloads). Styling and JS
  niceties (auto-refreshing now-playing) are the remaining polish
  milestone.
- Serving: `php -S` for development; in production lighttpd + php-fpm,
  installed and wired by the `radio-website` package (port 80 on the LAN —
  the one intentionally reachable piece).

## Repository layout

Monorepo with the two components as top-level directories:

```
service/      Rust daemon (cargo project; the binary is named radiod)
website/      PHP website
deploy/       packaging: systemd unit, config example, maintainer scripts,
              the website .deb build script
notes/        long-lived notes, this plan
plans/        per-step implementation plans (YYYYMMDD-NN-slug.md)
```

## Roadmap

Each milestone gets its own `plans/YYYYMMDD-NN-slug.md` and lands via one or
more PRs (see CLAUDE.md for the workflow).

1. ✅ **Daemon skeleton** — cargo project under `service/`, config loading
   with defaults, tiny_http server on `127.0.0.1`, stubbed `/status`,
   unit tests for config + volume math.
2. ✅ **Playback** — `.pls` fetch/parse, ffmpeg-next open/decode/resample
   pipeline behind a `Source` trait, `AudioSink` trait (ALSA / WAV / null),
   `/play` and `/stop` end to end on real hardware.
3. ✅ **Volume & mute** — `/volume` (a percentage of `max_volume`, revised
   during implementation), `/mute`, `/unmute`; the bound proven by
   exhaustive and end-to-end tests.
4. ✅ **Metadata & pause/resume** — `icy_title`/`icy_name` in `/status`
   (the mpv `av_opt_get` mechanism), pause/resume as a player state
   machine, reconnect-with-backoff so dropped streams keep playing.
5. ✅ **Website v1** — PHP channel list from cached `channels.json` with
   artwork, play buttons, status display, all controls. Functional,
   unstyled.
6. ✅ **Deployment** — `build-pi.sh` ARMv6 cross-compile (sysroot from the
   Pi), the two Debian packages (`radiod_armhf.deb` with systemd unit and
   conffile, `radio-website_all.deb` with lighttpd + php-fpm), install via
   `apt install ./<deb>`. Verified on the office Pi: reboot-proof, ~7%
   CPU, flat memory over a soak.
7. **Polish (remaining)** — styling the site, auto-refreshing now-playing,
   maybe favorites/presets.

## Formerly open questions, answered along the way

- ALSA device on the office Pi: the USB speakers are card 0 →
  `plughw:0,0` (the packaged config default).
- The Pi runs Raspbian trixie: FFmpeg 7.1, PHP 8.4 — both fine for the
  chosen stack.
- Cross-compilation: sysroot-from-the-Pi, buildable from the Debian PC or
  a Docker container (no Pi 4 needed). The gotchas live in
  `service/build-pi.sh` and `service/README.md`.
- License for the packages: still Stefan's call (`license` is unset in
  `Cargo.toml`; cargo-deb warns).
