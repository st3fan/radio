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

## Building the packages

On a Debian machine with SSH access to the Pi (see `service/README.md`
for prerequisites and the cross-compilation details):

```
service/build-pi.sh sync <pi-host>   # once, and after Pi upgrades
service/build-pi.sh deb              # → radiod_<version>_armhf.deb
deploy/build-website-deb.sh          # → radio-website_<version>_all.deb
```

## Installing on the Pi

```
scp radiod_*_armhf.deb radio-website_*_all.deb <pi-host>:
ssh <pi-host> apt install ./radiod_*_armhf.deb ./radio-website_*_all.deb
```

apt pulls all runtime dependencies (FFmpeg/ALSA libraries, lighttpd,
php-fpm). Then set the ALSA device in `/etc/radio/config.toml` (find it
with `aplay -l`) and `systemctl restart radiod`. The site is on port 80.

## Development

Plan-based, PR-based — see `CLAUDE.md` for the ways of working and
`notes/plan.md` for the design. Daemon development happens on macOS
(`cargo test` in `service/`), real-audio testing on a Debian machine,
and the Pi Zero only ever runs release packages.
