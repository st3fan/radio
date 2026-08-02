# Plan: absorb the website into radiod, retire PHP and lighttpd

- **Date:** 2026-08-02
- **Status:** draft

## Background

The monolith decision (milestone 11's decision record) left one seam:
the UI is still a separate stack — lighttpd + php-fpm + ~300 lines of
PHP whose entire job is to render one page from daemon status + the
SomaFM channel list, translate form POSTs into daemon API calls, and
offer edit-and-reload iteration. radiod already runs an HTTP server
(hyper). Absorbing the website collapses the install to **one binary,
one deb, one systemd unit**, removes two services and a language
runtime from a 1 GB appliance, and turns the website's
daemon-API-over-localhost hops into function calls.

## Design decisions (the fork Stefan asked about)

- **No embedded scripting engine** (rhai/Lua/JS): what PHP provides
  here is templating plus dev-loop ergonomics, not scripting.
  **`minijinja`** (pure Rust, ~zero transitive deps) covers both:
  templates are embedded in the release binary, and a `--web-dir <path>`
  dev flag loads them from disk instead — edit, reload, no recompile.
  That is the PHP property worth keeping, kept.
- **HTMX for interaction**: buttons become `hx-post` calls returning
  HTML fragments; the now-playing block becomes an `hx-get` poll (5 s,
  same cadence as today). This *deletes* `radio.js` — the custom
  polling/DOM code — and removes the full-page reload on every button.
  `htmx.min.js` is vendored and embedded (no CDN; the PWA stays
  self-contained). Plain no-JS forms keep working: handlers answer
  fragments to HTMX requests and redirects to bare form posts.

## Design

### One server, one origin

- The existing hyper server serves everything: the page (`GET /`),
  HTMX fragment/action routes, the JSON API (unchanged paths from
  `notes/api.md`), and embedded static assets (CSS, icons, manifest,
  htmx). Static assets via `include_bytes!` and a small route table —
  no asset-embedding crate needed.
- **The origin must stay `http://<host>/` (port 80)** — the website is
  an installed iPhone PWA and its identity is bound to the origin; a
  port change breaks every installed icon. radiod binds :80 as the
  `radio` user via `AmbientCapabilities=CAP_NET_BIND_SERVICE` in the
  unit. Config: `listen` defaults to `0.0.0.0:80`; the loopback-only
  validation is removed (see security note). Old loopback configs keep
  working — the operator just gets a loopback-only radio.
- Channel list: the SomaFM `channels.json` fetch moves in-process
  (`ureq` via `spawn_blocking`, small in-memory TTL cache like the PHP
  side has today; artwork stays hotlinked by the browser).
- The daemon-facing PHP (`lib/daemon.php`) disappears entirely: page
  and action handlers read the shared `Status` and send player
  commands in-process.

### Security note (the deliberate change)

The API stops being loopback-only. In capability terms nothing new is
exposed: the PHP site already offered every one of these controls to
the LAN on port 80, proxying them to the daemon — the loopback rule
protected a door that had a public hallway around it. After this
change the LAN can do exactly what the website allows (play/stop/
pause/volume/mute/status), the volume stays inside the never-amplify
clamp, and the hardware mixer ceiling caps output regardless. No
authentication, same as today, appropriate for a home-LAN appliance.
CLAUDE.md's "loopback-only by design" language is rewritten to state
this model.

### Packaging & migration

- `radio-website` is retired: the radiod deb gains
  `Conflicts: radio-website` + `Replaces: radio-website` so upgrading
  removes it. lighttpd/php-fpm stay installed but idle; the README
  documents the optional `apt purge` (the deb does not uninstall other
  packages).
- `deploy/build-website-deb.sh`, the lighttpd/php-fpm deploy files,
  and the release workflow's website steps are deleted; releases ship
  three radiod debs + SHA256SUMS.
- The `website/` directory is deleted; templates and assets move to
  `service/web/` (embedded at build time, `--web-dir service/web` for
  dev).

## Phases

One stack: this plan as the bottom PR, then one branch/PR per phase.

### Phase 1 — the web module

`service/src/web.rs` (+ `service/web/` templates and assets): minijinja
rendering of the page from live status + cached channels, HTMX action
routes sharing the existing player/status plumbing, embedded assets,
`--web-dir` dev override, the `listen`/port-80/capability changes.
The JSON API remains untouched at its paths. Tests: route tests for
page and fragments (driven like the existing server tests — no
network); channels cache behavior with an injected fetcher; verified
in a browser against the dev build. The PHP site keeps working during
this phase (nothing is removed yet).

### Phase 2 — retire PHP/lighttpd

Delete `website/`, the website deb machinery and workflow steps; add
Conflicts/Replaces; rewrite README, CLAUDE.md (components, PHP
section, loopback language, directory layout) and notes/plan.md;
validate on muzak: install the new deb, confirm the PWA still opens
from the existing home-screen icon (same origin), all controls work,
AirPlay badge shows, then purge lighttpd/php and confirm nothing
breaks. Update notes/clean-install.md.

## Test strategy

- Route-level tests on macOS with the null sink (page renders with
  status substituted; actions mutate state and return fragments;
  bare-form posts redirect; assets served with right content types).
- Hardware acceptance on muzak: the installed-PWA origin check is the
  critical one — the icon on Stefan's phone must keep working.

## Acceptance criteria

- One deb installs the whole radio; the PWA at `http://muzak.local/`
  works from the existing home-screen icon; lighttpd and php-fpm can
  be purged with no loss.
- The JSON API is unchanged for scripts, now documented as
  LAN-reachable.
- `radio.js` is gone; buttons update without page reloads; the site
  degrades to plain forms without JS.
- New dependency: `minijinja` only (plus vendored `htmx.min.js` as an
  embedded asset). `cargo test`, `clippy`, `fmt` green on macOS and
  Linux.

## Open questions

- Whether `/status` should keep serving the full JSON to the LAN or a
  trimmed view — lean unchanged (it is the scripting surface).
- HTMX poll cadence and whether to use `hx-trigger="every 5s"` on the
  now-playing block vs SSE later — start with polling, same as today.
