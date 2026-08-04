# Installing on a clean Raspberry Pi OS (arm64)

First done 2026-08-01 on "muzak", a Pi 4 (1 GB) running Raspberry Pi OS
arm64 — which identifies as plain **Debian 13 (trixie)** plus a raspi
package overlay. Findings from that install; the packages came from
`build-deb.sh arm64` + `build-website-deb.sh` on the build box.

## The short version

```
scp radiod_<v>_arm64.deb root@<host>:
ssh root@<host>
apt install --no-install-recommends ./radiod_<v>_arm64.deb
# then: edit /etc/radio/config.toml (see below) and
systemctl restart radiod
```

**Use `--no-install-recommends`.** It is worth 32 packages and 254 MB —
see "The Recommends trap" below. The one Recommends radiod actually
wants is `avahi-daemon`, for the AirPlay `_airplay._tcp` advertisement;
install it explicitly:

```
apt install --no-install-recommends avahi-daemon
```

Without it radiod logs a warning and runs radio-only, so this is only
needed if you want AirPlay.

(Historical note: through v0.2.0 there was a second package,
`radio-website_all.deb`, carrying a PHP site on lighttpd + php-fpm; the
website now lives inside radiod, which Conflicts/Replaces the old
package. On a box upgraded from that era, `apt purge lighttpd
php8.4-fpm php-common` reclaims the idle web stack.)

## Do the dependencies resolve? Yes.

Every pinned Depends (`libavformat61`, `libavcodec61`, `libavutil59`,
`libswresample5`, `libasound2t64`, `libc6`, and `avahi-daemon` via
Recommends) exists on Raspberry Pi OS trixie arm64 under exactly the
Debian names — arm64 RPi OS *is* Debian trixie, so nothing to
translate.

One eyebrow-raiser: Debian's `libavcodec61` drags in a ~100-package
closure — Mesa, LLVM, X11/Wayland client libs, VA-API, every codec —
on a headless appliance. Several hundred MB of disk. Correct, just
bulky; Debian ships one fat libavcodec build and there is no lean
variant to depend on instead.

## The Recommends trap

Measured on RPi OS trixie arm64 against a real dpkg status (a
provisioned machine simply lacking the av stack):

| installing radiod's Depends | packages | installed size |
|---|---|---|
| `apt install` (apt's default: Recommends on) | 113 | 360 MB |
| `apt install --no-install-recommends` | 81 | 106 MB |
| **avoidable by policy alone** | **32** | **254 MB** |

Four packages dominate that 254 MB: `libllvm19` (118 MB),
`mesa-vulkan-drivers` (70 MB), `mesa-libgallium` (34 MB) and `libz3-4`
(26 MB). A headless radio does not need a Vulkan driver or a full LLVM.

The doorway is **`libva2`**, which `libavcodec61` depends on and which
carries `Recommends: va-driver-all | va-driver` — that pulls
`mesa-va-drivers` and the rest of Mesa in behind it.

> **Correction.** An earlier revision of this note recorded that
> `libavcodec61` "has 35 hard Depends and zero Recommends —
> `--no-install-recommends` would not slim the install; the closure is
> structural." The first half is true, the conclusion is not: apt
> applies Recommends **transitively** across the whole closure, so it is
> `libva2`'s recommends that fire, not `libavcodec61`'s own. Checking
> only the top-level package's `Recommends:` field is what hid this.

Reproduce either number with:

```
apt-get install -s [--no-install-recommends] \
    libavformat61 libavcodec61 libavutil59 libswresample5 \
    libasound2t64 libc6 | grep -c '^Inst '
```

### Auditing a box that was already installed the default way

```
# are the big offenders present?
dpkg -l libllvm19 mesa-vulkan-drivers mesa-libgallium libz3-4 2>/dev/null | grep '^ii'
# what is actually installed, largest first
dpkg-query -Wf '${Installed-Size}\t${Package}\n' | sort -rn | head -20
```

If they are there, `apt autoremove` will *not* take them (they are
recommended, so apt treats them as wanted). Remove the driver
metapackage and let autoremove follow:

```
apt purge va-driver-all mesa-va-drivers
apt autoremove --purge
```

Verify radiod still plays afterwards — this is a live appliance, so do
it while you can watch it, not remotely at the end of the day.

This whole section becomes moot once radiod links its own minimal
FFmpeg (see `plans/20260803-01-dependency-optimization.md`), which drops
the dependency list to `libasound2t64, libc6, libssl3t64,
ca-certificates`.

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
- USB replug mid-play (simulated with a sysfs unbind/bind of the DAC):
  the sink fails cleanly ("No such device"), state goes `stopped`, the
  daemon stays up, and the next `/play` re-asserts the ceiling before
  audio flows. (This DAC happens to persist its level across
  re-enumeration; the comes-back-loud correction path was verified
  separately on the Debian PC by moving the control externally.)
- Dependency closure double-check: `libavcodec61` has 35 hard Depends
  and zero Recommends of its own. ~~So `--no-install-recommends` would
  not slim the install; the closure is structural.~~ **That inference
  was wrong** — apt applies Recommends transitively, and `libva2` (one
  of those 35) recommends the Mesa driver stack. See "The Recommends
  trap" above: the policy is worth 32 packages / 254 MB. Whether muzak
  itself carries them was not re-checked at the time of that
  correction.
- Footprint: ~190 MB used of 905 total while playing — comfortable on
  the 1 GB Pi 4. The board never builds anything; it only runs release
  packages.
