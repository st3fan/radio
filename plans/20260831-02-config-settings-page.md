# Plan: a `/config` page for device settings

- **Date:** 2026-08-31
- **Status:** draft

## Background

Changing the device's identity and output today means SSH, an editor,
and a `systemctl restart`. The four most-likely-to-need-changing fields
are: the **hostname**, the **AirPlay name**, whether to **resume the
radio after AirPlay**, and the **ALSA output device**. This is the
"setup screen for the config file" idea (`notes/ideas.md:120`), which
already worked out the two hard architectural questions — *where the
writes go* and *who applies them* — and its answer survives into this
plan unchanged:

- **Writes go to state, not to the config file.** `/etc/radio/config.toml`
  is a dpkg conffile on purpose; if the daemon rewrote it, every upgrade
  would start prompting about a modified conffile. Runtime-edited
  settings belong in `/var/lib/radiod/state.toml` (the same file the
  volume already lives in, `plans/20260802-02-settings-persistence.md`),
  layered *over* the conffile at startup. The conffile stays the
  SSH/admin-owned base; the daemon is the only writer of its own state.
- **The daemon owns it, behind the API.** A `GET`/`POST /config` pair,
  with the website as just another client.
- **Changes take effect after restart** (agreed for v1): the audio
  device, the AirPlay advertisement, and the hostname are all resolved
  once at startup into long-lived objects, and rebuilding them live is a
  much bigger job than this feature needs.

## Goal

A phosphor `/config` page (and the JSON API it rides on) that reads and
writes the four settings — hostname, AirPlay name, resume-after-AirPlay,
ALSA output device — validates them in the daemon, and persists them to
state so they apply on the next restart. Nothing else changes: no new
dependency, no change to the mixer-ceiling invariant, and the two
dangerous fields (`listen`, the mixer ceiling) are deliberately absent.

## Design

### 1. State, not config — and what "hostname" is

The four fields become runtime-editable **state** that overrides config
defaults, exactly like `volume` already does:

- `PersistedState` (`service/src/state.rs`) grows four `Option` fields —
  `hostname`, `airplay_name`, `resume_radio`, `audio_device` — beside
  `volume`. `Option` means "not overridden; use the config value", so
  existing state files load cleanly and a `reset` is just deleting the
  keys.
- At startup, after `state::load`, each present field is folded into the
  in-memory `Config` in place — the same single-line pattern that already
  applies the saved volume (`main.rs`). Everything downstream
  (`make_sink`, `make_mixer`, the AirPlay `Receiver`, the player) keeps
  reading from `Config` unchanged.
- One new config key, `hostname` (top-level, optional): the mDNS name
  radiod advertises, **independent of the OS hostname**. Absent/empty →
  today's behavior (the OS hostname, via openairplay2's own Avahi
  registration). This is the one field with nothing to read from today;
  see §3 for the advertisement.

A shared `Settings` hold isn't needed at startup; the values stay in
`Config`. But `/config` needs a *live* view of the current (possibly
just-saved) values, so `App` gains a small `settings: Arc<Mutex<Settings>>`
whose four fields mirror the effective config. `POST /config` updates it
and the saver persists it; `GET /config` reads it. `Settings` is seeded
from the overridden `Config` at startup.

### 2. The four fields, and validation

Validation lives in the daemon (the write path), not the form — the page
can offer whatever it likes, the daemon is the gate. All of these are
rejected with a useful `400 {"error": …}` and persisted only when clean.

| Field          | Type   | Rules |
|----------------|--------|-------|
| `hostname`     | string | Optional. A single DNS label: `[a-z0-9]` plus `-`, no leading/trailing `-`. No dots (the `.local` domain is appended). Empty clears it. |
| `airplay_name` | string | Non-empty (same rule as `[airplay] name`); run through the same trim/whitespace sanity `Config` already does. |
| `resume_radio` | bool   | — |
| `audio_device` | string | Non-empty. Best-effort probe: try opening it via ALSA and report "cannot open" as an error (advisory, not a hard block — see Recovery). |

**Deliberately out of the form** (`notes/ideas.md`): `listen` (the bind
that keeps the daemon off the LAN) and the mixer ceiling/`max_volume`
(the safety cap — `CLAUDE.md`). Both stay SSH/root-only; a web form must
not touch the ceiling, and the `/config` design never re-asserts it.

### 3. The hostname advertisement (riskiest piece)

openairplay2 owns Avahi today: `Receiver::run()` with `advertise(true)`
publishes `_airplay._tcp` with an empty host, so the service lands on
the **OS** hostname. A custom hostname needs radiod to own the
registration instead. The mechanism:

- Build the receiver with **`.advertise(false)`** and use its existing
  **`txt_records()`** for the service's TXT.
- radiod publishes both, through Avahi's D-Bus API via **zbus** (already
  a transitive dep of openairplay2, so this is a direct-dependency
  promotion with justification, not a new crate): the `_airplay._tcp`
  service **and** the hostname's address record, so
  `<hostname>.local` resolves to this box.

**Open points to settle against real Avahi** (the Debian PC has
avahi-daemon; this is the validation step, not a guess): whether an
`AddService` with a non-OS host also needs an explicit address record,
and the exact `AddService`/`AddAddress`/`AddRecord` calls. If it turns
out Avahi won't serve a second hostname cleanly, the fallback is a note
on the page that `hostname` requires a daemon restart plus the hostname
shown read-only — but try the real thing first. When `hostname` is
unset, `advertise` stays `true` and nothing changes.

### 4. The API

Two (three) endpoints, gated by the existing `[web] password` gate like
every other path — no new auth work; `/config` is simply not on the
open-path list, so it is protected whenever the password is set (and
open when it isn't, consistent with everything else). Worth a review
note: a reconfiguration page is a bigger blast radius than changing a
station; if that worries us, the answer is already there (set a
password), not a second mechanism.

- **`GET /config`** → `200` with the four current values, e.g.
  `{"hostname":"muzak","airplay_name":"Radio","resume_radio":true,"audio_device":"plughw:0,0"}`.
- **`POST /config`** → a JSON body with any subset of the four fields;
  validated + persisted. Returns the full `GET /config` object on
  success, `400 {"error":…}` on a bad field. (Empty/shape errors mirror
  the existing `/volume` handling.)
- **`GET /devices`** (phase 2, Linux-only) → the ALSA devices radiod can
  enumerate (`alsa` crate `HintIter`), so the page can offer a picker
  instead of a typed field. On non-Linux it returns `[]`.

### 5. The page

A new `web/config.html`, same phosphor language as the login page: the
banner, a `> CONFIG` prompt, one field per setting (hostname text,
AirPlay name text, resume-radio as a `[RESUME ON]/[OFF]` toggle, ALSA as
a text field or a `<select>` fed by `/devices` where available), a
`[SAVE]` that POSTs, a `[RESET DEFAULTS]` that clears the overrides, and
a dim line `CHANGES APPLY AFTER RESTART`. Plain POST-redirect-GET; the
existing `[LOCK]` and theme chrome apply. New `config.extra`/context
fields carry the current values into the template.

**Recovery** (`ideas.md`): the page is served by the same always-up HTTP
server, so a bad `audio_device` never locks the page out; the
test-before-save probe plus `[RESET DEFAULTS]` close the loop. A broken
device still fails loudly at the next start (as today), giving the
operator the message the log already gives.

## Phases

One stack: this plan as the bottom PR, then four implementation PRs.

### Phase 1 — the state layer

`hostname` in `Config` (+ `RawConfig`, example, validation), the four
`Option` fields on `PersistedState`, startup folding into `Config`, the
`Settings` holder + `App.settings`, and the saver snapshotting settings
alongside volume. Tests: config parse/default/reject; state round-trip
and precedence over config; saver writes settings only on change; old
state files still load.

### Phase 2 — the `/config` API

`GET /config`, `POST /config` (validation per §2) and `GET /devices`
(cfg-gated). Tests: get returns seeded settings; a valid subset posts
and persists; each invalid field is a 400 naming the field; devices
lists cards on Linux and is empty elsewhere; the gate 401s `/config`
without the cookie when a password is set.

### Phase 3 — the page

`config.html`, the route, render context, the resume toggle, the
pick/text field for ALSA, save/reset, the restart note, and a small
`style.css` addition for the toggle row. Tests: page renders current
values; save and reset follow POST-redirect-GET; reset clears the
override.

### Phase 4 — the hostname advertisement

radiod's own Avahi registration via zbus (`advertise(false)` +
`txt_records()`), the hostname address record, and the validation on the
Debian PC that `<hostname>.local` resolves and Apple devices still see
the receiver. This is its own PR because it is the one phase with real
upstream/OS uncertainty; phases 1–3 don't depend on it (until it lands,
a set `hostname` is simply persisted and applied at restart with no
visible effect, and `advertise` stays `true`).

## Acceptance criteria

- The four settings appear on `/config`, save to state, survive restart
  and package upgrade, and are re-applied on the next daemon start; a
  freshly-installed box with no state file behaves exactly as before.
- Each invalid value (empty AirPlay name, malformed hostname, missing
  audio device) is rejected by the daemon with a field-naming 400; a
  probe opening the configured device succeeds before save.
- `[RESET DEFAULTS]` clears the four overrides and returns to the
  conffile's values.
- `listen` and the mixer ceiling are nowhere on the page or in the API;
  the ceiling invariant is untouched (no new path near ALSA gain).
- With `hostname` unset, mDNS behaves exactly as today; with it set,
  `<hostname>.local` resolves to the radio on the Debian PC and the
  AirPlay receiver is still discoverable (phase 4).
- `cargo test`, `clippy`, `fmt` green on macOS and Linux; no new crates
  (zbus is already a transitive dep, promoted to direct).
