# Radio

An internet radio player and AirPlay speaker for a small ARM board (an
arm64 Raspberry Pi or an ARMv7 Banana Pi) connected to studio speakers.
One component: `service/`, a Rust daemon (`radiod`) that plays a stream
(from a `.pls` playlist URL) to a preconfigured ALSA device, embeds an
AirPlay 2 receiver (the `openairplay2` library), and serves its own
website — server-rendered minijinja templates with HTMX interactions
from `service/web/`, LAN-facing on port 80 alongside the JSON API
(`notes/api.md`). Streaming, demuxing and decoding are done in-process
via the FFmpeg libraries (libavformat/libavcodec, through the
`ffmpeg-next` crate); ALSA output via the `alsa` crate. The distro's
libav*/libasound shared libraries are runtime dependencies on the
radio.

The HTTP server is deliberately LAN-reachable: it carries the website,
and it exposes nothing the old PHP site did not already offer the LAN.

The overall design and roadmap live in `notes/plan.md`. Read that first.

## Critical invariant: the mixer ceiling

The speakers are protected by a **hardware ceiling that radiod owns**: the
ALSA mixer control named in the `[mixer]` config section is set to the
configured ceiling, read back and verified at startup, and re-asserted at
every playback session start — playing without a verified ceiling is
refused (this covers `alsamixer` meddling, boot-time `alsactl restore`,
and re-enumerating USB DACs). This is a safety requirement — the box
drives studio speakers. Belt and braces on top: the digital path runs at
full scale, and all gain flows through `volume::gain()` /
`volume::apply_gain()`, which clamp so software can attenuate but
**never amplify**; muted means gain 0. Never add a code path between
decoder and ALSA that bypasses that clamp, and never write code that
raises the mixer above the configured ceiling.

## Hardware constraints

Target is a small ARM board with **512 MB RAM**: an arm64 Raspberry Pi
(`aarch64-unknown-linux-gnu`) or an ARMv7 Banana Pi BPI-M2 Zero
(`armv7-unknown-linux-gnueabihf`, Debian armhf). The retired ARMv6
Pi Zero W is **not** supported — the armhf .deb refuses to install
there. Prefer small, few-dependency solutions over heavy frameworks.
The one async runtime is the **current-thread** tokio runtime that owns
the control plane (HTTP API, later AirPlay); the audio path stays
blocking (see below).

## How we work: plan-based development in PR stacks

Every time we work on a step of this project, we first write an
implementation plan as `plans/YYYYMMDD-NN-slug.md` (e.g.
`plans/20260729-01-daemon-skeleton.md`, where `NN` is a sequence number for
that day). The plan describes what will be built and how; complicated work
is split into **phases**, each phase its own pull request.

**A plan and its implementation live together in one stack** (managed with
the `gh-stack` skill). The plan document is the bottom PR of a fresh stack;
the implementation phases are PRs stacked on top, one branch per phase, each
based on the one below it:

- Open the stack with the plan PR alone, and **wait for Stefan to approve
  the plan** (review feedback on the open PR — not a merge) before stacking
  implementation PRs onto it.
- The plan PR **stays open for the whole task** — that is the point of
  stacking it: if the work reveals mid-way that the plan needs adjusting, or
  decisions worth recording, commit them to the plan document on its
  still-open branch (then `gh stack rebase --upstack`), so the plan that
  eventually merges matches what was actually built.
- At the end Stefan reviews and merges the whole stack himself.

This convention was proven in
[openairplay2](https://github.com/st3fan/openairplay2) and replaces the
earlier integration-branch workflow (used through milestone 5); already-
merged history keeps its shape.

## Pull requests

- All work lands via pull requests — no direct commits to `main`.
- Stefan reads, approves, and **merges** PRs and stacks himself.
- Always check PR status with `gh` (e.g. `gh pr view`, `gh stack view
  --json`) instead of assuming a PR is still open — it may have been merged
  or closed in the meantime. Never push follow-up commits to a branch whose
  PR is no longer open; put them on a new branch with a new PR.
- Small standalone work that needs no plan: branch from `main`, one PR
  against `main`.
- After a stack (or PR) merges, `gh stack sync --prune` brings `main` up to
  date and cleans up the merged branches.

## Runbooks

Operational procedures live in `runbooks/`. When asked to do a
**release**, follow `runbooks/releasing.md` — it defines the steps, the
failure procedure, and the autopilot arrangement (a release request is
standing permission to run every step once the version number is
agreed).

## Coding notes

### Rust (`service/`)

- Run `cargo test`, `cargo clippy` and `cargo fmt` before considering a
  change done.
- Keep dependencies minimal and justify new ones in the PR description. The
  approved core set: `ffmpeg-next`, `alsa`, `tokio`, `hyper` (with
  `hyper-util` and `http-body-util`), `ureq`, `serde`, `toml`, and
  `openairplay2` (the embedded AirPlay receiver, pure Rust, pulled from
  its git repo).
- **Async is for the control plane only.** The HTTP API (and later the
  embedded AirPlay receiver) runs on a current-thread tokio runtime;
  blocking calls in handlers go through `spawn_blocking`. The audio path —
  player thread, pipeline, sinks — stays on dedicated OS threads with
  blocking I/O: ALSA's blocking `writei` *is* the pacing mechanism. Never
  make the audio path async; channels bridge the two worlds.
- Audio output goes through an `AudioSink` trait. The ALSA implementation is
  Linux-only (`#[cfg(target_os = "linux")]` — the `alsa` crate does not
  compile on macOS); a dev sink (null / WAV file) keeps the crate building
  and testing on macOS and lets tests assert on the samples actually
  written (e.g. that the gain clamp held).

## Development environments

- **Mac (primary):** day-to-day development. Everything except the ALSA sink
  builds and tests here. Needs `ffmpeg` + `pkg-config` from Homebrew;
  bindgen uses Xcode's libclang. `cargo test`, `clippy`, `fmt` must always
  be green on macOS.
- **Debian PC (integration):** real-ALSA testing with its own sound
  card — test here regularly when audio code changes, since macOS gives
  no signal on the ALSA/Linux side. Same Debian library ecosystem as
  Raspberry Pi OS.
- **Any Debian 13 environment (packaging):** `.deb`s for amd64, arm64
  and armhf build via `service/setup-build.sh` + `service/build-deb.sh`
  (native, or Debian-multiarch cross on an amd64 host — no Docker, no
  emulation, no sysroots); the GitHub release workflow runs the same
  scripts on public runners, and publishing a release is the only thing
  that builds .debs.
- **muzak — the radio (target only):** a Pi 4 (1 GB, arm64 Raspberry Pi
  OS trixie) with the studio speakers on a USB DAC; reachable over SSH
  as root for installing .debs and reading logs (see
  `notes/clean-install.md`). It runs release packages only — never
  develop or build on it (1 GB), and treat the speakers as live:
  conservative ceilings and volumes for any audible test.

### Website (`service/web/`)

- Server-rendered minijinja templates + vendored HTMX; no build
  toolchain, no framework, and plain no-JS forms must keep working
  (POST-redirect-GET).
- Iterate with `cargo run -- --sink null --web-dir service/web`:
  templates and assets are read from disk per request — edit, reload,
  no recompile. Release builds embed them in the binary.
- The page must stay a working PWA at the origin `http://<host>/`
  (port 80): installed home-screen icons are bound to that origin.

## Directory layout

```
service/       Rust daemon (the binary is named radiod)
service/web/   website templates + static assets (embedded at build time)
notes/         long-lived notes and the overall plan
plans/         per-step implementation plans (YYYYMMDD-NN-slug.md)
runbooks/      operational procedures (releasing, ...)
```
