# The radiod HTTP API

The daemon's HTTP server (`listen` in the config, port 80 on the radio)
carries both the built-in website and this JSON API — the API is
LAN-reachable by design and exposes the same controls the website does.
Every API response has `Content-Type: application/json`. Request bodies
are JSON; no `Content-Type` header is required on requests, and bodies
are capped at 64 KB. (Dev builds default to port 8080; the examples
below use that.)

## Conventions

- **Success**: HTTP 200 with the full [status object](#the-status-object)
  as the body — every successful call returns the same shape, so a client
  never needs a follow-up `/status` to see the result.
- **Errors**: `{"error": "<message>"}` with a 4xx/5xx status.
- **Asynchrony**: playback changes happen on the player thread. A 200 from
  `/play`, `/stop`, `/pause`, or `/resume` means the command was accepted;
  the `state` in that same response may still show the previous state for
  a moment. Poll `GET /status` for the settled result (the website does
  this by reloading the page).

## Endpoints

### `GET /status`

Returns the status object. Never fails.

### `POST /play`

Body: `{"playlist_url": "https://somafm.com/defcon.pls"}`

Fetches the `.pls` playlist, resolves the first stream URL in it, and
starts playing. Special cases:

- The requested playlist is **already playing** → no-op 200; the stream is
  not interrupted and the playlist is not re-fetched.
- The requested playlist is the **currently paused** one → resumes it
  (again without re-fetching).
- Anything else — including a different playlist while playing or paused —
  switches to the new station.

Errors: 400 `invalid request body: …` (malformed JSON / missing field),
409 `airplay session active` (an AirPlay sender owns the pipeline; stop
is the local override), 502 `cannot fetch playlist: …` or `cannot parse
playlist: …` (the fetch happens synchronously in the handler, so a bad
playlist URL fails the request instead of failing silently later).

### `POST /stop`

Stops playback and clears the station (`playlist_url`, `stream_url`, and
the ICY fields become `null`). Always 200, including when already
stopped. During an AirPlay session this is the local override: playback
stops here (the remembered station is forgotten), while the sender keeps
its session until it notices or the user stops it there.

### `POST /pause`

Disconnects from the stream but **remembers the station** — the URLs and
ICY fields stay in the status. No audio is buffered while paused (live
radio has no meaningful seek position; resume plays "now", not where you
left off).

- While playing → 200, state becomes `paused`.
- While already paused → no-op 200.
- While stopped → **409** `nothing is playing`.
- During an AirPlay session → **409** `airplay session active` (the
  sender owns transport control).

### `POST /resume`

Reconnects to the remembered stream.

- While paused → 200, state becomes `playing`.
- While already playing → no-op 200.
- While stopped → **409** `nothing is playing`.
- During an AirPlay session → **409** `airplay session active`.

### `POST /volume`

Body: `{"volume": 30}`

The value is the volume, 0–100. Loudness protection does not live here:
the daemon owns the ALSA mixer and pins the hardware output to the
configured ceiling, so 100 means "as loud as the ceiling allows".

Takes effect within one audio chunk (~25 ms), whether or not something is
playing.

Errors: 400 `volume must be between 0 and 100` (for 101–255), 400
`invalid request body: …` (non-integers, negatives, malformed JSON).

### `POST /mute` / `POST /unmute`

Sets gain to zero / restores it. The `volume` value is untouched by
muting, so unmute returns to the pre-mute level. Always 200. Muting is
distinct from `volume 0`: `muted` is a separate flag in the status.

### Anything else

404 `{"error": "not found"}` — including wrong methods on valid paths
(e.g. `GET /play`).

## The status object

```json
{
  "state": "playing",
  "playlist_url": "https://somafm.com/defcon.pls",
  "stream_url": "https://ice2.somafm.com/defcon-128-mp3",
  "icy_title": "Nightmares on Wax - Les Nuits",
  "icy_name": "DEF CON Radio: SomaFM's year-round channel for DEF CON [SomaFM]",
  "volume": 25,
  "muted": false,
  "mixer": "ok",
  "source": "radio",
  "airplay": null
}
```

| Field          | Type            | Meaning |
|----------------|-----------------|---------|
| `state`        | `"playing"` \| `"paused"` \| `"stopped"` | `playing` means "trying to play": the state does **not** drop on network trouble — the daemon reconnects with backoff (0.5 s doubling to 30 s) until `/stop`. |
| `playlist_url` | string \| null  | The `.pls` URL that was played. Set while playing/paused, `null` when stopped. |
| `stream_url`   | string \| null  | The resolved icecast stream URL. Same lifecycle. |
| `icy_title`    | string \| null  | The current song (ICY `StreamTitle`), updated live as songs change. `null` when the stream sends none, when stopped, and briefly after a station switch. Survives pause. |
| `icy_name`     | string \| null  | The station name (ICY `icy-name` header). Same lifecycle as `icy_title`. |
| `volume`       | integer         | The volume, 0–100. |
| `muted`        | boolean         | Gain is forced to 0 when true; `volume` keeps its value. |
| `mixer`        | string          | Health of the daemon-owned hardware ceiling: `"ok"`, `"disabled"` (dev sinks without a mixer), or `"error: ..."` — playback is refused while the ceiling cannot be asserted. |
| `source`       | `"radio"` \| `"airplay"` | Which producer owns the pipeline. While `"airplay"`, the station fields are `null` and `/play`, `/pause`, `/resume` answer 409. |
| `airplay`      | object \| null | `{"rate": 44100, "channels": 2}` while an AirPlay stream is active. |

## Examples

```sh
curl http://127.0.0.1:8080/status
curl -X POST http://127.0.0.1:8080/play -d '{"playlist_url": "https://somafm.com/defcon.pls"}'
curl -X POST http://127.0.0.1:8080/volume -d '{"volume": 60}'
curl -X POST http://127.0.0.1:8080/mute
curl -X POST http://127.0.0.1:8080/unmute
curl -X POST http://127.0.0.1:8080/pause
curl -X POST http://127.0.0.1:8080/resume
curl -X POST http://127.0.0.1:8080/stop
```
