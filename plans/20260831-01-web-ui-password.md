# Plan: password-protect the web UI

- **Date:** 2026-08-31
- **Status:** draft

## Background

The HTTP server faces the LAN by design (it carries the website and the
JSON API), and today anyone who can reach `http://<radio>/` can play,
pause, mutter the volume up and down, and — most importantly — turn the
volume up. For a box wired to studio speakers the volume control is the
one thing that matters: a guest phone on the Wi-Fi is one GET away from
the volume bar. Stefan wants a config-file password so that, when it is
set, the web UI asks for it before showing anything (pressed in the same
phosphor terminal style), then remembers the browser with a cookie, and
shows a `[LOCK]` button to drop the cookie again.

The threat model is explicit and modest: this is casual gating of LAN
traffic, not transport security. We are served over plain HTTP, so the
password and the cookie cross the wire in the clear — accepted for this
scope. No auth, no users, no roles: one optional password, one cookie.

## Goal

When `[web] password` is set, every web and JSON route (except the login
flow and the static assets) requires a valid session cookie; the login
page is a phosphor-styled form that sets the cookie on success; the main
UI gains a `[LOCK]` button that clears it. When the password is absent,
the daemon behaves exactly as it does today — zero change, zero overhead.

## Design

### 1. Config: a `[web]` section

New optional `[web]` section with one key, `password`. Absent → feature
off; present → feature on. The key is a string, must be non-empty when
present (a blank password is a footgun, same "reject empty" spirit as
`[airplay] name`). Stored plaintext in the config conffile
`/etc/radio/config.toml` — a secret in a 644 file, unremarkable for a
LAN appliance and consistent with the accepted threat model; documented
in the example with a note that it is PLAINTEXT.

```toml
[web]
password = "hunter2"
```

`RawConfig` gains `web: Option<RawWebConfig>` (with
`deny_unknown_fields`, so a typo like `[web] passsword` fails loudly);
`Config` gains `web: Option<WebConfig>` (mirrors how `mixer`/`stream`
are `Option`s). `WebConfig { password: String }`.

### 2. The session token (stateless, no daemon state)

The cookie value is *not* the password — sending the password itself
round-trips it into every browser cookie jar and every request. Instead
the cookie is a **signed bearer token**: HMAC-SHA256 keyed by the
password over a fixed message, hex-encoded. Verification recomputes the
digest and compares in constant time. There is no session store in the
daemon, so it is restart-proof, any number of browsers can be logged in
at once, and changing the password in the config invalidates every
outstanding cookie for free.

- **Cookie name** `radiod`. Value `hex(hmac_sha256(key = password, msg
  = b"radiod-web-session-v1"))`.
- **Attributes** on login: `Path=/; HttpOnly; SameSite=Lax;
  Max-Age=31536000` (one year — "stays logged in" is literal). No
  `Secure` (we are HTTP), no `Domain` (host-only). `SameSite=Lax`
  shields the state-changing posts from cross-site forgeries; `HttpOnly`
  keeps the token out of JS (we never need it there).
- **Lock/logout** clears it: `radiod=; Path=/; Max-Age=0; HttpOnly;
  SameSite=Lax`.

This needs `hmac` (with `sha2`) as a *direct* dependency. Both are
already in the tree as transitive deps of `openairplay2` (RustCrypto),
pure Rust, essentially zero binary growth — the justification for what
is technically a new direct dependency. (`Mac::verify_slice` does the
constant-time comparison via the in-tree `subtle`.)

### 3. A new `src/auth.rs` module

A small `Auth` type owning `Option<String>` (the password), built from
config and threaded into `App`. It owns all of the above logic and is
unit-testable in isolation:

- `Auth::new(password: Option<String>)` / `enabled(&self) -> bool`
- `token(&self) -> Option<String>` — the expected cookie value
- `login_cookie(&self) -> String` / `logout_cookie(&self) -> String` —
  the two `Set-Cookie` value strings
- `check(cookie_header: Option<&str>) -> bool` — parse the `Cookie`
  header (hand-rolled `name=value` split, no `cookie` crate) and
  constant-time-compare the `radiod` value
- `open_path(path: &str) -> bool` — the allow-list

Open paths (never gated): the login flow and the static assets the login
page itself needs:

```
/login, /logout,
/style.css, /theme.js, /htmx.min.js,
/manifest.json, /icon-180.png, /icon-192.png, /icon-512.png,
/icon-maskable-512.png
```

Everything else is gated.

### 4. Routing: gate in `server::handle`, login in `web::route`

The gate is a short prelude in `server::handle` — after the `Cookie`
header is read and before routing, so both the website and the JSON API
go through one decision:

- if `!auth.enabled()` → straight through (today's path, byte for byte);
- else if `auth.open_path(path)` → straight through (login + assets);
- else if `auth.check(cookie)` → straight through;
- else → denied: HTML/browser requests get `303 /login`, everything else
  gets `401 {"error":"unauthorized"}`.

This is the one decision worth settling in review: **the JSON API is
gated too.** Gating only the HTML but leaving `/play`, `/volume`,
`/stop`, `/mute` open to curl would make the password a polite suggestion
for anyone who ignores the browser. The API is "the same controls the
website offers", so it gets the same lock. (Any external client that
wants in simply logs in first; the token is a normal cookie.)

Routes added to `web::route`:

- `GET /login` — render the login page (if already authed, redirect
  `/`).
- `POST /login` (form field `password`) — correct → `Reply` that sets
  the cookie and redirects `/`; wrong → redirect `/login?error=…`.
- `POST /logout` — clears the cookie and redirects `/login`.

`Reply` gains one variant to carry a cookie on a redirect (the existing
`Redirect(String)` is untouched, so the many existing call sites and
tests stay put): `RedirectCookie { location: String, set_cookie: String }`.

Auth state reaches `handle` via a new `App.auth: auth::Auth` field,
built in `main.rs` from `config.web`. (The `Web` struct stays
focused on rendering; the gate belongs to the server.)

### 5. The templates

**`web/login.html`** (new) — a small phosphor page in the same language
as the rest of the site: the `RADIO <version>` / `RADIO DEV` banner, a
`> IDENTIFY` prompt, a `> PASSWORD` line, a masked input, and a `[ENTER]`
submit. Reuses `style.css`, `theme.js` (so `t` still cycles the tube on
the login screen), the `.term-head` / `.prompt` / `.error` classes and
the `?error=` state. Plain POST-redirect-GET, no HTMX, no poller.

**`web/index.html`** — a `[LOCK]` button appears in the header next to
the mode subtitle, only when the password is set (new `password_set`
flag on `PageContext`). It is a plain `POST /logout` form (no HTMX), so
the whole page navigates to `/login` when it is pressed rather than
leaving the poller running behind the lock.

**`web/style.css`** — a rule for `input[type="password"]` matching the
existing `number` input treatment (mono, phosphor, dim border, bright
focus ring).

The login page is intentionally *not* the app page: it has no
`#app`/`hx-select` target, so if the cookie ever expires under an open
page and the poller receives the `303 /login`, the worst that happens is
an empty swap — a reload shows the login form. Acceptable for this
scope.

### 6. Docs

- `deploy/config.toml.example` gains a commented `[web]` section
  (defaulting to absent), with the PLAINTEXT caveat.
- `notes/api.md` gains a short "Authentication" note: when `[web]
  password` is set, all API endpoints return `401` unless the request
  carries the `radiod` cookie; `POST /login` accepts a form-encoded
  `password` and returns the cookie.

## Phases

One stack: this plan as the bottom PR, then a single implementation PR.
There is no natural seam worth splitting — config + auth + gate + login
page + lock button are one logical change and one review.

### Phase 1 — `[web] password` end to end

Config key + validation, `src/auth.rs` (token, cookie strings, header
parse, open-path list) with unit tests, the `handle` gate, the two
`Reply` plumbing changes, the login page + `[LOCK]` button + CSS, the
doc updates, and the whole matrix of tests:

- **auth.rs unit tests** — token verifies; a wrong value, an absent
  cookie, and a logout cookie all fail `check`; login vs logout
  `Set-Cookie` strings; the open-path allow-list.
- **server routing tests** — with the password set: unauth web GET →
  `303 /login`; unauth JSON `GET /status` → `401`; `POST /login`
  wrong → `303 /login?error=…` (no cookie); `POST /login` right →
  redirect that sets the cookie; authed (cookie presented) GET `/` →
  full page; `/logout` clears. With the password unset, every existing
  test demonstrates the unchanged path (the whole suite doubles as the
  "off = no change" regression).
- `cargo test`, `clippy`, `fmt` green on macOS; build on the Debian PC.

Out of scope (deliberately): transport security (HTTPS), a real
account/multi-user model, rate-limiting the password attempts, hashing
the password in the config file, and any `/status` "auth required" field.

## Acceptance criteria

- With no `[web]` section the daemon is byte-for-byte today's behavior;
  no cookie is set or required anywhere.
- With `[web] password` set, navigating to `http://<radio>/` shows the
  phosphor login form; the right password sets the `radiod` cookie and
  lands on the tuner; the wrong password shows `! ERROR` and no cookie.
- The browser stays logged in across reloads and restarts of the
  browser (the cookie is persistent, one year); changing the password in
  the config invalidates old cookies.
- The `[LOCK]` button in the header clears the cookie and returns the
  browser to the login form; it appears only when the password is set.
- The JSON API returns `401` when unauthenticated and works normally
  once the cookie is presented; the static assets stay reachable without
  auth (the login page needs them).
- The mixer-ceiling invariant is untouched — this is purely HTTP
  control-plane work, no code path near the audio.
- New direct dependency is justified and effectively free: `hmac` +
  `sha2`, already compiled as transitive deps of `openairplay2`.
- `cargo test`, `clippy`, `fmt` green on macOS and the Debian PC.
