# Plan: debug mode for silent streaming stalls

- **Date:** 2026-08-29
- **Status:** draft

## Background

Sometimes the audio stops while the website (and `GET /status`) keeps
saying `playing`. This is partly by design — `playing` means "trying to
play", and the reconnect loop deliberately rides out network trouble
without dropping the state — but nothing in the daemon can currently
tell us *which* kind of stop this is. The process has no notion of
audio progress: once `state` is `Playing`, the only evidence of whether
samples are actually reaching ALSA is your ears.

The goal of this step is **observability, not the fix**: add a debug
mode that records what the audio path is doing, so the next stall on
muzak can be diagnosed from the couch (`curl http://radio/debug`) or
from the journal afterwards, and the follow-up fix targets the real
cause instead of a guess.

### What could be happening (the hypothesis space)

Reading the current code, a silent stall with `state = playing` has
four plausible mechanisms, with distinct signatures the debug mode must
be able to tell apart:

1. **The player thread is blocked forever inside `source.read()`** —
   the prime suspect. `FfmpegSource::open` sets the `reconnect*`
   options but **no `rw_timeout`**, so a half-open TCP connection (the
   server or a NAT/Wi-Fi hop dies without sending FIN/RST) leaves
   `av_read_frame` blocking indefinitely. libavformat's reconnection
   only helps when the socket *errors*; a socket that simply goes
   quiet blocks forever. Signature: the play loop stops iterating
   entirely — and because commands are only polled between chunks,
   `/stop` and `/pause` return 200 but never take effect until the
   read returns (possibly never). The reconnect log lines never
   appear.
2. **The reconnect loop is failing forever.** The stream URL resolved
   from the `.pls` went stale, or DNS broke: every reopen fails, the
   backoff sits at 30 s, and the state stays `playing` by design. The
   journal does show `reconnecting to … in 30.0s` lines, but nothing
   is visible from the UI or API.
3. **The player thread is blocked in `sink.write()`** — the USB DAC
   hiccuped or re-enumerated and `writei` (or the recover-and-retry
   loop around it) never completes. Same "loop stopped iterating"
   signature as (1), but stuck at a different stage.
4. **Data arrives, but too slowly or as silence.** The icecast server
   itself underruns (sends below real time) or the decoder produces
   zeros. Reads succeed, so the loop looks alive — only the *rate* of
   samples reveals it.

(The AirPlay path is not a suspect for this symptom: its source reads
with a 50 ms timeout and substitutes silence, so it cannot silently
block the loop, and the UI clearly shows AirPlay mode.)

## Design

### Always-on pipeline heartbeat, off the API contract

A new `debug` module owns a `PipelineHealth` snapshot behind its own
`Arc<Mutex<…>>` — deliberately **not** part of `Status`, so the
`/status` contract and the UI stay untouched. The player thread updates
it at the natural points of the play loop:

- `stage` + `stage_entered_at`: which blocking call the loop is in
  (`connecting`, `reading`, `writing`, `backoff`, `idle`, `airplay`).
  Crucially the stage is written **before** each blocking call, so an
  observer sees where the thread is stuck *while* it is stuck —
  hypotheses (1) and (3) are directly readable even when the loop
  never comes back.
- `last_read_at`, `last_write_at`, and cumulative sample counters for
  the current session: distinguishes "loop is dead" from "loop is slow"
  from "everything flows" — hypothesis (4) falls out of the sample
  rate.
- `session_started_at`, `connect_attempts`, `current_backoff`,
  `last_error` (message + timestamp): hypothesis (2) shows up as a
  climbing attempt counter with a fresh error.

Timestamps are monotonic (`Instant`) internally and serialized as ages
(`…_ms_ago`), which sidesteps clock questions. The extra cost per chunk
is one short mutex lock next to the two the loop already takes — noise
at ~23 ms per chunk, fine for the 512 MB targets.

### A small event ring buffer

The heartbeat answers "what is it doing *now*"; a bounded ring buffer
(last ~100 events, wall-clock timestamped) answers "what happened at
3 AM": connect start/success/failure, EOF, read/write errors, stall
enter/exit, each command processed. A few kilobytes, fixed size.

### A stall monitor that does not live on the player thread

Detection cannot run on the thread being diagnosed — it may be blocked.
A tokio interval task (the control plane's job, like the state saver)
samples the heartbeat every few seconds: when `state == playing` and
`last_write_at` is older than a threshold (~10 s), it logs one line on
the transition into (and out of) the stall, including the stuck stage
and ages. That plants timestamped evidence in the journal even when
nobody is watching, and pushes a `stall` event into the ring buffer.

### `GET /debug` and a `/debug` page

- **`GET /debug`** returns the heartbeat snapshot plus the event ring
  as JSON. Documented in `notes/api.md` as a diagnostic endpoint whose
  shape is explicitly *not* part of the stable API contract. Read-only,
  exposes nothing sensitive — the same LAN posture as everything else.
- **`/debug` website page**: a minimal server-rendered table of the
  same snapshot with the recent events underneath, auto-refreshing the
  HTMX way; a plain reload keeps working without JS. Not linked from
  the main page — it's a tool, reachable by URL.

### Optional verbose FFmpeg logging

libavformat has opinions during reconnects that we currently throw
away. A `--debug` flag (and matching `debug = true` config knob)
installs an `av_log` callback forwarding warnings and info lines to
stderr, prefixed `radiod: ffmpeg:`, with basic rate limiting. Off by
default; on muzak it can be flipped on in the unit file or config when
hunting.

### Deliberately out of scope: the fix

The likely remedies are already visible — `rw_timeout` on the FFmpeg
open, a watchdog that tears down a stalled session and lets the
existing reconnect loop take over, maybe surfacing "stalled" to the
UI — but choosing among them before knowing the actual mechanism is
guesswork. The fix is its own plan, written once the debug data has
pointed at the culprit. The heartbeat fields are chosen so each
hypothesis above confirms or eliminates cleanly.

## Phases

One stack: this plan as the bottom PR, three implementation PRs on top.

### Phase 1 — heartbeat, events, stall monitor

The `debug` module (`PipelineHealth`, stages, ring buffer), player-loop
instrumentation for both the radio and AirPlay paths, and the monitor
task with its journal lines. Tests drive the player with a source that
blocks forever (and one that dribbles below real time) and assert the
recorded stage, the ages, and the stall transition — the dev sinks make
this macOS-testable.

### Phase 2 — expose it: `GET /debug` and the website page

The JSON endpoint, the server-rendered page, and the `notes/api.md`
section marking the shape unstable. Route tests alongside the existing
server tests.

### Phase 3 — `--debug` FFmpeg log capture

The flag + config knob, the `av_log` callback with rate limiting, and a
README/config-example note. Smallest phase, last because it is the
least likely to be needed.

## Reproducing it deliberately (the network-interruption hunch)

The working hunch is that a network interruption puts the app in this
state. An interruption breaks the connection in one of two ways, which
map straight onto hypotheses 1 and 2 — and both are reproducible on
the Debian PC with the debug build playing a real stream:

- **Silent break** (half-open connection — a NAT entry expiring, an AP
  rebooting, a hop going dark): simulate with
  `iptables -I INPUT -s <stream-ip> -j DROP` mid-playback (or pull the
  Ethernet cable). TCP reports nothing, so with no `rw_timeout` the
  read should block forever. Expected `/debug` signature: `stage:
  reading` with ever-growing ages, no new events — and `/stop`
  accepted but never processed.
- **Clean break**: same rule with `-j REJECT --reject-with tcp-reset`.
  The read errors out and the reconnect loop should take over.
  Expected: `connect_attempts` climbing, backoff visible, recovery
  when the rule is removed — unless the resolved stream URL has gone
  stale, which is the permanent variant of hypothesis 2.

If the DROP experiment reproduces the exact field symptom, the hunch
is confirmed and the fix plan writes itself (`rw_timeout`, letting the
existing reconnect loop do its job).

## Using it (the analysis procedure)

When the radio goes quiet with the UI showing playing:

1. `curl http://radio/debug` (or open `/debug`).
2. Read the signature:
   - `stage: reading` with `stage_entered_ms_ago` huge and no recent
     events → hypothesis 1 (blocked read, missing `rw_timeout`).
   - `stage: backoff`/`connecting`, `connect_attempts` climbing,
     recent errors → hypothesis 2 (reconnect loop failing; the error
     text says why).
   - `stage: writing` stuck → hypothesis 3 (ALSA/DAC).
   - Loop alive but sample counters advancing well below the spec's
     rate → hypothesis 4 (starving stream).
3. `journalctl -u radiod --since …` for the stall-monitor lines and
   (if enabled) the FFmpeg chatter around the moment it stopped.
4. Paste the JSON into the follow-up fix plan as evidence.

## Acceptance criteria

- With a stalled source (test double), `/debug` shows the stuck stage
  and growing ages, and the monitor logs the stall transition once.
- During normal playback on muzak, `/debug` shows flowing counters and
  the page refreshes; `/status` and the UI are byte-for-byte unchanged.
- The ring buffer and heartbeat have fixed, small memory bounds.
- `cargo test`, `clippy`, `fmt` green on macOS and Linux; no new
  dependencies.
