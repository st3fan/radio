# Plan: Daemon skeleton (milestone 1)

Create the `radiod` cargo project with configuration loading, the volume
clamp, and a minimal HTTP server on `127.0.0.1` whose `GET /status` returns
stubbed state. No audio yet — this PR establishes the project structure, the
config format, and the safety-critical volume logic with tests.

Single PR against `main` (no phases needed).

## Scope

In:

- `radiod/` cargo project (binary crate).
- Config loading from TOML with defaults, `--config` flag.
- The volume clamp function — the single place gain is computed — with
  thorough unit tests.
- `tiny_http` server bound to the configured listen address,
  `GET /status` returning stubbed JSON, JSON 404 for everything else.
- CI-friendliness: `cargo test`, `cargo clippy`, `cargo fmt` all clean.

Out (later milestones):

- Any FFmpeg/ALSA code or dependency.
- `/play`, `/stop`, `/pause`, `/resume`, `/volume`, `/mute`, `/unmute`
  (added in milestones 2–3; they 404 for now).
- systemd unit, cross-compilation setup.

## Project setup

```
radiod/
  Cargo.toml
  src/
    main.rs        arg parsing (--config, -v), wiring
    config.rs      Config struct + TOML loading + defaults
    volume.rs      clamp + gain functions and their tests
    status.rs      Status struct + JSON serialization
    server.rs      tiny_http accept loop and routing
```

Dependencies: `tiny_http`, `serde` (derive), `serde_json`, `toml`. Nothing
else. Edition 2021; pin a modest `rust-version` so it stays buildable with
the toolchain we'll use for ARMv6 cross-compilation.

Arg handling is `std::env::args` by hand — two flags don't justify a crate:

- `--config <path>` (default `/etc/radio/config.toml`; if the default path
  doesn't exist, run with built-in defaults so `cargo run` just works)
- `-v` / `--version` prints the version and exits

## Config (`config.rs`)

```toml
listen = "127.0.0.1:8080"
audio_device = "plughw:1,0"
max_volume = 50
initial_volume = 25
```

- Every field optional in the file; struct built via `Default` + overrides.
  Defaults: exactly the values shown above.
- Validation on load: `max_volume` ≤ 100, `initial_volume` gets clamped to
  `max_volume` (through the volume module, not ad hoc), `listen` must parse
  as a `SocketAddr`.
- A non-loopback `listen` address is refused with a clear error. The API is
  loopback-only by design; if we ever regret this we'll loosen it
  deliberately in a reviewed PR, not by accident.
- Unknown keys in the TOML are an error (`serde(deny_unknown_fields)`) so
  typos like `max_volum = 90` can't silently leave the default in place.

Tests: defaults when fields are missing, full file parse, rejection of bad
values (volume > 100, unknown keys, non-loopback listen, malformed TOML).

## Volume (`volume.rs`)

The safety-critical module. Two small pure functions:

```rust
/// The only way a requested volume becomes an effective volume.
pub fn effective_volume(requested: u8, max_volume: u8) -> u8;   // min(requested, max)

/// The only way an effective volume becomes a sample multiplier.
pub fn gain(volume: u8, muted: bool) -> f32;                    // 0.0 when muted, else volume/100
```

Later milestones must route *all* gain through these — that rule is already
in CLAUDE.md. Tests: requested below/at/above max, 0, 100, muted-overrides-
volume, gain bounds (never > max_volume/100), monotonicity.

## Status (`status.rs`)

The full status shape from `notes/plan.md`, serialized with `serde_json`:

```json
{
  "state": "stopped",
  "playlist_url": null,
  "stream_url": null,
  "icy_title": null,
  "icy_name": null,
  "volume": 25,
  "muted": false,
  "max_volume": 50
}
```

In this milestone the daemon holds a `Mutex<Status>`-style shared state
initialized from config (`state: "stopped"`, `volume: initial_volume`); only
`/status` reads it. The command channel and playback thread arrive in
milestone 2 — no speculative scaffolding for them now.

Test: serialization matches the documented shape (field names, nulls).

## Server (`server.rs`)

- `tiny_http::Server::http(config.listen)`, plain blocking accept loop —
  requests handled sequentially. One caller (the PHP site) and trivial
  handlers; no thread pool until profiling says otherwise.
- Routes: `GET /status` → 200 with status JSON. Anything else → 404
  `{"error": "not found"}`. All responses `Content-Type: application/json`.

## Acceptance

- `cargo test`, `cargo clippy` (no warnings), `cargo fmt --check` pass in
  `radiod/`.
- `cargo run` (no config file present) then
  `curl http://127.0.0.1:8080/status` returns the stubbed JSON with
  `volume: 25`, `max_volume: 50`.
- A config file setting `max_volume = 30` and `initial_volume = 80` yields
  `volume: 30` in `/status` — proving the clamp is wired in from day one.
