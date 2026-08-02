# Plan: Radio OS — a flashable card image

- **Date:** 2026-08-02
- **Status:** draft

## The crazy idea

A disk image that turns a Pi into a radio: flash a card, boot, and
http://radio.local is the radio — no `apt install`, no SSH, no config
editing. The Raspberry Pi Imager is the flashing tool, ideally with
its OS-customisation dialog (hostname, user account, Wi-Fi, SSH key)
still working on our image.

## Straight answers to the two questions

**Can Imager flash a custom Raspbian-based image?** Yes — "Use
custom" flashes any local `.img`/`.img.xz`.

**Does OS customisation (network + account) still work then?** With a
caveat, and it decides the shape of this plan:

- The *applying* side is easy: an image built on Raspberry Pi OS keeps
  the first-boot machinery (`raspberrypi-sys-mods`) that consumes
  Imager's customisation files (`firstrun.sh` in the "systemd" format,
  `rpi-preseed.toml` in the newer one). Our image supports whatever
  the official RPi OS Lite of the same release supports, for free.
- The *offering* side is the catch: Imager decides whether to show the
  customisation dialog from the `init_format` field in the OS-list
  metadata. **A local "Use custom" image has no metadata, so Imager
  2.x assumes `init_format: none` and offers no customisation at all**
  (Imager 1.x just wrote `firstrun.sh` and hoped; 2.x stopped —
  rpi-imager issues #1302/#1377). Two supported ways around it:
  1. **Ship our own OS-list manifest** (a small JSON at a stable URL,
     e.g. GitHub Pages) declaring the image URL, checksums and
     `init_format`. Imager pointed at that repo lists "Radio OS" like
     any official OS, downloads it itself, and the customisation
     dialog works exactly like for Raspberry Pi OS. This is the
     first-class experience.
  2. The rpi-imager repo's `create_local_json.py` generates the same
     metadata for a locally downloaded image — the documented escape
     hatch, fine for us, unlovely for guests.

So: **yes, the whole idea works**, provided we publish a manifest
rather than relying on "Use custom".

## Design

### Building the image: pi-gen

[`pi-gen`](https://github.com/RPi-Distro/pi-gen) is the tool that
builds the official Raspberry Pi OS images — stages 0–2 produce
exactly Raspberry Pi OS Lite. We add one custom stage on top:

- **Base:** RPi OS Lite, trixie, **arm64 only** (the image targets the
  Pi 4/Zero 2 class; the ARMv7 Banana Pi is not a Pi and boots its own
  way — out of scope, the armhf .deb remains its path).
- **Radio stage:** install the pinned radiod release `.deb` (fetched
  by exact version + sha256 from the GitHub release — the image is a
  packaging of a release, never a dev build), which pulls the
  FFmpeg/ALSA/avahi runtime deps and enables the service; default
  hostname `radio` (→ `http://radio.local`, overridable in Imager);
  `/etc/radio/config.toml` preconfigured for the canonical hardware.
- **Keep everything stock otherwise** — especially the first-boot
  customisation machinery and SSH-off-by-default; Imager's dialog is
  where users make an account and enable SSH.
- Output: `radio-os-<version>-arm64.img.xz` + `SHA256SUMS`.

Build host: the amd64 Debian 13 build box. pi-gen cross-builds via
`qemu-user-static` binfmt (debootstrap's standard trick) or its
Docker wrapper — a different game from the .deb rule of "no
emulation", and confined to image builds. Phase 1 settles which mode;
if amd64 proves painful, GitHub's free arm64 runners build it
natively.

### The config problem (the one honest wrinkle)

radiod **refuses to start without a verified mixer ceiling** — that is
the point of it — but a generic image cannot know the DAC. Decision:

- The image ships the canonical config: the standard USB DAC
  (`plughw:1,0`, `PCM`, conservative ceiling) — the hardware this
  project actually pairs with speakers.
- On other audio hardware radiod will fail its mixer check and
  systemd will hold it in restart; the fix is one documented edit of
  `/etc/radio/config.toml` over SSH. The failure mode is *silence*,
  never *loud* — the invariant behaves exactly as designed.
- A first-boot hardware-detect that writes the mixer config was
  considered and rejected for v1: guessing at ceilings is the one
  thing this project must never do.

### Distribution

- The `.img.xz` + checksums attach to the GitHub release next to the
  `.deb`s (assets comfortably under limits; provenance attestation
  like the debs).
- The Imager manifest JSON lives on GitHub Pages (`imager/` →
  published), pointing at the latest release's image. This pairs
  naturally with the "apt repo on Pages" idea in `notes/ideas.md` —
  same publishing channel, and the apt repo is what would give the
  image an *update* story beyond reflashing.

## Phases

One stack: this plan at the bottom, phases as PRs on top.

### Phase 1 — spike: build one by hand, prove the loop

Scripted-but-manual pi-gen run on the build box: base + radio stage,
producing an image that is flashed with "Use custom" +
`create_local_json.py` metadata. **Acceptance is the full loop on real
hardware:** flash a spare card, set hostname/Wi-Fi/account in the
Imager dialog, first boot on a Pi, and the radio plays at
`http://radio.local` with zero SSH. Findings (pi-gen branch/arch
choices, amd64-vs-arm64 build reality, exact `init_format` for
trixie) land back in this document.

### Phase 2 — `image/` in the repo, repeatable build

`image/build-image.sh` in the style of `service/build-deb.sh`
(argument: the release version to bake), pi-gen config + the radio
stage checked in, README section. No CI yet — the script is the
contract, like the deb scripts were before the workflow.

### Phase 3 — Imager manifest + docs

The OS-list JSON published on GitHub Pages with `init_format` and
per-release image URL/checksums; README gains the two-line "point
Imager at this repo" instructions alongside the "Use custom" path.

### Phase 4 — CI (optional, judged after phase 2)

A workflow (release-triggered or manual) building the image on an
arm64 public runner and uploading it to the release — same
public-runner constraint as the deb workflow. Image builds are slow
and large; whether automation earns its keep is decided when we see
phase 2's build times.

## Non-goals

- armhf/Banana Pi image (deb install remains its path).
- In-place update mechanism for image installs (that's the apt-repo
  idea, separate).
- Hardware autodetection of DACs/mixer ceilings — never guessed.

## Open questions for review

- Name: "Radio OS"? It ends up user-visible in Imager's list.
- Should the image bake a first favourite / default station, or boot
  to standby?
- Spare Pi + card for the acceptance test (muzak is the production
  radio; flashing its card is the goal, not the test bench).
