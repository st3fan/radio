# Plan: website polish round

- **Date:** 2026-08-02
- **Status:** draft
- **Shape:** one stack, **one PR per change** (cheap, individually
  reviewable and revertable), with two groupings where changes
  physically collide: the three link changes are one PR, and the
  artwork + fixed-height changes are one layout redesign.

## The changes (Stefan's list)

1. Header: RADIO links to `github.com/st3fan/radio`.
2. Footer: `RADIO` becomes `github.com/st3fan/radio` (linked); the
   SomaFM credit properly links to `somafm.com`.
3. Remove the volume textbox row (`VOL% [__] [SET]`); VOL-/VOL+ stay.
4. Remove the PAUSE, STOP and MUTE buttons — they mostly do the same
   thing. UI only: the JSON API keeps `/pause`, `/resume`, `/stop`,
   `/mute`, `/unmute` unchanged for scripts and the AirPlay gating.
5. Fixed-height top section: no more layout jumps when playback starts.
6. Channel artwork next to the now-playing data, phosphor-tinted; no
   artwork in the channel list.
7. Sortable channel list: STATION, GENRE, LSNRS. Default order
   unchanged, no indicator until a heading is clicked; then a `^`/`v`
   indicator, click again to flip direction.
8. Poll interval 5 s → 2.5 s.

## Design notes

- **Sorting is server-side**, carried in the query string
  (`/?sort=station&dir=asc`): headings are plain links (no-JS works)
  with HTMX swap + `hx-push-url`; the rendered page bakes the current
  sort into its own poll URL, so the 2.5 s refresh self-perpetuates the
  chosen order across swaps. Sort keys: station (title, A→Z first),
  genre (A→Z first), listeners (high→low first, the default). Missing
  or invalid params mean the default view.
- **Artwork**: SomaFM's `channels.json` carries per-channel image URLs;
  the current channel's image renders beside the now-playing text,
  hotlinked (as the channel data policy already is) and tinted to the
  terminal theme with CSS filters (grayscale → sepia → hue-rotate into
  phosphor green; the exact hue tuned against the stylesheet's green,
  amber as the fallback candidate). When nothing plays (or AirPlay is
  active — no SomaFM artwork), the image slot renders as an empty
  dimmed frame of the same size.
- **Fixed height** falls out of the artwork slot: the now-playing
  section becomes a flex row (fixed-size art box + text column) with a
  `min-height` sized to its fullest state, so STANDBY, NOW PLAYING and
  AIRPLAY all occupy identical space.
- With PAUSE/STOP/MUTE gone the paused state is unreachable from the
  UI (still reachable via API; the page still renders it correctly if
  it happens).

## Stack layout

1. plan (this document)
2. `web-links` — changes 1 + 2
3. `web-no-volbox` — change 3
4. `web-no-transport` — change 4
5. `web-poll-interval` — change 8
6. `web-sortable-channels` — change 7
7. `web-now-playing-art` — changes 5 + 6

Each PR: template/CSS/web.rs edits + adjusted route tests where
behavior changes (sorting gets real tests; removals get assertion
updates). Verified on the dev server throughout; final validation on
muzak once the stack is reviewed.

## Acceptance criteria

- All eight changes visible and behaving on muzak (and the phone PWA).
- Sorting survives the poll cycle; the no-JS fallback sorts too.
- No layout shift between STANDBY → NOW PLAYING → AIRPLAY.
- `cargo test`, `clippy`, `fmt` green; no new dependencies.
