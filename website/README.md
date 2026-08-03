# RADio landing page

The public-facing website for RADio — the page you send people to. Not to
be confused with the LAN-facing control UI, which radiod serves itself
from `service/web/`.

Static files, no build step, no framework, no external assets. The look
is Phosphor, the app's own CRT terminal style; the tube palettes in
`style.css` are copied verbatim from `service/web/style.css`, which stays
the source of truth. Clicking `TUBE:` in the header cycles green → amber
→ white → blue, like the app's "t" easter egg.

## Previewing

Open `index.html` in a browser, or:

```
python3 -m http.server -d website
```

## Layout

```
index.html   the whole page
style.css    Phosphor, ported from the app
radio.js     the tube cycler (progressive enhancement — the only JS)
```

Hosting is decided in `plans/20260803-01-public-landing-page.md` under
"Out of scope, for later" (likely GitHub Pages).
