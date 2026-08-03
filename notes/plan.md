# Radio — Project Plan

An internet radio player for a small ARM board (an arm64 Raspberry Pi or
an ARMv7 Banana Pi; originally a Raspberry Pi Zero W) connected to studio
speakers. One binary, `radiod`: it plays an audio stream (from a `.pls`
playlist URL such as `https://somafm.com/defcon.pls`) to a preconfigured
ALSA device, embeds an AirPlay 2 receiver, and serves its own website
(SomaFM channel picker + controls) and JSON API on port 80.

## Hard constraints

- **Volume safety (critical):** the speakers are protected by a hardware
  mixer ceiling that radiod owns — asserted, verified and re-asserted on
  every playback session (milestone 10; see CLAUDE.md's critical
  invariant). The digital path runs at full scale and its gain clamp can
  attenuate but never amplify.
- **Hardware:** a small ARM board with 512 MB RAM — an arm64 Raspberry Pi
  (`aarch64-unknown-linux-gnu`) or an ARMv7 Banana Pi BPI-M2 Zero
  (`armv7-unknown-linux-gnueabihf`, Debian armhf). Keep CPU and memory
  use low and dependencies few. (Originally a single-core ARMv6 Pi
  Zero W, retired 2026-08 — see `plans/20260801-12-*.md`.)
- The HTTP server faces the LAN by design (it carries the website); the
  JSON API exposes exactly the controls the website offers, nothing
  more. No auth — a home-LAN appliance.

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
| HTTP server        | `hyper` on tokio                  | Served by `tiny_http` through milestone 8; milestone 9 moved the control plane onto a current-thread tokio runtime (see `plans/20260801-09-async-tokio.md`) so milestone 11 can embed the async openairplay2 library. Raw hyper, not axum: the hand-rolled router is the API contract and the dependency tree stays smaller. |
| HTTP client        | `ureq`                            | Only for fetching the `.pls` (a 5-line INI we parse by hand). |
| Config             | `serde` + `toml`                  | |
| Packaging          | `cargo-deb`                       | See "Packaging & deployment" below. |
| Website            | minijinja + vendored HTMX         | Served by radiod itself since the absorb-website milestone (`plans/20260802-01-*.md`): templates/assets embedded in the binary, `--web-dir` for dev edit-and-reload. PHP + lighttpd retired. |

### Build & runtime dependencies

- Runtime: the distro's shared libraries (trixie: `libavformat61`,
  `libavcodec61`, `libavutil59`, `libswresample5`, `libasound2t64`) —
  linked dynamically and declared as .deb Depends, so `apt install` pulls
  them.
- Build time: FFmpeg dev headers + libclang (the sys crates run bindgen).
  `.deb`s for amd64, arm64 and armhf build on any Debian 13 environment
  via `service/setup-build.sh` + `service/build-deb.sh` — native or
  Debian-multiarch cross, no Docker/emulation/sysroots — and the GitHub
  release workflow builds the same way on public runners (see
  `plans/20260801-12-*.md`; the earlier ARMv6 sysroot flow died with the
  Pi Zero W).
- Environments (details in CLAUDE.md): primary development on macOS
  (everything except the ALSA sink builds and tests there); a Debian PC —
  attached to the actual speakers — for real-ALSA integration testing;
  the radio board only ever runs the release packages.

### Packaging & deployment

One Debian package; distribution is scp + `apt install ./<deb>` or the
GitHub release assets (see the top-level README):

- **`radiod_<version>_{amd64,arm64,armhf}.deb`** (cargo-deb, via
  `service/build-deb.sh`): `/usr/bin/radiod`, a systemd unit (dedicated
  `radio` system user in the `audio` group, `Restart=always`, hardening)
  enabled and started on install, `/etc/radio/config.toml` as a conffile
  so upgrades never clobber local edits, Depends pinned to trixie's
  package names, and a preinst guard that keeps the armhf (ARMv7)
  package off ARMv6 hardware.

(The former `radio-website_all.deb` — PHP on lighttpd + php-fpm — was
absorbed into radiod; the radiod package Conflicts/Replaces it.)

Measured on the (since retired) Pi Zero: ~7% CPU and ~30 MB RSS while
playing; page loads in ~0.3 s; everything comes back by itself after a
reboot.

### Configuration

TOML file, path via `--config` (default `/etc/radio/config.toml`; the
radiod package ships a documented example there as a conffile). All fields
optional:

See `deploy/config.toml.example` for the authoritative, commented
example: `listen` (port 80 on the radio), `audio_device`, the `[mixer]`
ceiling (required for ALSA playback), `initial_volume`, and the
`[airplay]` section.

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

## Component 2: the built-in website

Originally plain PHP behind lighttpd + php-fpm; absorbed into radiod in
the absorb-website milestone (`plans/20260802-01-absorb-website.md`).
Now: minijinja templates + vendored HTMX served by radiod's own hyper
server on port 80, `service/web/` embedded into the binary.

- Fetches `https://api.somafm.com/channels.json` server-side with a
  5-minute in-memory cache and a stale fallback.
- Lists channels (sorted by listeners) with a **Play** button per
  channel; forms submit channel *ids* and the server resolves the
  playlist URL from its own cached channel data.
- Now-playing (state, station, icy-title, volume) with pause/resume,
  stop, mute and volume controls: HTMX swaps for reload-free buttons
  and a 5 s poll, plain POST-redirect-GET forms without JS.
- Installable iPhone PWA; the origin `http://<host>/` (port 80) is part
  of its identity.

## Repository layout

```
service/      Rust daemon (cargo project; the binary is named radiod)
service/web/  website templates + static assets (embedded at build time)
deploy/       packaging: systemd unit, config example, maintainer scripts
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
7. ✅ **Polish** — the Phosphor terminal UI (chosen from three rendered
   proposals), live now-playing via a status.php proxy + a tiny poller,
   blinking-cursor heartbeat. (Favourites, then still "a future idea",
   landed with `plans/20260802-05-favourites.md`.)

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
