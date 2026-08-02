# Radio

An internet radio **and AirPlay speaker** for a small ARM board (an
arm64 Raspberry Pi or an ARMv7 Banana Pi) connected to studio speakers:
pick a SomaFM channel in the browser — or pick "Radio" in any Apple
device's AirPlay list — and music comes out of the speakers.

```
browser (LAN) ──▶ lighttpd + php-fpm (website, port 80)
                        │ loopback HTTP API
                        ▼
Mac / iPhone ────▶ radiod (Rust daemon)
  (AirPlay 2)           │ libavformat/libavcodec/libswresample · openairplay2
                        ▼
                  ALSA ──▶ USB speakers
```

Two components, shipped as two Debian packages:

- **`radiod`** (`service/`) — Rust daemon: streams and decodes the icecast
  stream in-process via the FFmpeg libraries, plays to ALSA, exposes a
  loopback-only HTTP API (`/play`, `/stop`, `/pause`, `/resume`,
  `/volume`, `/mute`, `/unmute`, `/status` with live ICY song titles) —
  and embeds an **AirPlay 2 receiver**
  ([openairplay2](https://github.com/st3fan/openairplay2)): an AirPlay
  session preempts the radio, the station resumes when it ends, and the
  sender's volume slider behaves like the website's. The speakers are
  protected by a **hardware mixer ceiling** the daemon owns, asserts,
  and re-asserts — no source, slider, or software bug can exceed it.
- **`radio-website`** (`website/`) — plain PHP site: the SomaFM channel
  list with artwork, playback controls, now-playing display, and an
  AirPlay badge while a session is active.

## Getting the packages

**Releases**: publishing a GitHub Release (from a `v*` tag) builds and
attaches `radiod_<version>_{amd64,arm64,armhf}.deb`,
`radio-website_<version>_all.deb` and a `SHA256SUMS` file — built on
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
deploy/build-website-deb.sh          # → radio-website_<version>_all.deb
```

## Installing on the radio device

With the .deb for the board's architecture (`arm64` for a 64-bit Pi,
`armhf` for the ARMv7 Banana Pi — the armhf package refuses to install
on ARMv6 relics like the Pi Zero W):

```
scp radiod_*.deb radio-website_*_all.deb <host>:
ssh <host> apt install ./radiod_*.deb ./radio-website_*_all.deb
```

apt pulls all runtime dependencies (FFmpeg/ALSA libraries, lighttpd,
php-fpm). Then set the ALSA device in `/etc/radio/config.toml` (find it
with `aplay -l`) and `systemctl restart radiod`. The site is on port 80.

## Development

Plan-based, PR-based — see `CLAUDE.md` for the ways of working and
`notes/plan.md` for the design. Daemon development happens on macOS
(`cargo test` in `service/`), real-audio testing on a Debian machine,
and the radio board only ever runs release packages.
