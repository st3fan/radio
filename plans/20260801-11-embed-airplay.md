# Plan: embed the openairplay2 library (milestone 11)

- **Date:** 2026-08-01
- **Status:** draft
- **Order:** third of three; milestones 9 (tokio runtime) and 10 (mixer
  ownership / settled volume model) are merged — the runway is clear.

## Background

[openairplay2](https://github.com/st3fan/openairplay2) is now an embeddable
library (its PRs #14–#17): a cargo workspace whose `openairplay2` crate owns
**network → PCM** — discovery advertisement, transient pairing, the
encrypted control channel, fp-setup, two-phase SETUP, per-packet decrypt,
AAC decode, and the session semantics (pause gate, seek/flush preemption,
backpressure pacing). The host owns **PCM → speaker**. Pause now *holds*
the buffered audio rather than dropping it (its PR #20), so resume is
immediate and sender-side timeline arithmetic stays honest.

The embedding API (all on the caller's tokio runtime):

```rust
let receiver = openairplay2::Receiver::builder()
    .name("Radio")                       // the AirPlay picker name
    .identity_path("/var/lib/radiod/airplay-identity")
    .build()?;                            // identity is REQUIRED & stable
receiver.run(sink_factory, event_sender).await
```

- `AudioSink { fn write(&mut self, pcm: &[i16]); fn flush(&mut self); }` —
  `write` is called from a library-managed thread and **may block; blocking
  is the pacing mechanism**. Only audio that should actually play arrives
  (the library withholds during pause and drops pre-seek audio itself).
- `SinkFactory = Arc<dyn Fn(rate, channels) -> Box<dyn AudioSink>>` —
  invoked at SETUP phase 2, once per stream.
- Events over an unbounded tokio mpsc: `SessionStarted { rate, channels }`,
  `Volume { db }` (AirPlay dB; the library does **not** apply gain),
  `Paused(bool)`, `Flushed`, `SessionEnded`. `#[non_exhaustive]`.
- `advertise(true)` (default) registers `_airplay._tcp` with the system
  Avahi daemon — radiod has no mDNS of its own, so we use this.

Result: one binary. Browse to muzak and pick a SomaFM channel, or pick
"Radio" in any Apple device's AirPlay list and stream to it.

## The risk that retired with the hardware

This plan originally gated everything on an ARMv6 go/no-go spike:
per-packet ChaCha20-Poly1305 plus AAC-LC decode on the Pi Zero's single
1 GHz ARM11 was unproven. The Zero is retired; the radio is now muzak, a
quad-core Cortex-A72 Pi 4 — for which that workload is not in doubt.
Phase 1 shrinks accordingly: still a standalone-receiver spike first,
but as a *device validation* (avahi registration, discovery on this LAN,
identity persistence, real senders, baseline CPU/RSS numbers), not a
performance gate.

## Design

### Decision record: monolith

Considered and rejected during plan review: splitting radiod into
services (streamer / api / AirPlay receiver) talking over D-Bus. The
deciding argument is the audio plane — the never-amplify gain path, the
single ALSA open, and the mixer-ceiling ownership are enforced *by
construction* inside one process; any split either gives two producers
device access or requires a PCM-over-IPC audio daemon that turns the
invariant into a convention. The control plane already has an IPC
surface (the loopback HTTP API), so a separate `radio-api` buys nothing.
The embed proceeds as designed — and deliberately keeps the would-be cut
line: AirPlay enters as a `Source` behind a bounded channel, so if the
receiver ever needs process isolation, the channel becomes a socket
without redesigning radiod. (Related idea recorded in `notes/ideas.md`:
absorbing the website into the binary and retiring PHP/lighttpd.)

### AirPlay is a source, not a second player

radiod's pipeline is pull-based (`source.read → apply_gain → sink`); the
library pushes PCM into an `AudioSink`. The bridge is a **bounded
`std::sync::mpsc` channel** (a few hundred ms of audio):

- The `AudioSink` impl handed to the library writes chunks into the channel;
  a full channel blocks `write` — which is exactly the backpressure the
  library's pacing model wants. `flush()` drains the channel.
- A new `AirplaySource` implementing radiod's `Source` trait reads from the
  channel. From the player's perspective AirPlay is just another station:
  PCM flows through the **existing** pipeline, through `volume::apply_gain`,
  into the existing `AlsaSink`. No second audio path, no second ALSA open,
  and the mixer ceiling from milestone 10 applies unchanged.

### Arbitration: AirPlay preempts radio

- `Event::SessionStarted` → the player switches to the AirPlay session; if a
  station was playing it is remembered (the existing pause machinery —
  `Session::Paused(Station)` — already models "remember and come back").
- `Event::SessionEnded` → configurable: resume the remembered station or go
  idle (`[airplay] resume_radio = true|false`, default `true`).
- `/play` during an AirPlay session: policy decision — reject with 409
  (`{"error": "airplay session active"}`) for v1. Kicking the AirPlay sender
  gracefully is more design than it is worth right now.
- `/status` gains `source: "radio" | "airplay"` plus an `airplay` object
  (active, rate/channels). The library does not yet expose the peer's name
  ("Stefan's MacBook") — noted as a wanted `SessionStarted` field in
  openairplay2's plan; when it lands, thread it into `/status` and the
  website.

### Volume

Two models meet here; keep them separate and explicit:

- The **website volume** (0–100 through `volume::gain`) stays the master
  software volume and applies to both sources — it is "the speaker's
  volume".
- The **AirPlay sender's slider** (`Event::Volume { db }`) maps
  `10^(db/20)` (with −144 dB → 0) onto a *separate* AirPlay-session gain
  factor, multiplied into the pipeline gain only while the AirPlay source
  is active. Both factors are ≤ 1, the never-amplify clamp still holds, and
  the mixer ceiling caps everything in hardware regardless.
- Out of scope: reflecting website volume changes back to the sender's
  slider (needs receiver→sender eventing; revisit in openairplay2).

### Configuration & packaging

```toml
[airplay]
enabled = true            # default true on Linux
name = "Radio"            # AirPlay picker name
port = 7000
resume_radio = true
```

- Identity at `/var/lib/radiod/airplay-identity` via systemd
  `StateDirectory=radiod` (senders remember the receiver by it — it must
  survive upgrades; it is state, not config).
- `.deb`: `Recommends: avahi-daemon` (the library registers over D-Bus;
  without avahi, AirPlay is off and radio still works — degrade, don't
  fail). Startup logs one clear line either way.
- macOS dev: the library builds and tests on macOS (no ALSA in it), so the
  whole radiod crate keeps compiling and testing on the Mac with the
  null/wav sinks; `enabled = false` is the natural macOS default.

## Phases

One stack: this plan as the bottom PR, then one branch/PR per phase
stacked on top.

### Phase 1 — standalone receiver on muzak (device validation)

Build `openairplay2-receiver` for arm64 (native in CI-style container or
the arm64 multiarch cross path — whatever its dependency tree prefers;
this is also the first look at what that tree needs). Run it standalone
on muzak while radiod is stopped: verify avahi is present and the
`_airplay._tcp` registration appears, pair and stream from Stefan's Mac
and iPhone (pause/resume, seeks, a long session), confirm identity
persistence across restarts, and record CPU/RSS numbers in this plan.
Expected to be a formality on the A72; anything surprising gets recorded
here before phase 2.

**Results (2026-08-01):** the library turns out to be pure Rust
(symphonia for AAC, RustCrypto ciphers, tokio-flavored zbus for Avahi —
no pkg-config or bindgen anywhere in the lib; only the standalone
binary adds `alsa`), so the arm64 cross-build needed nothing beyond the
existing multiarch environment. On muzak: the `_airplay._tcp`
advertisement appears immediately (listed alongside the household's
real AirPlay devices), the identity keypair persists across restarts,
and the idle footprint is ~5 MB RSS at <1 % CPU. The spike receiver is
left running as **"Radio (spike)"** with radiod active beside it —
outstanding items, which need Stefan's Apple devices: pair, stream,
pause/seek, a long session, and under-load CPU numbers.

### Phase 2 — embed and arbitrate

Add the git dependency; spawn `Receiver::run` on the tokio runtime (from
milestone 9); the channel-bridged `AudioSink` → `AirplaySource`; player
arbitration (preempt, remember, resume policy); volume-event mapping;
`[airplay]` config; `/status.source`. Tests: a fake AirPlay side (drive
the `AudioSink`/events directly — no network needed) covering preemption,
resume, flush draining the bridge, dB mapping, and the 409 on `/play`.

### Phase 3 — surface & ship

Website: show the AirPlay state (source badge, "AirPlay active" instead of
station controls, volume still usable); `.deb` packaging (StateDirectory,
avahi Recommends, config example); end-to-end hardware validation on
muzak with Stefan's Apple devices: radio → AirPlay preemption →
pause/resume/seek from the phone → session end → radio resumes; both
volume paths; the mixer ceiling verified (via `amixer` and ears) to hold
under AirPlay at slider-max.

## Test strategy

- Unit/integration on macOS: everything in phase 2's list runs with the
  null sink and a driven fake — the library's seam was designed for exactly
  this.
- The openairplay2 library's own test suite stays upstream; radiod tests
  the *bridge and arbitration*, not the protocol.
- Hardware is the real acceptance: a phone and a Mac against muzak, long
  sessions, and the milestone-10 USB-replug case repeated while an
  AirPlay session is active.

## Acceptance criteria

- One `radiod` binary/deb: SomaFM via the website *and* AirPlay from
  Apple devices, on muzak, with the phase-1 baseline numbers holding in
  the integrated build.
- AirPlay preempts radio; end of session honors `resume_radio`; `/status`
  and the website reflect the active source truthfully.
- Both volume controls behave; nothing can exceed the mixer ceiling.
- `cargo test`, `clippy`, `fmt` green on macOS and Linux.

## Open questions

- Whether `enabled = true` should require the identity dir to be writable
  at startup (fail fast) or lazily on first pairing — lean fail-fast.
- Bridge channel depth (latency vs. underrun margin on the Pi) — pick by
  measurement in phase 2, not by taste.
- The AirPlay picker name: fixed config vs. reusing the website's device
  name if one ever exists — config for now.
