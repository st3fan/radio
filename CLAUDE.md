# Radio

An internet radio player for a Raspberry Pi Zero W connected to studio
speakers. Two components:

- `radiod/` — Rust daemon with an HTTP REST API on `127.0.0.1` that plays a
  stream (from a `.pls` playlist URL) to a preconfigured ALSA device.
  Streaming, demuxing and decoding are done in-process via the FFmpeg
  libraries (libavformat/libavcodec, through the `ffmpeg-next` crate); ALSA
  output via the `alsa` crate. The distro's libav*/libasound shared
  libraries are runtime dependencies on the Pi.
- `web/` — PHP website on the same Pi that lists SomaFM channels and controls
  the daemon (server-side; the browser never talks to the daemon directly).

The overall design and roadmap live in `notes/plan.md`. Read that first.

## Critical invariant: maximum volume

The audio level sent to the device can **never** exceed the configured
`max_volume`. If not configured, the maximum is **50** (0–100 scale). This is
a safety requirement — the Pi drives studio speakers. All decoded PCM flows
through our code, and the gain applied to it must come from a single,
unit-tested clamping function (`min(volume, max_volume)`, or 0 when muted).
Never add a code path between decoder and ALSA that bypasses it, and never
touch the hardware/ALSA mixer volume.

## Hardware constraints

Target is a Pi Zero W 1.0: single-core 32-bit **ARMv6** (Rust target
`arm-unknown-linux-gnueabihf`, not armv7), 512 MB RAM. Prefer small, blocking,
few-dependency solutions over heavy frameworks and async runtimes.

## How we work: plan-based development

Every time we work on a step of this project, we first write an
implementation plan as `plans/YYYYMMDD-NN-slug.md` (e.g.
`plans/20260729-01-daemon-skeleton.md`, where `NN` is a sequence number for
that day). The plan describes what will be built and how.

- If the work is complicated, the plan lists **phases**, where each phase is
  its own pull request.
- Plans are written before implementation starts, so they can be reviewed.

## Pull requests

- All work lands via pull requests — no direct commits to `main`.
- Stefan reads and approves PRs before they merge.
- Single-PR work: branch from `main`, open a PR against `main`.
- Multi-PR work (a plan with phases): create an **integration branch** from
  `main` (e.g. `integration/daemon-playback`), open each phase's PR against
  that branch, and when the whole plan is done, merge the integration branch
  to `main` with one final PR.

## Coding notes

### Rust (`radiod/`)

- Run `cargo test`, `cargo clippy` and `cargo fmt` before considering a
  change done.
- Keep dependencies minimal and justify new ones in the PR description. The
  approved core set: `ffmpeg-next`, `alsa`, `tiny_http`, `ureq`, `serde`,
  `toml`. No async runtimes.
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
- **Debian PC (integration):** connected to the actual speakers. Used over
  SSH for real-audio testing with real ALSA from milestone 2 onward — test
  here regularly once audio code exists, since macOS gives no signal on the
  ALSA/Linux side. Same Debian library ecosystem as Raspberry Pi OS. Also
  the likely place to produce the ARMv6 cross-compiled binary.
- **Pi Zero (target only):** deployment target. Never develop or build on
  it — one ARMv6 core and 512 MB make that a non-starter.

### PHP (`web/`)

- Plain PHP, no framework. Start functional and unstyled; iterate on looks
  later.
- All calls to the daemon happen server-side against `127.0.0.1`.

## Directory layout

```
radiod/   Rust daemon
web/      PHP website
notes/    long-lived notes and the overall plan
plans/    per-step implementation plans (YYYYMMDD-NN-slug.md)
```
