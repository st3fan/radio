# Plan: replace max_volume with ALSA mixer ownership (milestone 10)

- **Date:** 2026-08-01
- **Status:** draft
- **Order:** second of three (async → mixer ownership → AirPlay embed).
  Milestone 9 (async) is merged; this must land before the AirPlay embed
  so AirPlay volume maps onto the settled volume model.

## Background

Today the speaker-protection invariant is `max_volume`: a software cap
(default 50) applied as gain in the digital path, enforced by construction
through `volume::effective_volume()`. It works, but it has two costs:

1. **Resolution.** Capping at 50 % in software throws away a bit of every
   sample, permanently. The DAC never sees full-scale audio.
2. **It doesn't compose.** Milestone 11 adds a second audio producer
   (AirPlay). Every producer must faithfully route through the software cap,
   and AirPlay's volume model (dB, where 0 dB = full scale) has an impedance
   mismatch with "percent of a percent".

The replacement: **radiod owns the ALSA mixer.** The hardware/master output
level is set to a configured ceiling once, by radiod; the digital path runs
at full scale; each source's 0–100 volume becomes an ordinary volume
control. The physical ceiling protects the studio speakers no matter which
source is playing or what its software does.

### The safety trade, stated honestly

The invariant stops being "enforced by construction in one pure function"
and becomes **runtime state in the sound card**. Things that can move it
behind our back: `alsamixer`, `alsactl restore` at boot, a USB DAC
re-enumerating (these frequently come back at 100 %), a driver update.
The failure mode is full-scale output into studio speakers. The design
below is therefore belt *and* braces: the mixer is asserted, re-asserted,
and the software gain path still never exceeds unity.

## Scope

In:

- A `mixer` module (Linux-only, like `AlsaSink`) that owns one mixer
  control: open, set the ceiling, **read back and verify**, expose
  `assert_ceiling()` for re-checks.
- **Prefer the dB API** (`snd_mixer_selem_set_playback_dB` /
  `..._get_playback_dB`) when the element supports it — raw mixer ranges
  are often nonlinear, so "40 % of the range" may not be 40 % of anything.
  Fall back to the raw-range percentage only when the element has no dB
  info, and log which mode is in use.
- **Startup assertion:** if the configured mixer element doesn't exist, or
  the readback after setting doesn't match, radiod refuses to start audio
  (clear error in the log and in `/status`). Silently degrading to "no
  ceiling" is the one unacceptable outcome.
- **Re-assertion:** on every playback session start (each `/play`, each
  source open — cheap), and whenever the ALSA device is reopened after an
  error (the USB re-enumeration path). This piggybacks on code paths that
  already exist; no polling loop.
- Config: `max_volume` is replaced by a `[mixer]` section — `control`
  (element name, e.g. `"PCM"` or `"Speaker"`), `ceiling_db` (preferred) or
  `ceiling_percent`. `deny_unknown_fields` means old configs with
  `max_volume` fail loudly at startup with a message that names the
  migration; the `.deb`'s conffile example is updated.
- `volume.rs` simplifies: `effective_volume()` (the percent-of-cap mapping)
  is deleted; `gain()` maps 0–100 → `[0.0, 1.0]` with mute, and
  `apply_gain()` keeps its **never-amplify clamp** — that clamp is the
  software half of the belt-and-braces and keeps its exhaustive tests.
- Website/docs: remove "percentage of the cap" language; volume is just
  volume now.
- **CLAUDE.md invariant rewrite.** The current invariant says "never
  touch the hardware/ALSA mixer volume" — this milestone inverts that on
  purpose. The section becomes: radiod owns the mixer ceiling
  (asserted, verified, re-asserted), the software gain path never
  amplifies, and no code path may raise the mixer above the configured
  ceiling.
- **The milestone 9 drain finding** (recorded in
  `plans/20260801-09-async-tokio.md`): `AlsaSink` sets no buffer-size
  constraint, so on deep-buffer devices `snd_pcm_drain` in `close()`
  runs for seconds — `/stop` and `/pause` keep playing buffered audio
  and shutdown overran its 5 s deadline on the Debian PC. In scope
  here: constrain the ALSA buffer to a sane bound (~0.5 s) in
  `AlsaSink::open()`, and use `drop()` (discard) instead of `drain()`
  for stop/pause — stop should mean *now* on a live radio.

Out:

- Per-source volume balancing, tapers/curves, any UI beyond text changes.
- Non-Linux mixer support (macOS keeps null/wav sinks; the mixer module is
  `cfg(target_os = "linux")` with a trait + test fake, same pattern as
  `AudioSink`).
- Touching PCM routing or the sink write path (gain application stays where
  it is, in the pipeline).

## Design notes

- **Which element?** On USB DACs the playback element is usually
  `PCM` or `Speaker` on card N; the config names it explicitly rather than
  guessing. `amixer -c1 scontrols` is the operator's discovery tool; the
  startup error message should suggest exactly that command.
- **Trait for testability:** `trait MixerControl { fn set_ceiling(..);
  fn read_back(..) }` with the ALSA implementation Linux-only and a
  recording fake for tests — matching the house `AudioSink`/`TestSink`
  pattern, so the assertion/refusal logic is testable on macOS.
- `/status` gains a `mixer` field (`"ok"` / `"error: ..."`) so the website
  can surface a mixer problem instead of playing silently at an unknown
  level.
- Document (in the config example) that `alsactl`/`alsa-state` should be
  left enabled but harmless: radiod re-asserts on session start, so a boot
  `alsactl restore` is corrected the moment anything plays.

## Phases

One stack: this plan as the bottom PR, then one branch/PR per phase
stacked on top.

### Phase 1 — mixer module + config

The `MixerControl` trait, the ALSA implementation (dB-preferred, raw
fallback, readback verification), the `[mixer]` config section replacing
`max_volume` (with the loud migration error), startup ownership + refusal
wiring in `main`. Tests via the fake; manual verification on the Debian PC
(`amixer` shows the ceiling landing).

### Phase 2 — simplify the volume path + re-assertion + sink timing

Delete `effective_volume()`, rewire `gain()` to plain 0–100, keep the
never-amplify clamp and its exhaustive tests; re-assert on session start
and device reopen; `/status.mixer`; website text updates; `.deb` conffile
example + upgrade note; CLAUDE.md invariant rewrite. Plus the milestone 9
finding: bound the ALSA buffer in `AlsaSink::open()` and switch
stop/pause to `drop()` so they silence immediately (graceful shutdown
also stops overrunning its deadline).

Hardware validation on the Debian PC — the machine attached to the
actual speakers: ceiling lands (`amixer`), level parity with the old
50 % cap by ear/meter, `/stop` silences immediately, and the nasty
case — unplug/replug the USB DAC mid-play and verify the ceiling is
re-asserted before audio resumes. Validation on the new radio board
(arm64 Pi or Banana Pi) happens when that hardware arrives.

## Test strategy

- Unit (fake mixer): ceiling set on startup; readback mismatch → refusal;
  missing element → refusal with the suggested `amixer` command in the
  error; re-assert called on session start and reopen.
- Unit (volume): 0–100 → gain mapping, mute wins, clamp never amplifies
  (existing exhaustive tests, adjusted).
- Hardware: full-scale test tone at ceiling vs. the old 50 % software cap —
  confirm level parity by ear/meter before trusting it; USB replug case.

## Acceptance criteria

- Old config with `max_volume` fails at startup with a migration message.
- With the mixer at the ceiling, volume 100 from the website is no louder
  than the old capped maximum (measured on the actual speakers).
- Killing the mixer setting externally (`alsamixer` to 100 %) is corrected
  at the next session start.
- `/stop` and `/pause` silence the speakers immediately (no seconds of
  buffered tail), and graceful shutdown finishes inside its deadline.
- `cargo test`, `clippy`, `fmt` green on macOS and Linux; `/status`
  reports mixer health.
- CLAUDE.md's volume invariant describes the new model.
