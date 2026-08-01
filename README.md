# Radio

An internet radio for a Raspberry Pi Zero W connected to studio speakers:
pick a SomaFM channel in the browser, music comes out of the speakers.

```
browser (LAN) ──▶ lighttpd + php-fpm (website, port 80)
                        │ loopback HTTP API
                        ▼
                  radiod (Rust daemon)
                        │ libavformat/libavcodec/libswresample
                        ▼
                  ALSA ──▶ USB speakers
```

Two components, shipped as two Debian packages:

- **`radiod`** (`service/`) — Rust daemon: streams and decodes the icecast
  stream in-process via the FFmpeg libraries, plays to ALSA, exposes a
  loopback-only HTTP API (`/play`, `/stop`, `/pause`, `/resume`,
  `/volume`, `/mute`, `/unmute`, `/status` with live ICY song titles).
  The output level is **hard-capped** by the `max_volume` setting —
  requests are percentages of that cap, so it cannot be exceeded.
- **`radio-website`** (`website/`) — plain PHP site: the SomaFM channel
  list with artwork, playback controls, now-playing display.

## Getting the packages

**Releases**: publishing a GitHub Release (from a `v*` tag) builds and
attaches `radiod_<version>_amd64.deb`, `radiod_<version>_arm64.deb`,
`radio-website_<version>_all.deb` and a `SHA256SUMS` file — built
natively on public runners inside `debian:trixie` containers by the
same scripts used locally.

**Local builds**, on any Debian 13 environment (see `service/README.md`
for details):

```
service/setup-build.sh cross         # once; multiarch toolchain for arm64
service/build-deb.sh amd64           # → radiod_<version>_amd64.deb
service/build-deb.sh arm64           # → radiod_<version>_arm64.deb
deploy/build-website-deb.sh          # → radio-website_<version>_all.deb
```

The ARMv6 Pi Zero W path (`service/build-pi.sh`, needs SSH to the Pi)
is legacy and remains only until that board is replaced by an arm64 one.

## Installing on the radio device

```
scp radiod_*_arm64.deb radio-website_*_all.deb <host>:
ssh <host> apt install ./radiod_*_arm64.deb ./radio-website_*_all.deb
```

apt pulls all runtime dependencies (FFmpeg/ALSA libraries, lighttpd,
php-fpm). Then set the ALSA device in `/etc/radio/config.toml` (find it
with `aplay -l`) and `systemctl restart radiod`. The site is on port 80.
(The current Pi Zero W still takes the legacy `_armhf.deb` from
`build-pi.sh` until the hardware swap.)

## Development

Plan-based, PR-based — see `CLAUDE.md` for the ways of working and
`notes/plan.md` for the design. Daemon development happens on macOS
(`cargo test` in `service/`), real-audio testing on a Debian machine,
and the Pi Zero only ever runs release packages.
