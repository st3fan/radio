# Radio

An internet radio **and AirPlay speaker** for a small ARM board (an
arm64 Raspberry Pi or an ARMv7 Banana Pi) connected to studio speakers:
pick a SomaFM channel in the browser — or pick "Radio" in any Apple
device's AirPlay list — and music comes out of the speakers.

```
browser (LAN) ──▶ radiod (Rust daemon, port 80)
Mac / iPhone ──▶      │ built-in website + JSON API
  (AirPlay 2)         │ libavformat/libavcodec/libswresample · openairplay2
                      ▼
                ALSA ──▶ USB speakers
```

One binary, one Debian package. `radiod` (`service/`):

- **Streams and decodes** the icecast stream in-process via the FFmpeg
  libraries and plays to ALSA, with live ICY song titles.
- **Serves its own website** (`service/web/`): the sortable SomaFM
  channel list, transport and volume controls, now-playing display with
  channel artwork, rendered server-side (minijinja) with HTMX
  interactions, installable as an iPhone PWA — plus the JSON API
  (`/play`, `/stop`, `/pause`, `/resume`, `/volume`, `/mute`,
  `/unmute`, `/status`) on the same port for scripts. The volume
  setting survives restarts and upgrades.
- **Embeds an AirPlay 2 receiver**
  ([openairplay2](https://github.com/st3fan/openairplay2)): an AirPlay
  session preempts the radio, the station resumes when it ends, and the
  sender's volume slider behaves like the website's. During a session
  the website turns into the receiver page and shows what the sender is
  playing — track title, artist, album and cover art, live.
- The speakers are protected by a **hardware mixer ceiling** the daemon
  owns, asserts, and re-asserts — no source, slider, or software bug
  can exceed it.

## Getting the packages

**Releases**: publishing a GitHub Release (from a `v*` tag) builds and
attaches `radiod_<version>_{amd64,arm64,armhf}.deb` and a `SHA256SUMS`
file — built on
public runners inside `debian:trixie` containers by the same scripts
used locally. Every `.deb` carries a signed build-provenance
attestation; verify a download with:

```
gh attestation verify radiod_<version>_<arch>.deb --repo st3fan/radio
```

**Local builds**, on any Debian 13 environment (see `service/README.md`
for details):

```
service/setup-build.sh cross         # once; arm64 + armhf multiarch toolchains
service/build-deb.sh amd64           # → radiod_<version>_amd64.deb
service/build-deb.sh arm64           # → radiod_<version>_arm64.deb
service/build-deb.sh armhf           # → radiod_<version>_armhf.deb (ARMv7)
```

## Installing on the radio device

With the .deb for the board's architecture (`arm64` for a 64-bit Pi,
`armhf` for the ARMv7 Banana Pi — the armhf package refuses to install
on ARMv6 relics like the Pi Zero W):

```
scp radiod_*.deb <host>:
ssh <host> apt install ./radiod_*.deb
```

apt pulls the runtime dependencies (FFmpeg/ALSA libraries, and
avahi-daemon via Recommends for AirPlay). Then set the ALSA device and
`[mixer]` ceiling in `/etc/radio/config.toml` (find the device with
`aplay -l`) and `systemctl restart radiod`. The site is on port 80.
Upgrading from the PHP era: the deb removes `radio-website`
automatically; `apt purge lighttpd php8.4-fpm php-common` reclaims the
now-idle web stack.

## Development

Plan-based, PR-based — see `CLAUDE.md` for the ways of working,
`notes/plan.md` for the design, and `runbooks/` for operational
procedures such as releasing. Every pull request runs the full
`cargo test` / `clippy` / `fmt` CI in a Debian trixie container.
Daemon development happens on macOS (`cargo test` in `service/`),
real-audio testing on a Debian machine, and the radio board only ever
runs release packages — unreleased builds give themselves away with a
`RADIO DEV (git hash)` banner in place of the version number. Website
iteration: `cargo run -- --sink null --web-dir service/web` serves
templates and assets from disk — edit, reload, no recompile.
