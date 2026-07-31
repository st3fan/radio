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

## Artwork in the Phosphor UI

**Why:** the pre-Phosphor website showed SomaFM's channel artwork; the
terminal redesign dropped it, and the channel list is now pure text. The art
is genuinely good — it's a lot of what gives SomaFM its character — and
stations are much faster to recognise by picture than by name in a list.
We're already caching `channels.json`, which carries `image`, `largeimage`
and `xlimage`, so the data is sitting right there unused.

**The tension, which is the whole design problem:** Phosphor is a green CRT.
The stylesheet says it out loud — "single-theme by design: a CRT has no light
mode". Dropping full-colour JPEGs into it would look like a browser window
pasted onto a terminal and would undo the thing that makes the UI good. So
the artwork has to be *rendered as the terminal would render it*, not merely
placed on top of it.

**Options, roughly cheapest first:**

1. **CSS filters only.** `grayscale` + `contrast`, tinted toward
   `--phosphor`, sitting under the existing scanline sheen. No new
   dependencies, no server cost, about ten lines. Won't look like a real
   bitmap render, but it will look deliberate.
2. **An SVG filter** (`feComponentTransfer` to posterize to a few levels,
   `feColorMatrix` to tint). Still declarative, still no JS or PHP
   dependencies, but a genuinely 4-level poster rather than a wash — much
   closer to "a CRT drawing a bitmap".
3. **Real 1-bit dithering** — Floyd–Steinberg or an ordered Bayer matrix, in
   phosphor green on near-black. This is the one that would look *right*:
   chunky, monochrome, unmistakably a terminal displaying an image. Needs
   either canvas in the browser or image processing on the Pi.
4. **ASCII art.** Render each logo to character cells, like `chafa` or
   `jp2a`. The most on-theme option available and pleasingly absurd — the
   whole UI is monospace already. Precompute once per channel, cache the
   text, and it costs nothing to serve. Risk: at thumbnail sizes it may just
   read as noise. Worth prototyping precisely because it's either delightful
   or unusable, with little in between.

My instinct is (3) for the now-playing panel and (1) or nothing for the list
rows — but this is a "render four versions and look at them" decision, the
same way the Phosphor direction itself was chosen from three proposals in
milestone 7.

**Where it goes:**

- **A now-playing panel** — the current station's art, larger, next to the
  ICY title. This is the one that earns its pixels, and it's a fixed cost of
  one image regardless of how many stations exist.
- **List thumbnails** — small, one per row. Careful here: the list's current
  strength is that it's dense and instantly scannable. Forty images must not
  turn it into a slow, layout-shifting scroll. Fixed dimensions, lazy
  loading, and a plain glyph placeholder when art is missing.

**Practicalities:**

- **Proxy and cache the images on the Pi** rather than hotlinking, the same
  way `channels.json` is already cached — otherwise every page load has the
  browser talking to somafm.com, which is slow on first paint and leaks
  visitors to a third party. Station art never changes, so it caches
  indefinitely; a few dozen thumbnails is trivial disk.
- **Do the expensive rendering once**, at cache-fill time, not per request —
  an ARM1176 should not be dithering images on the fly. If dithering happens
  server-side it means PHP-GD, which is a new dependency for the website
  package and needs justifying; doing it in a canvas in the browser avoids
  that entirely and costs the Pi nothing.
- **Assume the art is missing or bad.** Radio Browser stations (above) come
  with favicons of wildly varying size and quality, and some with none at
  all. Whatever we build needs a fallback that doesn't look broken.

**Worth noting:** if AirPlay lands, `shairport-sync` supplies real cover art
per track — so an artwork panel built now gets a second, better use later,
and album art changing every few minutes is a much stronger reason to have
one than a station logo that never changes. Track art for *radio* is a
different story: ICY gives us only a title string, so it would mean querying
some third-party lookup service and living with wrong matches. Not worth it.

**Effort:** small for CSS filters, a weekend for dithering done properly.
Pure polish — but polish is the entire point of the Phosphor milestone.

---

## A setup screen for the config file

**Why:** `/etc/radio/config.toml` has four fields, and today changing any of
them means SSH, an editor, and `systemctl restart radiod`. `audio_device` in
particular is the one field a new install is most likely to have wrong, and
the failure mode is silence with no clue why. A settings page in the website
would close that loop — and it's a prerequisite for the prebuilt image below,
where there is no "just SSH in" step.

**Sketch:** a `/settings` page in the website. `audio_device` picked from a
list rather than typed — the daemon already links libasound, so a
`GET /devices` endpoint enumerating cards is nicer than the website shelling
out to `aplay -l` (and it reports what `radiod` itself can actually open,
which is the question being asked). A **test tone** button next to it, so you
can confirm the device works *before* saving. `initial_volume` is a plain
number field.

**Where the writes go — this is the part that needs thought.**
`config.toml` is a dpkg conffile, deliberately, so upgrades don't clobber
local edits (`notes/plan.md`, Packaging). Having php-fpm rewrite it fights
that: dpkg would see a modified conffile and start prompting on every
upgrade. Better: a drop-in the package doesn't own — `/etc/radio/conf.d/` or
a `/var/lib/radio/settings.toml` — layered over the conffile at load time,
with the conffile staying the admin/SSH-owned base. That's a change in
`radiod`'s config loading, not just a new page.

Then the config has to take effect. Three options, in increasing niceness:
website restarts the unit (needs a polkit or sudoers rule for one specific
`systemctl restart radiod` — more privilege than the website has today), the
daemon reloads on `SIGHUP`, or — probably cleanest — the daemon owns this
entirely behind a `GET`/`POST /settings` API and the website just renders it.
The daemon is the only process that should be writing its own config.

**Two things that must not be in the form:**

- **`listen`.** It binds loopback, and that's the property that keeps the
  daemon off the LAN. A web form that can change it can undo that. Leave it
  SSH-only.
- **`max_volume`.** This is the safety cap (`CLAUDE.md`), and right now it can
  only be raised by someone with root on the box. Putting it behind an
  unauthenticated LAN page means the invariant is only as strong as "nobody
  else is on the WiFi" — which is not what we promised. Options, if we want it
  at all: allow *lowering* only, or show it read-only with a note saying where
  to change it. My inclination is to leave it out of v1 entirely and see if we
  ever miss it.

**Open questions:**

- The website has no authentication whatsoever today, which is fine when the
  worst a stranger can do is change the station. A page that reconfigures the
  device is a different blast radius. Does this want a password, or is
  "it's my WiFi" enough? (Related: the auth conversation the Home Assistant
  idea forces.)
- Recovery. If you save a broken `audio_device` and playback dies, can you
  fix it from the same page, or have you locked yourself out of the only UI?
  The test-tone-before-save flow mostly avoids this; a "reset to defaults"
  escape hatch would finish the job.
- Validation belongs wherever the write happens — the daemon should reject a
  device it can't open and a volume out of range, regardless of what the form
  allowed.

**Effort:** small for the page, medium once the layered-config and
who-writes-it questions are answered. Those answers are also what the image
needs, so it's worth doing this one first and letting the image inherit it.

---

## Sleep timer, alarm clock, scheduled streams

Three features, one mechanism: *do X at time T*. Worth designing together even
if they ship separately, because building three ad-hoc timers would be silly.

- **Sleep timer** — "stop in 45 minutes". Relative, one-shot, the thing you
  want at 23:00.
- **Alarm clock** — "wake me to Drone Zone at 07:30 on weekdays". Absolute,
  recurring, and the one with real consequences if it fails.
- **Scheduled streams** — "play the news at 08:00 and 18:00". Absolute,
  recurring, and unlike the alarm it *interrupts and then returns*: play this
  for ten minutes, then go back to whatever was on (or to silence, if that's
  what it interrupted).

### Where the scheduler lives

Cron or systemd timers poking the REST API would work and need almost no new
code, but generating unit files or crontab lines from a web UI is grim, and
neither can express "return to what was playing before". A small scheduler in
`radiod` is probably right: it already has a playback thread and mutex-guarded
state, so a "what's the next due action" check is cheap. Schedules persist in
`/var/lib/radio` (the same place the setup screen's settings would live) so
they survive restarts and upgrades.

### The Pi Zero has no real-time clock

This is the detail that makes or breaks the alarm, and it's easy to miss. A Pi
Zero W has no RTC — it has no idea what time it is until NTP answers after
boot. So:

- After a power cut the clock is wrong, then jumps once `systemd-timesyncd`
  syncs. An alarm must not fire against a pre-sync clock, and must not fire a
  volley of "missed" events when the clock jumps forward past them.
- Practical rule: don't honour any schedule until the clock is known-synced,
  and treat anything more than a few minutes overdue as missed rather than due.
- Worth being honest in the docs: if the WiFi is down at 07:00 after an
  overnight power cut, the alarm does not go off. That's inherent to the
  hardware, not something we can fix in software.

### Everything else that needs deciding

- **Local time, not UTC offsets.** Store schedules as local time plus a day
  mask; an alarm set for 07:30 must stay 07:30 across a DST change.
- **Fades come free, and safely.** We own the gain path, so a sleep timer that
  fades out over the last minute and an alarm that ramps up from quiet to
  target over a few minutes are both easy — and because they go through
  `effective_volume()`, the `max_volume` cap still holds. An alarm can never be
  louder than the cap, which is worth stating so nobody relies on it being
  louder.
- **What if the stream is dead at 07:00?** The classic internet-radio-alarm
  failure: silence, and you oversleep. Needs a fallback — a local file, or
  simply a beep — after N seconds of failing to connect. Non-negotiable for
  anything called an alarm clock.
- **Precedence.** Define it once, in a table: what happens when a scheduled
  news slot fires during a sleep-timer fade, or when you manually change
  station mid-schedule. Sane defaults: a manual action always wins and cancels
  the schedule that's running; an alarm outranks a sleep timer.
- **Visibility and escape.** The UI should show "stopping in 12 min" and
  "news at 18:00" with a one-click cancel. A timer you can't see or stop is
  worse than no timer.

### Overlap with Home Assistant

If the HA integration lands, HA automations do alarms and schedules well, and
some of this becomes redundant. But a radio that needs a home automation hub
to wake you up isn't an appliance any more — the sleep timer especially should
work on a box with nothing else on the network. Suggest: build the sleep timer
regardless (small, self-contained, immediately useful), and let the alarm and
schedules wait until we know whether HA is happening.

**Effort:** sleep timer alone is small — worth doing on its own. The full
scheduler is medium, and most of that is the clock-sync and fallback
edge cases rather than the timing logic.

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

**Sketch:** don't write the protocol ourselves. `shairport-sync` is the mature
implementation and packaged in Debian. But it must be *integrated*, not merely
installed — see the requirement below.

### Requirement: one player, not two daemons fighting over a sound card

The bar for this feature is that it feels like one appliance. Start AirPlay
from a phone and the radio stops cleanly and the phone's audio comes out of
the speakers; end the AirPlay session and the radio comes back on the station
it was playing. No clicks, no "device busy", no half-second of two things at
once, no state where the website says "playing" but the speakers say
otherwise. Anything less isn't worth shipping — a box that sometimes refuses
to play because another process grabbed the card is worse than a box that
doesn't do AirPlay.

That rules out the obvious approach (install `shairport-sync`, point it at
`plughw:0,0`, hope). Two processes opening the same ALSA device means whoever
gets there first wins and the other one errors out, and it also breaks the
volume cap, since `shairport-sync`'s samples would never pass through
`volume::effective_volume()` — a phone at full volume would drive the studio
speakers at full scale. Both problems have the same fix.

**`radiod` stays the only writer to the sound card.** `shairport-sync` never
touches it; it hands us PCM instead, and AirPlay audio becomes a second
`Source` inside the daemon, going through the same gain path and the same
sink as the radio does. The invariant holds by construction, arbitration
becomes an ordinary state machine inside one process, and there is one place
that knows what the speakers are doing.

**Transport:** `shairport-sync` has backends that write PCM to a pipe rather
than to ALSA, which is the simplest version — a named FIFO that `radiod`
reads. If the AirPlay 2 build turns out to require the ALSA backend
specifically (worth checking early — I think it may), the fallback is
`snd-aloop`: `shairport-sync` plays to a loopback device and `radiod` captures
from the other end. Same architecture, different plumbing.

**Session signalling:** we need to know a session started *before* audio
arrives, so the radio can stop first rather than during. `shairport-sync` has
session hooks that run a command when play begins and ends — pointed at
`radiod`'s loopback API, that's the trigger. Treat data appearing on the pipe
as a backstop, not the primary signal, and treat the pipe running dry mid-song
as a network hiccup (write silence, hold the session) rather than an end.

### Handoff behaviour, spelled out

| Event | Radio was… | Behaviour |
|---|---|---|
| AirPlay session starts | playing | Fade out over ~200 ms, disconnect the stream, remember the station. Switch the sink to the AirPlay source. |
| AirPlay session starts | paused / stopped | Nothing to stop; just take the sink. Remember that we were *not* playing. |
| AirPlay session ends | was playing | Reconnect the remembered station and fade back in. |
| AirPlay session ends | was paused / stopped | Stay silent. Do not surprise the room with radio. |
| Website `/play` during AirPlay | — | The person at the keyboard wins: end the AirPlay session and play the station. |
| Volume changed from either side | — | One volume, one cap. See below. |

A few consequences worth stating:

- **It's a takeover, not a mixer.** One pair of speakers, one thing playing.
  Mixing two sources is more code and worse behaviour.
- **The phone's volume slider should work**, and must not escape the cap.
  AirPlay senders transmit volume changes; map them onto `radiod`'s own volume
  value so `effective_volume()` still applies. The phone at 100% means 100% of
  `max_volume`, exactly like the website's slider. `shairport-sync` must be
  told not to apply its own volume, or we'd be attenuating twice.
- **`/status` should tell the truth**, with an AirPlay state and the sender's
  name, and the website should show it — including that the radio controls are
  inert while a phone owns the speakers. A UI that lies about what's playing
  is the failure mode this whole design exists to avoid.
- **Metadata comes along for free.** `shairport-sync` can emit what's playing
  (artist, title, album, artwork) — feeding that into `/status` alongside
  `icy_title` means the now-playing display keeps working during AirPlay, and
  the Phosphor UI needs no special case.
- **If `radiod` is down, AirPlay is down**, because nothing else can open the
  card. That's the correct coupling, not a bug — and the rule that follows is
  absolute: **never configure a fallback where `shairport-sync` writes to the
  hardware directly**, because that path bypasses the cap. Better silent than
  loud.
- **Packaging:** `shairport-sync`'s unit ordered after `radiod`, the FIFO (or
  loopback) created at startup with permissions shared between the `radio`
  user and whatever `shairport-sync` runs as, and AirPlay off by default in
  the config until someone turns it on.
- **It stays testable on macOS.** An AirPlay source that reads PCM from a pipe
  is exactly the shape the existing `Source`/`AudioSink` traits already
  assume: feed it a canned file, assert on what the dev sink received, and the
  handoff state machine gets unit tests without an Apple device or a sound
  card in the loop.

**Open questions:** does a Pi Zero W's WiFi hold up? How much latency does the
pipe or loopback add (for music, irrelevant; it only matters if we ever care
about video sync)? What does the AirPlay 2 build actually require — see below.

**Effort:** large, and it changes the audio architecture — `radiod` grows a
notion of "which source owns the sink". Prototype `shairport-sync` standalone
on the Debian PC first and just *listen* before designing any of it.

### Explore AirPlay 2 specifically

AirPlay 1 vs 2 is not a version bump you can defer — they have different
dependencies, different CPU costs, and different *value*, so it's worth
deciding deliberately rather than taking whatever the package gives us.

**What AirPlay 2 buys over AirPlay 1:**

- **It shows up in the Apple Home app.** AirPlay 2 speakers appear as Home
  accessories, which means Siri and automations without implementing HomeKit
  or Matter ourselves. If the underlying want is "control it from Apple
  stuff", this may be the cheapest route on the whole list — cheaper than
  either option in the Matter entry above. Worth weighing them against each
  other rather than treating them as unrelated ideas.
- Grouping with other AirPlay 2 speakers and HomePods (multi-room, for free,
  from someone else's clock-sync implementation — see the multi-room bullet
  at the bottom).
- Buffered audio rather than realtime streaming, which should be *kinder* to
  flaky WiFi than AirPlay 1, not harsher.

**What it costs:** `shairport-sync` needs to be built with AirPlay 2 support
plus **`nqptp`**, a separate PTP timing daemon that wants exclusive use of UDP
ports 319/320 and runs as its own service. The dependency list grows
(libplist, libsodium, and AAC decoding — though we already link FFmpeg). The
Debian package may or may not be built with AirPlay 2 enabled, so this could
mean building it ourselves, which in turn means it stops being "just apt
install".

**The question that decides it: does it run on a Pi Zero W 1.0?** My
understanding is that AirPlay 2 in `shairport-sync` is substantially heavier
than AirPlay 1 and that the original single-core Zero W is at best marginal —
but that's recollection, not measurement, and the project's own README is
explicit about supported hardware. **Read that first**; it may answer the whole
question in five minutes and save the spike entirely.

**If it doesn't fit, there are two honest outs**, and neither is a failure:

1. **Ship AirPlay 1.** It's cheap, it works, and "play whatever is on my
   phone" — the actual want — is fully satisfied by it. The Home-app trick is
   lost, but the Home Assistant route covers a lot of that ground.
2. **Change the board.** A Pi Zero 2 W is quad-core, a few dollars, and runs
   our existing 32-bit armhf binaries unmodified (ARMv6 code runs fine on
   it) — so it costs a re-flash, not a port. If AirPlay 2 turns out to be the
   one feature the hardware can't do, that's a hardware answer, not a reason
   to spend weeks optimising. Worth noting the constraint in `CLAUDE.md` is
   "Pi Zero W 1.0" because that's the box on the desk, not because ARMv6 is
   sacred.

**The spike, in order:** read the `shairport-sync` docs on AirPlay 2 hardware
requirements → if plausible, build it on the Debian PC and confirm the
experience is actually good (does it appear in Home? does grouping work?) →
only then try it on the Pi Zero and measure CPU, dropouts and stability over
an hour → and only then design the loopback integration above. Stop at
whichever step says no.

**Effort:** an afternoon to know the answer. The build-it-ourselves and
`nqptp`-packaging tail is real work if it passes.

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

**Leaning:** (2), with (4) as the fallback if the MPD spike turns ugly.
`notes/api-standards.md` surveys the protocols properly and strengthens this:
MPD isn't one integration, it's the hub that `mpDris2` (MPRIS) and `upmpdcli`
(UPnP) bridge from, so option (2) subsumes option (3) at no extra cost. It
also records the one rule a front-end must follow — report the requested
percentage, not the effective device volume. Either
way it forces a decision we've so far avoided: **letting something other than
the local website talk to the daemon.** That means an auth story (token? LAN
only? bind to a specific interface?) and re-reading the threat model in
`notes/plan.md`. Worth writing that down before writing any code.

**Effort:** medium for the protocol, small for the HA side, and a real
security-design conversation attached to both.

---

## Matter support

**Why (the appeal):** Matter is the cross-vendor smart-home standard backed by
Apple, Google, Amazon and Samsung. The pitch is that you implement it *once*
and the device shows up natively in Apple Home, Google Home, Alexa and Home
Assistant — no per-ecosystem integration, no cloud, local control, commission
by scanning a QR code. For a box like this, "it just appears in the Home app"
is genuinely the dream.

**Why it may not deliver that here — the thing to check first.** Matter is
strongest for lightbulbs, plugs, thermostats, locks and sensors. Its media
side exists (clusters like Media Playback, Channel, Audio Output, and a
Speaker device type) but it was designed around TVs and streaming boxes, and
*ecosystem support for those device types is much thinner than the
lightbulb-and-plug core*. The plausible bad outcome: we implement Matter and
Apple Home renders our internet radio as an on/off switch with a brightness
slider, which is a worse experience than a HomeKit-shaped lie we could have
told in a weekend. So the very first task is not code — it's finding out what
the current spec's media device types actually are and what each ecosystem
does with them today. Everything below is contingent on that answer.

If the device types *are* usable, the mapping is quite pretty: on/off and
level for play/stop and volume, and Matter's **Channel** cluster — a channel
list plus "change channel" — is a surprisingly natural fit for radio stations
and would pair well with the favorites idea above.

**What implementing it involves.** Matter is a much bigger protocol than
anything else on this list:

- IPv6 is mandatory (link-local is enough on the LAN, but it must work),
  plus mDNS-based discovery.
- Commissioning is a real cryptographic handshake — SPAKE2+ from a setup
  code, then certificate-based session establishment — so we'd be taking on a
  crypto stack on an ARM1176.
- The reference SDK (`connectedhomeip`) is large C++. There is a Rust
  implementation, `rs-matter`, which would fit the house style much better —
  but it's a substantial dependency either way, and "keep dependencies few" is
  a stated constraint.
- **Certification is the likely hard blocker.** Shipping a real Matter device
  means CSA membership, a vendor ID and device attestation certificates —
  thousands of dollars and paperwork, which a hobby project isn't going to do.
  Test/development credentials exist and Home Assistant will generally accept
  them, but the commercial ecosystems are the ones that may not — and they're
  the entire reason to want Matter in the first place. Worth confirming
  exactly where each one stands before anything else.

**Cheaper things that get most of the benefit:**

- If the goal is **Home Assistant**, the MPD route above is a fraction of the
  work and gives a proper media player entity. Matter buys nothing here.
- If the goal is **Apple Home specifically**, two cheaper routes exist. An
  AirPlay 2 speaker appears in the Home app on its own (see the AirPlay entry
  above) — no protocol work by us at all, if the hardware can run it. Failing
  that, HomeKit's own accessory protocol is far more tractable for a
  non-commercial project (Homebridge already does this, and there are HAP
  libraries) — with the same caveat that HomeKit has no real "radio" accessory
  type either, so it'd be a switch-plus-slider fiction.
- If the goal is **"my phone can control it from anywhere in the house"**, the
  website already does that, and a physical knob (below) may be the better
  answer to the underlying want.

**Honest summary:** the *idea* is right — one standard, every ecosystem, all
local — but for an internet radio specifically, Matter's media support and the
certification wall are both open questions, and the answers may well be "not
yet". Cheap to investigate, expensive to build. Park it until someone spends
an hour reading the current spec and poking at the Home app; revisit if a
later Matter release takes speakers seriously.

**Effort:** an hour to find out whether it's viable. Large and uncertain if it
is.

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
  needs to pick a sane device at first boot, or lean on the setup screen
  above (which is why that one should land first).
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
