# Plan: Public landing page (website/)

A public-facing landing page for **RADio** (capitalization matters — it's
the wordmark), separate from the LAN-facing control UI that radiod serves
from `service/web/`. This is the page you send people to; it explains the
project and links to the code, docs and downloads. It lives in the repo
root under `website/` — the directory freed up when the old PHP control
site was absorbed into the daemon.

Chosen from three rendered mockups (Phosphor / Faceplate / Poster), the
same way the app's style was chosen in milestone 7: **Phosphor**, reusing
the app's exact CRT terminal look so the landing page and the product are
recognizably the same thing.

## The message

- **Flash a Pi and have internet radio.** That's the pitch, above the fold.
- Works with built-in audio or USB speakers and soundcards.
- Also an OpenAirPlay receiver.
- Honest about maturity: a work-in-progress notice (rough edges, bugs,
  missing features while we work toward a v1.0 release) linking to
  [GitHub Discussions](https://github.com/st3fan/radio/discussions).

## What gets built

A static site — no build step, no framework, no external assets — in:

```
website/
  index.html   the whole page
  style.css    Phosphor, ported from service/web/style.css
  radio.js     the tube cycler (the only JS on the page)
  README.md    what this is, how to preview it
```

Content, top to bottom:

1. **Terminal header** — `RADIO/1.0`, plus a clickable `TUBE:` readout.
2. **Hero** — the `RADio` wordmark with blinking block cursor (the one
   element exempt from the CRT's uppercase-everything rule), the tagline,
   and three buttons: **Download** (GitHub releases), **Documentation**
   (GitHub wiki), **Source code** (the repo).
3. **How it works** — three numbered terminal steps: flash, plug in, tune.
4. **Spec sheet** — a terminal table in the app's channel-list idiom:
   internet radio, audio out (built-in / USB), AirPlay, phone remote
   (PWA), and `SUBSCRIPTION — NO`.
5. **AirPlay callout** — a bordered box: the same box that plays radio is
   an OpenAirPlay target on your network.
6. **Work-in-progress notice** — small dashed-border fine print above the
   footer, with the Discussions link.
7. **Footer** — one line of hardware truth (arm64 Raspberry Pi / ARMv7
   Banana Pi, 512 MB is plenty).

## Style rules (inherited from the app)

- The four tube palettes are copied **verbatim** from
  `service/web/style.css`: green (default), amber, white, blue — keyed on
  `html[data-theme]`, exactly like the app's "t" easter egg. On the
  landing page the tube is cycled by clicking `TUBE:` in the header;
  palette changes fade (~0.9 s) and the fade is disabled under
  `prefers-reduced-motion`, along with the cursor blink.
- Monospace system stack, uppercase transform, scanline sheen, phosphor
  glow — all as in the app. No webfonts, no images.
- If the app's palettes ever change, the landing page copies the new
  values; `service/web/style.css` stays the source of truth.

## Stack

1. `public-landing-plan` — this document.
2. `public-landing-site` — the site itself under `website/`.

## Checks

- Page renders with JS disabled (the tube cycler is the only script and
  is pure progressive enhancement).
- Readable on a phone (the app's ~540 px breakpoints carry over).
- No external requests: view-source shows everything the page loads.

## Out of scope, for later

- **Hosting** — probably GitHub Pages serving `website/`; needs a
  workflow (or Pages' "deploy from branch") and possibly a custom
  domain. Decide when the page content settles.
- Favicon / social preview card (og:image).
- Screenshots of the app on the page (would want the dithered-Phosphor
  treatment from `notes/ideas.md`, not raw PNGs).
- A real "download an SD-card image" story — today the button points at
  GitHub releases (.debs); the prebuilt-image idea lives in
  `notes/ideas.md`.
