# Plan: Metadata & pause/resume (milestone 4)

Three features that finish the daemon's core behavior:

1. **Pause/resume** — `POST /pause` disconnects but remembers the station;
   `POST /resume` reconnects to it.
2. **Reconnect with backoff** — a dropped stream no longer silently stops
   playback; the player reconnects while `state` stays `playing`.
3. **ICY metadata** — `icy_title` (the current song) and `icy_name` (the
   station) stop being `null` in `/status`.

All three touch the player, so this runs as **three phases on an
integration branch** `integration/milestone-4`, ordered so each builds on
the previous: the pause/resume state machine first, backoff on top of it,
metadata last (mostly independent, in the pipeline).

## Phase 1 — pause/resume (player state machine)

Refactor the player loop from "idle or playing" into an explicit state
machine — this is the foundation the other phases build on:

```rust
enum Session { Idle, Playing(Station), Paused(Station) }
struct Station { playlist_url: String, stream_url: String }
```

- `Command::Pause`: tear down the source and close the sink (no buffering —
  512 MB RAM, live radio has no seek position, and icecast drops stalled
  clients anyway), set `state: "paused"`, **keep** `playlist_url` and
  `stream_url` in the status.
- `Command::Resume`: rebuild the source from the remembered `stream_url`
  and continue. If the reopen fails, fall through to the normal
  error path (phase 2 turns that into retries).
- `/play` while paused switches to the new station (or resumes if it is the
  same one). `/stop` from paused clears everything.

API semantics:

| Request | while playing | while paused | while stopped |
|---------|--------------|--------------|---------------|
| `POST /pause`  | → paused, 200 | no-op, 200 | **409** `{"error": "nothing is playing"}` |
| `POST /resume` | no-op, 200 | → playing, 200 | **409** |

Tests: state transitions including the 409s, urls retained while paused,
sink closed on pause (capturing sink) and reopened on resume, source
factory invoked again on resume, play-while-paused switches station.

## Phase 2 — reconnect with backoff

Today a source error or EOF drops to `stopped`. For a radio that should
"just keep playing" on flaky WiFi:

- While `state` is `playing`, source EOF and source errors trigger a
  **reopen loop**: wait, rebuild the source from the remembered
  `stream_url`, continue. Backoff doubles from 500 ms to a 30 s max, and
  resets after ~30 s of stable playback. Retries continue indefinitely —
  the user's remedy is `/stop`.
- The wait must stay responsive: use `recv_timeout` on the command channel
  so `/stop`, `/pause`, and `/play` interrupt a backoff sleep immediately.
- Sink errors that survive ALSA's `try_recover` remain fatal (they mean
  the device is gone, not the network) → `stopped`, logged.
- Backoff timings are injectable (a small `PlayerTuning` with `Default`)
  so tests run in milliseconds.
- Status is unchanged during retries (`playing`); attempts are logged to
  stderr. libavformat's own `reconnect` options stay on as the first line
  of defense.

Tests: factory that fails N times then succeeds (player recovers, state
never leaves `playing`, factory called N+1 times), EOF triggers reconnect,
`/stop` during backoff interrupts immediately, backoff escalates
(observable via injected tuning + factory call timestamps).

## Phase 3 — ICY metadata

- Open the input with `icy=1` set explicitly (it is the http default, but
  being explicit documents the dependency).
- **Station name**: after open, read the `icy_metadata_headers` option off
  the format context (`av_opt_get` with `AV_OPT_SEARCH_CHILDREN` — the
  same mechanism mpv uses) and parse the `icy-name:` header.
- **Song title**: poll the `icy_metadata_packet` option after reads and
  parse `StreamTitle='…';`. ffmpeg-next has no safe wrapper for
  `av_opt_get`, so this is one small contained `unsafe` block in
  `pipeline.rs` using the re-exported `ffmpeg_next::sys` — no new
  dependency.
- Surface through the `Source` trait: a new `fn icy(&mut self) ->
  Option<IcyMetadata>` returning `{ name, title }` when something
  *changed*; the player polls it once per chunk and writes changes to the
  shared status. `SineSource` returns `None`.
- Parsing (`StreamTitle` extraction, `icy-name` header extraction) lives in
  pure functions with unit tests — including titles containing quotes and
  semicolons, empty titles, and missing fields.
- Lifecycle: `icy_title`/`icy_name` are cleared on stop and on station
  switch, retained while paused (the station has not changed).

Tests: the parsers; a test `Source` that emits metadata changes → player
propagates them to status; clearing on stop/switch; retained on pause.

## Acceptance (milestone level)

- All tests green on macOS and Debian (Docker); clippy/fmt clean.
- Live on the Debian PC:
  - `/status` while playing SomaFM shows `icy_name` ("DEF CON Radio") and
    an `icy_title` that changes when the song changes.
  - `/pause` stops the audio; `/status` says `paused` and keeps the URLs;
    `/resume` brings the music back within a couple of seconds.
  - Pull the network cable (or block the stream) mid-play for ~15 s and
    restore it: audio resumes by itself, `state` stayed `playing`
    throughout.
