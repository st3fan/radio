# Plan: fix the silent streaming stall (rw_timeout + watchdog)

- **Date:** 2026-08-30
- **Status:** draft

## Background

The debug mode from `plans/20260829-01-streaming-stall-debug.md` (now in
the daemon: the `/debug` heartbeat, event ring, and stall monitor) was
built to point at the real cause of the silent stall — audio stops while
`/status` still says `playing` — before we chose a fix. It has. Running
the `runbooks/stall-testing.md` scenarios against a live SomaFM stream on
the dev Pi reproduced the field symptom and named the mechanism:

- **Silent break** (`iptables`/`nft` drop of the stream server's packets,
  no FIN/RST): audio stops within the buffer, `/debug` shows
  `stage: reading` with the age growing without bound, no new events, and
  `stalled` after ~10 s. `/stop` returns 200 but is **never processed** —
  the player thread is blocked inside `av_read_frame` and cannot poll the
  command channel. The daemon has to be restarted. This is hypothesis 1
  from the debug plan, confirmed.
- **15 % packet loss** produced the same `stage: reading` wedge, which
  then **self-healed after ~50–70 s** once libavformat's *own* internal
  read finally errored. That long, silent gap — recovering eventually, on
  FFmpeg's timing, not ours — is exactly what the field reports describe.

The root cause is concrete: `FfmpegSource::open` sets the `reconnect*`
options but **no `rw_timeout`**, so a socket that goes quiet without
erroring leaves `av_read_frame` blocking indefinitely. libavformat's
reconnection only helps when the socket *errors*; a half-open connection
never does. The player's own reconnect-with-backoff loop is sound — it
just never gets control back, because the blocking read never returns.

## Goal

Turn the indefinite wedge into a bounded, self-healing reconnect, and
make the rare case where the audio path itself is stuck visible and
recoverable — without weakening the reconnect behavior that correctly
rides out ordinary network blips, and without touching the mixer-ceiling
invariant.

## Design

Two independent layers, smallest-blast-radius first.

### 1. `rw_timeout` on the FFmpeg open (the fix)

Set libavformat's `rw_timeout` (and the equivalent `timeout` for the
protocols that read it) in `FfmpegSource::open`, so a read that stalls
returns an error instead of blocking forever. The error is exactly the
signal the existing player loop already handles: `read()` returns
`Err`, `play()` breaks with `Outcome::SourceEnded`, and
`play_with_retries` reopens with backoff — the same path the "clean
break" scenario already recovers through today.

Design points to settle in review:

- **Value.** Long enough not to abort a legitimately slow but alive
  stream (a low-bitrate station over a congested hop), short enough that
  a real stall becomes a reconnect in seconds rather than a minute of
  silence. A first cut around **10 s** matches the stall monitor's
  threshold; make it a `[stream]`/tuning knob rather than a bare
  constant so muzak can adjust without a rebuild, defaulting to the
  chosen value.
- **Units.** `rw_timeout` is microseconds; document it in whatever unit
  the config exposes (seconds) and convert at the edge.
- **Interaction with `reconnect*`.** Confirm on the bench that
  `rw_timeout` fires on a half-open socket *before* libavformat's own
  reconnect swallows it — the silent-break scenario is the test. If the
  two interact badly, prefer our loop (it drives the backoff, the
  heartbeat, and the journal lines) and reconsider the `reconnect*`
  options.
- **Distinguish stall from EOF.** A timeout and a clean end of stream
  both surface as `SourceEnded` today. That is fine for control flow
  (both reconnect), but the heartbeat/event ring should tell them apart
  so `/debug` reads honestly — a `read_timeout` event distinct from
  `eof`.

### 2. A session watchdog (belt and braces)

`rw_timeout` covers the network read — hypothesis 1, the confirmed
cause. It does **not** cover a wedge in the *sink* (`stage: writing`,
hypothesis 3: a USB DAC that stopped draining) or any future blocking
point in the loop. The stall monitor already detects "playing but no
audio for N seconds" from the control plane; today it only logs. Give it
one action: on a sustained stall (comfortably longer than `rw_timeout`,
so the read path is left to fix itself first), tear the session down so
the player falls back into its reconnect loop.

The mechanism needs care — the whole problem is that the player thread is
blocked, so the watchdog cannot ask it politely. Options to weigh in
review, each with a real cost:

- A dedicated **`Command::Restart`** the monitor sends: clean, but a
  blocked loop won't read it until it unblocks — useless for exactly the
  wedge we care about unless paired with (1).
- Making the **source interruptible** from another thread
  (libavformat's `interrupt_callback`, an `AtomicBool` the watchdog
  sets): this directly unblocks `av_read_frame`, which is the real fix
  for a stuck read, and composes with the sink case only if the sink is
  likewise interruptible. Most promising; scope it honestly.
- Accept that with (1) in place the read can no longer wedge, and limit
  the watchdog to the sink case, documenting that a truly stuck
  `writei` is rarer and may still need the existing fatal-error path.

The watchdog must not fight normal reconnection: its threshold sits
above the backoff ceiling's worst case plus `rw_timeout`, so a station
that is simply down keeps retrying quietly (state stays `playing` by
design) and only a genuine *stuck* session triggers a teardown.

### 3. Surface "stalled" (optional, decide in review)

The heartbeat already knows `stalled`. Whether it belongs in `/status`
(and thus the UI) is a real product question: `playing` deliberately
means "trying to play", and a brief stall that self-heals via (1) should
probably not flicker a scary banner. A candidate: expose it only after
it has persisted past the watchdog threshold, as a subtle indicator, not
a state change. This is explicitly out of scope for the first PR and may
be dropped entirely; the `/debug` page already serves the operator.

## Phases

One stack: this plan as the bottom PR, then:

### Phase 1 — `rw_timeout`

The config knob, the option in `FfmpegSource::open`, the
timeout-vs-EOF event distinction, and a bench run of the silent-break
and 15 %-loss scenarios showing the wedge become a bounded reconnect.
The unit-testable part (config parse/convert, the event on a timeout
via a source double) lands with it; the real proof is the runbook
scenarios, captured in the PR.

### Phase 2 — the watchdog

The monitor's teardown action and whichever interruption mechanism
review settles on, with a test driving a blocked source/sink double and
asserting the session is torn down and reconnects. Gated behind the
same stall threshold, tunable.

### Phase 3 — surface "stalled" (only if we decide to)

Small, last, easy to drop.

## Acceptance criteria

- With the silent-break scenario running, audio recovers by itself
  within roughly `rw_timeout` + backoff (no restart needed), and
  `/debug` shows the reconnect signature (climbing `connect_attempts`,
  a timeout event) instead of an unbounded `reading` age.
- `/stop` during that scenario is processed promptly, not deferred until
  a read that may never return.
- Ordinary network blips still ride out on the existing backoff without
  a teardown; a station that is simply down still retries quietly with
  state `playing`.
- The mixer-ceiling invariant is untouched; no new code path between
  decoder and ALSA.
- `cargo test`, `clippy`, `fmt` green on macOS and Linux; the new
  dependency budget is zero (all of this is libavformat options and
  existing threads).
