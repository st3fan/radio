# Ideas

A parking lot for things we might build after the roadmap in `notes/plan.md`.
Nothing here is committed to. When one of these graduates it becomes a
`plans/YYYYMMDD-NN-slug.md` and a roadmap entry.

Ordering is rough: the first few feel closest to "obviously worth doing".

---

## Favorite stations

**Why:** Seven clicks of scrolling to get back to Drone Zone every morning is
silly. Also the natural home for a future physical button / rotary encoder:
"favorite 1..4".

**Sketch:** Keep it in the website, not the daemon — `radiod` stays a dumb
player that takes a playlist URL, which is what makes it testable. A small
JSON file (`/var/lib/radio-website/favorites.json`, owned by the php-fpm
user, created by the package postinst) plus a star/pin toggle on each channel
row and a "Favorites" section pinned at the top of the page.

**Open questions:**

- Right now favorites can just be SomaFM channel ids, and the server keeps
  resolving ids → `.pls` from its cached `channels.json`. The moment we add
  other directories (below), a favorite needs a fuller record: source,
  station id, display name, playlist URL, artwork URL. Worth designing that
  record *now* even if v1 only ever stores SomaFM ids, so we don't have to
  migrate the file later.
- Ordering: manual drag is a lot of JS for this UI. Probably "order added",
  with plain up/down buttons if it ever annoys us.
- Do favorites survive a package reinstall? They must — so the file lives in
  `/var/lib`, not under the docroot, and the package must not own it.

**Effort:** small. Best next feature/effort ratio on this list.

---

## More streams beyond SomaFM

**Why:** SomaFM is great but it is one curator's taste. Sometimes you want a
news station, a local station, or something SomaFM doesn't do.

Two separable pieces, and we should not conflate them:

### 1. Paste-your-own-URL

The daemon already plays any `.pls` it can fetch. Add a text field on the
website: paste a stream or playlist URL, play it, optionally favorite it.
This alone covers most "but I want station X" cases and costs almost nothing.

Watch out: the website currently never lets the browser hand a URL to the
daemon (that's a deliberate property — see `notes/plan.md`, Component 2).
Accepting arbitrary URLs weakens it. Mitigations: require `http(s)` scheme,
resolve and reject non-public IPs before handing anything to the daemon
(the daemon listens on loopback and would happily fetch a LAN address
otherwise), and keep it a separate, explicit page rather than a hidden field
on the main list.

### 2. A real stream directory

**Radio Browser** (`api.radio-browser.info`) is the obvious candidate: free,
no API key, community-maintained, ~50k stations, JSON API with search by
name/country/tag/codec, and station click/vote endpoints. Discovery is via
DNS round-robin over mirror hosts, and it asks clients to send a
`User-Agent` identifying themselves. Downsides: quality is uneven, dead
streams are common, and results need filtering (bitrate, codec, last-check-ok
flag).

Alternatives worth a look: the Icecast directory (`dir.xiph.org`), and
station lists we curate by hand in a TOML file if we decide a full directory
is more than we want.

**Sketch:** generalise `website/lib/somafm.php` into a small "directory"
interface — `list()`, `search()`, `resolve(id) → playlist/stream URL` — with
SomaFM and Radio Browser as two implementations, each with its own on-disk
cache. The UI grows a source switcher; SomaFM stays the default landing view
because that's what we actually listen to.

**Open questions:**

- Search UI on a page that is currently forms-and-full-reloads. Probably
  fine: a search box, a POST, a results page.
- Caching a directory of 50k stations on a Pi Zero is not sensible — cache
  *queries*, short TTL, small result sets.
- Radio Browser has plenty of streams that are 320 kbps or AAC+ oddities;
  worth a codec/bitrate filter so we don't ask an ARM1176 to do something
  silly. (We should measure: what *is* the ceiling on the Pi Zero? 128 kbps
  MP3 costs ~7% CPU, so there is a lot of headroom, but let's know the number
  before promising it.)

**Effort:** medium. The directory abstraction is the real work; Radio Browser
itself is a couple of `ureq`/`file_get_contents` calls.

---

## AirPlay support

**Why:** the speakers are good, and "play whatever is on my phone" is the
obvious thing to want from a box that already owns them. It turns the Pi from
a radio into a speaker endpoint.

**Sketch:** don't write this ourselves. `shairport-sync` is the mature
implementation and packaged in Debian. AirPlay 1 is cheap; AirPlay 2 needs
`nqptp` and more CPU/RAM — on a Pi Zero W 1.0 that needs measuring before we
promise it, and 2.4 GHz-only WiFi is a real constraint for a streaming
protocol with tight timing.

**The two problems that actually matter:**

1. **The volume cap.** This is the big one. `shairport-sync` writes to ALSA
   directly — its samples never pass through `volume::effective_volume()`, so
   our critical invariant (`CLAUDE.md`) simply does not hold for AirPlay
   audio. A phone at full volume would drive the studio speakers at full
   scale. Options, none free:
   - Configure `shairport-sync` with a software volume attenuation and its
     own max (`volume_max_db`) — but that's a *second* place the invariant
     lives, enforced by config rather than by a unit-tested function, which
     is exactly what the invariant was written to avoid.
   - Have `shairport-sync` output to a pipe/loopback (`snd-aloop`) and let
     `radiod` be the only writer to the real device, applying our gain. This
     preserves the invariant properly — one code path to the hardware — at
     the cost of a more complex audio graph and added latency.
   - The second option is almost certainly the right one, and it's a
     meaningful chunk of design work in `radiod` (a second `Source`).
2. **Device arbitration.** Two processes want the ALSA device. Who wins when
   AirPlay starts while the radio is playing? Nicest behaviour: AirPlay
   takes over, radio auto-resumes when the AirPlay session ends — which is
   what the loopback design gives us naturally, since `radiod` sees both
   sources and can implement the policy.

**Open questions:** does AirPlay 2 even fit on this hardware? Does a Pi
Zero W's WiFi hold up? Is the added latency of a loopback acceptable (for
music, yes; nobody is playing games through this).

**Effort:** large, and it changes the audio architecture. Prototype
`shairport-sync` standalone on the Debian PC first and just *listen* before
designing anything.

---

## Make the player show up in Home Assistant

**Why:** automations. "Kitchen radio on at 07:30", "stop everything when we
leave", a card in the dashboard, voice control for free.

**What it would take — four routes, cheapest first:**

1. **REST commands + a template media player.** No code on our side at all:
   HA `rest_command` entries for play/stop/volume and a template sensor
   polling `/status`. But `radiod` binds loopback only, so HA (on another
   host) can't reach it — we'd need to expose the API on the LAN, which
   deliberately isn't the case today. Ugly, and gives a second-class entity
   with no proper media-player card.
2. **Speak MPD.** HA ships a first-party MPD integration. If `radiod` spoke a
   small subset of the MPD protocol (`status`, `play`, `stop`, `pause`,
   `setvol`, `currentsong`, plus enough of the command list to satisfy the
   client), we'd get Home Assistant *and* the whole ecosystem of MPD remotes
   and phone apps for one protocol implementation. Attractive: it's a plain
   line-based TCP protocol, no dependencies, fits the "small and blocking"
   house style. Risk: HA's client may poke at more of the protocol than we
   want to implement (playlists, database, `idle`), so this needs a spike
   against the actual integration before committing.
3. **DLNA / UPnP MediaRenderer.** Also a first-party HA integration, also
   gets us other clients — but SSDP + SOAP + XML is a lot more surface than
   MPD's text protocol, and pulling in a UPnP stack fights the
   few-dependencies constraint.
4. **A custom HA integration.** A small Python component talking to our REST
   API, distributed via HACS. Cleanest entity, full control, mDNS discovery
   possible — but it's a second codebase in a second language, and it still
   needs the API reachable off-loopback.

**Leaning:** (2), with (4) as the fallback if the MPD spike turns ugly. Either
way it forces a decision we've so far avoided: **letting something other than
the local website talk to the daemon.** That means an auth story (token? LAN
only? bind to a specific interface?) and re-reading the threat model in
`notes/plan.md`. Worth writing that down before writing any code.

**Effort:** medium for the protocol, small for the HA side, and a real
security-design conversation attached to both.

---

## A prebuilt image for a CF/SD card

**Why:** today, standing up a new box is: flash Raspberry Pi OS, install
packages, edit config, find the ALSA device. A "write this image, boot it,
open `http://radio.local`" story is what makes this shareable at all.

**Sketch:** `pi-gen` (the tool Raspberry Pi OS itself is built with) with a
custom stage that installs our two `.deb`s, or the newer `rpi-image-gen`.
Build it in CI or in the same Docker container we already cross-build in.

**What the image has to solve that the packages don't:**

- **WiFi and first boot.** Pi Zero W is 2.4 GHz only and headless. Either
  lean on Raspberry Pi Imager's customisation (it writes WiFi credentials and
  hostname into the boot partition — the least work by far), or ship a
  captive-portal-style setup AP, which is a project in itself.
- **Hostname/mDNS:** `radio.local` via avahi, so there's a URL to hand
  someone.
- **Audio device autodetection.** The packaged default is `plughw:0,0`, which
  is right for the office Pi's USB speakers and wrong in general. An image
  needs to pick a sane device at first boot, or a first-run page in the
  website that lists `aplay -l` output and writes the config.
- **First-boot volume.** An unknown user's speakers with our default cap
  of 50 — is that safe? Probably start lower for an image build.
- **Size, and rebuilds.** An image is a release artifact that goes stale;
  either it auto-updates (`unattended-upgrades` plus our own apt repo) or we
  commit to rebuilding it.
- **Licensing.** We link FFmpeg (LGPL, and GPL if anyone's build enables it)
  and redistributing an image means shipping those binaries. The
  still-unanswered license question in `notes/plan.md` has to be answered
  before an image is published anywhere.

**Effort:** large, and it's mostly a release-engineering project rather than a
coding one. Also strictly downstream of "our own apt repo", which we don't
have yet.

---

## A website and documentation — moving beyond a personal hack

**Why:** it's a genuinely nice little appliance, and right now the only way
to know it exists is to read this repo.

**Sketch, in dependency order** (each step is useful on its own — this is not
all-or-nothing):

1. **README that assumes nothing.** What it is, a photo of the actual box, a
   screenshot of the Phosphor UI, hardware list, install in five commands.
   Cheap, and most of the value.
2. **A LICENSE file.** Blocking for everything below it, and already flagged
   as open in `notes/plan.md`. Nobody can use this until it exists.
3. **Docs proper.** Install, configure, the REST API (we already have
   `notes/api.md` — it's most of a reference page), building for the Pi,
   troubleshooting. Static site from Markdown, GitHub Pages.
4. **An apt repository** so `apt upgrade` works instead of scp-ing `.deb`s.
   Prerequisite for the prebuilt image being maintainable.
5. **CI**: `cargo test`/`clippy`/`fmt` on every PR, and the ARMv6 cross-build
   plus both `.deb`s as release artifacts. This is the one that changes how it
   feels to work on the project, and it's arguably worth doing before any of
   the public-facing items.

**Open questions:** how much support does publishing invite? Issues from
people on hardware we don't have, using audio devices we can't test. Worth
being explicit in the README about what's tested (Pi Zero W 1.0, Raspbian
trixie, one USB speaker) and what's best-effort.

**Effort:** each item small-to-medium; the sequence is the point. (5) and (2)
are the two that unblock other things.

---

## Smaller things, unsorted

- **Sleep timer.** "Stop in 45 minutes." Trivial in the daemon, and the kind
  of thing you want at 23:00.
- **Alarm / wake to radio.** Falls out of Home Assistant integration for free,
  or a couple of lines of cron if we don't want to wait for that.
- **Physical controls.** A rotary encoder for volume and 3–4 buttons for
  favorites, on the GPIO header. This is the real endgame for a box that
  drives studio speakers — you shouldn't need a phone to turn it down. Needs
  a GPIO story in the daemon (`gpiod`?) and pushes back on the
  few-dependencies rule.
- **A little display.** Small OLED/e-ink showing station and ICY title.
  Cheap, charming, and would use the metadata we already parse.
- **Recording / "what was that track?"** We already have ICY titles; keeping
  a rolling history of what played, with timestamps, is nearly free and
  surprisingly useful. Actual audio recording is a different (and legally
  murkier) matter.
- **Multi-room.** Two Pis, synchronised. Very hard to do well (clock sync),
  and probably the moment to admit that Snapcast exists.
- **Metrics.** The soak test numbers in `notes/plan.md` were measured by hand.
  A `/metrics` endpoint (CPU, RSS, reconnect count, stream uptime) would make
  regressions visible instead of anecdotal.
- **Stream health.** Reconnect-with-backoff already works; surfacing "this
  station has dropped 5 times in an hour" in the UI would make bad streams
  obvious — and becomes important the moment we add a directory full of
  half-dead stations.
