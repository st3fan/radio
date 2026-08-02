# Installing on a clean Raspberry Pi OS (arm64)

First done 2026-08-01 on "muzak", a Pi 4 (1 GB) running Raspberry Pi OS
arm64 — which identifies as plain **Debian 13 (trixie)** plus a raspi
package overlay. Findings from that install; the packages came from
`build-deb.sh arm64` + `build-website-deb.sh` on the build box.

## The short version

```
scp radiod_<v>_arm64.deb radio-website_<v>_all.deb root@<host>:
ssh root@<host>
apt install ./radiod_<v>_arm64.deb ./radio-website_<v>_all.deb
# then: edit /etc/radio/config.toml (see below) and
systemctl restart radiod
```

## Do the dependencies resolve? Yes.

Every pinned Depends (`libavformat61`, `libavcodec61`, `libavutil59`,
`libswresample5`, `libasound2t64`, `libc6`; lighttpd/php for the
website) exists on Raspberry Pi OS trixie arm64 under exactly the
Debian names — arm64 RPi OS *is* Debian trixie, so nothing to translate.
The website postinst wired its php-fpm pool for PHP 8.4 (trixie's
version) without help, and lighttpd served on port 80 immediately.

One eyebrow-raiser: Debian's `libavcodec61` drags in a ~100-package
closure — Mesa, LLVM, X11/Wayland client libs, VA-API, every codec —
on a headless appliance. Several hundred MB of disk. Correct, just
bulky; Debian ships one fat libavcodec build and there is no lean
variant to depend on instead.

## The one manual step: the audio config

The shipped `/etc/radio/config.toml` example uses `plughw:0,0` and
mixer control `PCM`. On a Pi, **card 0 is the bcm2835 headphone jack**,
and it also has a `PCM` control (with a comedy dB range of
`-99999.99..4`), so radiod starts *successfully* against the wrong
device — ceiling asserted and playback routed to the headphone jack,
not the speakers. Consistent and safe, but silent where it matters.

On muzak the speakers hang off the USB DAC (card 1, "SZONE C7",
`amixer -c1 scontrols` → `PCM`, dB range -60..0):

```toml
audio_device = "plughw:1,0"

[mixer]
control = "PCM"
ceiling_db = -20.0
```

`aplay -l` / `amixer -c<N> scontrols` are the discovery tools;
`alsa-utils` was preinstalled on the RPi OS image (a Debian netinst
might need `apt install alsa-utils`).

Open question for a future change: should the shipped example use a
deliberately-invalid control name so a fresh install *refuses* loudly
until configured, instead of accidentally working against the wrong
card? (With `Restart=always`, refusal means a brief restart burst and
then a failed unit with a helpful journal.)

## Redeploying dev builds (same version number)

`apt install --reinstall ./radiod_<v>.deb` is a trap for dev builds:
when the version number hasn't changed, apt quietly prefers its cached
archive of that version over the file you just copied — the binary on
disk never changes and there is no error. For same-version redeploys
use `dpkg -i ./radiod_<v>.deb` (then `systemctl restart radiod`), and
when in doubt compare `sha256sum /usr/bin/radiod` against the build.
Real releases bump the version, so apt behaves there.

## Validation results on muzak

- Ceiling: `PCM` on `hw:1` pinned at exactly -20.00 dB, verified via
  `amixer` while a live SomaFM stream played to the speakers.
- `/stop` settled in 86 ms (bounded ALSA buffer + drop-not-drain).
- Footprint: ~190 MB used of 905 total while playing — comfortable on
  the 1 GB Pi 4. The board never builds anything; it only runs release
  packages.
