# Plan: a real AirPlay mode for the web UI

- **Date:** 2026-08-02
- **Status:** draft

## Background

During an AirPlay session the website today is the SomaFM tuner with a
badge on it: AIRPLAY prompt, disabled channel rows. Stefan wants a
proper mode switch — when the radio is an active AirPlay receiver, the
page should *be* an AirPlay receiver page, not a disabled tuner.

## The mode (per Stefan's spec)

While `source == "airplay"`:

- Banner subtitle: **OPENAIRPLAY RECEIVER** (instead of SOMAFM TUNER).
- Prompt: **> STREAMING OPENAIRPLAY**.
- The SomaFM-specific UI disappears entirely — no channel table, no
  disabled [AIRPLAY] rows, no "sender controls playback" note (the mode
  itself says it).
- **No volume controls** (review decision): AirPlay volume belongs to
  the sender; the whole volume line is hidden in this mode. Simplify
  first, iterate later.
- **A [STOP] button** (review decision): kills the AirPlay session
  locally and returns to the SomaFM tuner (idle). Honest limitation:
  this stops *our* playback — the protocol session stays up and the
  sender keeps streaming into the void until it notices or the user
  stops it there; a true sender-kick is a future openairplay2 feature.
- The title line shows a dim placeholder (`— NO TRACK INFO —`) where
  track metadata will land (see below); the station line shows what we
  *do* know: the negotiated stream, e.g. `44100 HZ · 2 CH · AAC`, from
  the `airplay {rate, channels}` status object.
- The art box shows an **animated "air waves" mark** instead of the
  lone DJ: concentric phosphor arcs radiating, drawn as inline SVG and
  animated with CSS. (WebGL was considered and skipped: an SVG+CSS
  animation is self-contained, inherits the current phosphor theme via
  `currentColor`, respects `prefers-reduced-motion`, and costs no GPU
  path. If a fancier animation is ever wanted, the slot is one div.)

## Metadata and artwork

Artist/album/title are **not in the AAC stream** — raw AAC-LC carries
no tags. They (and real cover art, as JPEG/PNG) arrive as DMAP
payloads on the AirPlay control channel via `SET_PARAMETER`, which the
openairplay2 library originally received but did not surface. Phase 1
therefore shipped placeholders, and the feature was requested upstream
(openairplay2 milestone 8). **Upstream delivered mid-task**: two new
variants on the `#[non_exhaustive]` `Event` enum, merged to
openairplay2 main and hardware-validated against an iPhone —
`Event::Metadata { title, artist, album }` (a complete statement per
event, not a delta: absent fields replace previous values) and
`Event::Artwork { content_type, data }` (raw bytes as sent; empty data
/ `image/none` means the sender cleared the art). Both arrive only
between `SessionStarted` and `SessionEnded`; clearing display state at
session end is the consumer's job; duplicates are common and must be
absorbed idempotently. Phase 2 consumes them.

## Phases

One stack: this plan as the bottom PR, implementation PRs on top.

### Phase 1 — the mode

Template branches on `airplay_active` (already in the render context):
subtitle, prompt, hidden channels section and volume line, the [STOP]
button (the existing stop action), stream-info station line
(rate/channels formatted server-side), the metadata placeholder, the
SVG wave mark + CSS animation. The channels fetch is skipped for
AirPlay renders (nothing uses it). Tests: the existing airplay render
test becomes the mode test — asserts the subtitle, prompt, placeholder,
stream line, wave mark and [STOP]; channel table, volume bar and
SomaFM strings absent; radio-mode renders unchanged.

### Phase 2 — track metadata and artwork (upstream delivered)

Bump the `openairplay2` git dependency (`cargo update -p openairplay2`)
and consume the two new events:

- **Status:** session-state fields on `Status` (`#[serde(skip)]`, like
  `airplay_gain` — the JSON API contract is unchanged): the current
  track's title/artist/album, and the latest artwork as content type +
  `Arc`'d bytes + a monotonic version for cache busting. Cleared where
  the other AirPlay session state is cleared.
- **Event task** (`main.rs`): `Metadata` replaces the track wholesale
  (equality-checked, so duplicate events don't churn the shared
  status); `Artwork` stores the bytes, empty data clears.
- **Artwork endpoint:** `GET /airplay/artwork` serves the stored bytes
  with the sender's content type while a session is active, 404
  otherwise. The page references it as `/airplay/artwork?v=N` so a
  track change is a new URL; responses carry a short `max-age`, making
  repeated polls cheap without risking stale art across restarts.
- **Page:** in AirPlay mode the title line shows the track title
  (placeholder only when the sender sent none); the station line
  prefers `ARTIST — ALBUM` over the negotiated-stream line; and when
  artwork exists the art box shows it — same markup as the SomaFM
  artwork, so the phosphor treatment applies — with the airwaves
  animation as the no-artwork fallback.
- Tests: a session with metadata + artwork renders title, artist —
  album and the versioned artwork URL; the endpoint serves the bytes
  and 404s outside a session; a metadata-less session still renders
  the phase-1 placeholders and waves.

## Acceptance criteria

- Picking "Radio" from an Apple device flips the page into receiver
  mode within a poll (~2.5 s); ending the session flips it back —
  verified on muzak with a real sender.
- [STOP] during a session silences the radio and returns the tuner UI;
  the waves animate in every phosphor theme; reduced-motion users get a
  static mark.
- With a sender that pushes metadata (iPhone Music.app), the page
  shows real title/artist/album and the cover art in the art box
  within a poll of a track change; a clear (`image/none`) brings the
  waves back.
- `cargo test`, `clippy`, `fmt` green; no new dependencies beyond the
  openairplay2 version bump.
