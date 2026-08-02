# Plan: settings persistence (starting with the volume)

- **Date:** 2026-08-02
- **Status:** draft

## Background

Every restart of the daemon — a reboot, a `systemctl restart`, an
upgrade — resets the volume to `initial_volume`. The radio should
remember its settings across daemon restarts, device reboots, and
package reinstalls. First setting: the volume level. The mechanism
should be a small, growable state file, so later candidates (the last
station, mute, a future "resume where it was" behavior) are additive
changes, not redesigns.

## Design

### State, not config — and where it lives

Settings changed at runtime are **state**, exactly like the AirPlay
identity: they belong in the systemd `StateDirectory`
(`/var/lib/radiod/`), which survives upgrades, `apt remove`, and even
`apt purge` (dpkg never touches state directories; only the shipped
files and conffiles are its business). New file:
`/var/lib/radiod/state.toml`, e.g.:

```toml
volume = 35
```

- TOML via the serde stack already in the tree; human-readable and
  hand-editable like the config.
- `state_path` is configurable (like `identity_path`) for development;
  default `/var/lib/radiod/state.toml`.
- **State is convenience, not safety**: an unreadable, corrupt, or
  unwritable state file logs a clear line and falls back to config
  defaults — it must never refuse startup (contrast with the mixer
  ceiling, which does). The mixer ceiling continues to cap everything
  in hardware regardless of what any state file says.

### Load: precedence at startup

`state::load()` runs at startup: a saved volume overrides
`initial_volume`; `initial_volume` remains the first-boot (and
fallback) value and keeps its config meaning. Documented in the config
example.

### Save: an observer, not touchpoint sprawl

The volume is mutated in several places (JSON API, website actions,
future sources); instrumenting every site invites drift. Instead a
small **saver task** on the tokio runtime samples the shared status
every ~2 s and atomically rewrites the state file when the persisted
fields changed (write temp file + rename, same directory). Properties:

- Bounded SD-card wear (at most one small write per interval, and only
  on change) with no code at the mutation sites — future persisted
  fields are one struct field away.
- Graceful shutdown does a final save (the existing shutdown path).
- A hard power cut can lose at most the last ~2 s of knob-turning —
  acceptable for a volume level.

### Scope decision: volume only, deliberately

- **Muted is not persisted**: coming back from a power cycle silently
  muted is a "why is the radio broken" trap; a fresh start is unmuted.
- **The last station is not persisted yet** — it is the obvious next
  field, but auto-resuming playback after a reboot is a behavior
  decision (should a power blip un-pause the studio?) that deserves its
  own review; the file format is ready for it.

## Phases

One stack: this plan as the bottom PR, one implementation PR on top.

### Phase 1 — the state module

`service/src/state.rs`: load (with fallback semantics), atomic save,
the saver task, `state_path` config knob, startup wiring, config
example + README note. Tests: round-trip, corrupt-file fallback,
precedence over `initial_volume`, saver-writes-only-on-change (fast
interval + temp dir in tests). Hardware validation on muzak: set a
volume from the website, `systemctl restart radiod` → volume kept;
reboot the device → kept; reinstall the deb → kept.

## Acceptance criteria

- Volume survives daemon restart, device reboot, and package
  reinstall/upgrade on muzak.
- First boot (no state file) behaves exactly as today
  (`initial_volume`).
- A corrupt or unwritable state file degrades loudly-but-gracefully:
  logged, defaults used, daemon runs.
- `cargo test`, `clippy`, `fmt` green on macOS and Linux; no new
  dependencies.
