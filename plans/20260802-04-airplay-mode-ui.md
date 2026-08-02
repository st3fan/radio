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

## Metadata and artwork: the honest answer

Artist/album/title are **not in the AAC stream** — raw AAC-LC carries
no tags. They (and real cover art, as JPEG/PNG) arrive as DMAP
payloads on the AirPlay control channel via `SET_PARAMETER`, which the
openairplay2 library receives but does not parse or surface. Getting
them is therefore an **upstream feature**: an
`Event::Metadata { artist, album, title }` (and an artwork variant) in
openairplay2, then threading them through radiod's status and this
page. Out of scope here — placeholders hold the slots, and a separate
openairplay2 project will investigate what the controller actually
sends (review decision).

## Phases

One stack: this plan as the bottom PR, one implementation PR on top.

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

## Acceptance criteria

- Picking "Radio" from an Apple device flips the page into receiver
  mode within a poll (~2.5 s); ending the session flips it back —
  verified on muzak with a real sender.
- [STOP] during a session silences the radio and returns the tuner UI;
  the waves animate in every phosphor theme; reduced-motion users get a
  static mark.
- `cargo test`, `clippy`, `fmt` green; no new dependencies; no
  openairplay2 changes.
