# Is there a standard API for what `radiod` does?

`notes/api.md` describes an API we invented: `POST /play`, `/pause`,
`/resume`, `/stop`, `/volume`, `/mute`, `/unmute`, `GET /status`. This note
answers the question "should we have used something standard instead?"

**Short answer:** there is no standard to adopt. No IETF RFC covers
controlling a local audio renderer, and the de facto standards that do
(MPRIS, MPD, UPnP AV) are all non-HTTP and all model something slightly
different from what we're doing. Our API stays as it is.

**But** two findings are worth keeping:

1. If we ever want ecosystem reach — Home Assistant, phone remotes, "Play
   To" from a laptop — the cheapest single thing to implement is a **subset
   of the MPD protocol**, because bridges already exist that turn MPD into
   MPRIS and into UPnP. One protocol, three ecosystems.
2. Every one of these standards defines volume as **absolute, 0–100 (or
   0.0–1.0) meaning full scale**. Ours means *percentage of `max_volume`*.
   That mismatch has a specific, non-obvious consequence for any future
   front-end — see "The volume mismatch" below. It does not endanger the
   cap, but it will make remote sliders behave strangely if we get it wrong.

---

## Is there an official standard? (RFC / ISO / W3C)

Essentially no. What exists:

### RTSP — RFC 2326, obsoleted by RFC 7826

The closest thing to an official "play/pause over a network" protocol.
HTTP-like syntax, `PLAY`, `PAUSE`, `SETUP`, `TEARDOWN`, plus `GET_PARAMETER`
/ `SET_PARAMETER` as an extension escape hatch.

It is **not applicable**. RTSP controls the *delivery of a stream from a
server to a client* — the client is the thing being fed, and PAUSE means
"stop sending me bytes". `radiod` is on the other side of that relationship:
it is the client of SomaFM's Icecast server, and the thing being controlled
is our own speaker output. RTSP has no concept of a renderer's volume, mute,
or output device at all — no volume anywhere in the spec. Modelling our
daemon as an RTSP server would mean using `SET_PARAMETER` with parameters we
invented, i.e. exactly the bespoke API we already have, wearing a costume.

### RFC 9457 — Problem Details for HTTP APIs

The one genuinely applicable RFC, and it only covers error bodies:
`application/problem+json` with `type`, `title`, `status`, `detail`,
`instance`. It obsoletes RFC 7807.

We currently return `{"error": "cannot fetch playlist: …"}`. Conforming
would mean `{"type": "…", "title": "…", "status": 502, "detail": "…"}` and a
different `Content-Type` on error responses.

Worth doing? Marginal. The value of RFC 9457 is machine-readable error
*types* for API consumers you don't control; we have exactly one consumer,
in the same repo, on loopback, and it renders errors as text. Filed as a
"if we ever publish the API" item, not as work.

### W3C / others

- **Media Session API** and **Remote Playback API** are browser-side: they
  let a web page publish metadata to OS media controls, or push a
  `<video>` to a second screen. Neither defines a server API.
- **Matter** (CSA) has a Media Playback cluster (`0x0506`) and Level
  Control, but it was designed around TVs and streaming boxes, and there's
  a certification wall. Already covered in `notes/ideas.md` — nothing in
  this research changes that entry's conclusion ("cheap to investigate,
  expensive to build, park it").
- **UPnP AV** (below) is a formal standardised spec — the UPnP Forum's
  MediaRenderer DCP, now maintained by the Open Connectivity Foundation. It
  is the only *official* standard here that actually models what we do.

---

## The de facto standards

### MPRIS — freedesktop.org, D-Bus

The Linux desktop standard. `org.mpris.MediaPlayer2.Player` on the
`/org/mpris/MediaPlayer2` object path:

| MPRIS | Ours |
|---|---|
| `Play()`, `Pause()`, `Stop()` | `POST /play` (resume case), `/pause`, `/stop` |
| `PlayPause()` | — (the website has separate buttons) |
| `OpenUri(Uri)` | `POST /play {"playlist_url": …}` |
| `PlaybackStatus`: `Playing` / `Paused` / `Stopped` | `state`: `playing` / `paused` / `stopped` |
| `Metadata` (dict, `xesam:title` etc.) | `icy_title`, `icy_name` |
| `Volume` (double, 0.0 = mute) | `volume` (integer) |
| `CanPlay`, `CanPause`, `CanSeek`, `CanControl` | — |

Our state vocabulary is already MPRIS's, lowercased — which is a decent
sign the model isn't eccentric.

Two things make it a poor fit as our *primary* API. It's D-Bus, so the PHP
site would need a D-Bus binding to talk to a daemon on the same box, which
is strictly more machinery than an HTTP GET. And its `Volume` is a double
where 1.0 is "normal" and **values above 1.0 are explicitly allowed** —
the spec has no notion of a hard ceiling, so a compliant client is entitled
to write 2.0. Our gain path would clamp it (that's the invariant, and it
holds by construction), but "the standard says you may, we silently won't"
is a bad contract to sign up for.

Where MPRIS *would* pay off: media keys, Bluetooth AVRCP (`mpris-proxy`),
and desktop applets. Not things a headless Pi in a studio needs.

### MPD — the Music Player Daemon protocol

Line-based UTF-8 over TCP, `OK` / `ACK [error]` responses, greeting
`OK MPD <version>`. Commands we'd care about: `status`, `play`, `pause 0|1`,
`stop`, `setvol 0-100`, `getvol`, `currentsong`, `add`/`addid` (which accept
**remote HTTP stream URLs**), and `idle` — a long-poll that blocks until
something changes and names the changed subsystem.

This is the closest match to what we're actually doing, for a simple reason:
MPD has played internet radio streams for twenty years, so its model already
contains the awkward cases (a "song" with no duration, a title that changes
mid-track). `status` returns `state: play|stop|pause` and `volume: 0-100`;
`currentsong` carries the ICY title. That's `GET /status`, near enough.

What it drags in that we don't have: MPD is built around a *queue* and a
*music database*. A radio has neither. An MPD front-end for `radiod` means
faking a one-item queue and answering enough of `commands`, `outputs`,
`tagtypes`, `lsinfo`, `plchanges` etc. to keep real clients from falling
over. That's the risk noted in `notes/ideas.md` — how much of the protocol
a given client actually pokes at.

The payoff is disproportionate, though, and bigger than that entry gives it
credit for. Speaking MPD gets us, for one implementation:

- Home Assistant's first-party MPD integration.
- `mpc`, `ncmpcpp`, and the large ecosystem of MPD phone remotes.
- **MPRIS for free** via `mpDris2`, an existing bridge.
- **UPnP/DLNA for free** via `upmpdcli`, a UPnP MediaRenderer front-end
  built on MPD — so we'd get "Play To" without linking a UPnP stack into
  `radiod`.
- `idle` would replace the website's status polling with a push.

Precedent that a partial implementation is a normal thing to ship: the
Nuclear player exposes an MPD-compatible server implementing only the subset
needed for playback control and notifications, and it works with `mpc`,
`ncmpcpp` and `mpDris2`.

### UPnP AV MediaRenderer — the official one

Three services: **AVTransport** (`SetAVTransportURI`, `Play`, `Pause`,
`Stop`, `Seek`, `GetTransportInfo`), **RenderingControl**
(`SetVolume`/`GetVolume`, `SetMute`/`GetMute`), and ConnectionManager.
That maps onto our API almost field for field — `SetAVTransportURI` is
`POST /play`, and `RenderingControl` is `/volume` + `/mute`. It's the only
standard here that has *both* transport control and a renderer volume with
a separate mute flag, which is our exact shape.

The cost is the wire format: SSDP discovery, SOAP/XML actions, GENA
eventing with subscription renewals, and an XML device description. That's a
lot of surface for an ARM1176 and directly against the few-dependencies
rule. Also worth knowing: of RenderingControl's 30-odd optional actions,
only `Get`/`SetVolume` and `Get`/`SetMute` are widely implemented in
practice, so the effective standard is much smaller than the document.

**OpenHome** (Linn's `ohMedia`) is the notable variant — UPnP base protocols
with different, better services, de facto among hi-fi streamers, with a
first-party Home Assistant integration. Same transport cost.

If we ever want this, `upmpdcli` in front of an MPD interface is the sane
route, not implementing UPnP ourselves.

### The vendor HTTP APIs — and why they don't help

Sonos, Roon, Kodi, VLC, mpv, Snapcast and Squeezebox all expose control
APIs, and several are HTTP+JSON — the Sonos Control API is REST/JSON, Kodi
is JSON-RPC, VLC has `/requests/status.json?command=pl_pause`. They are all
mutually incompatible and all bespoke. Sonos additionally uses SOAP for its
music-service API and is cloud-mediated.

The conclusion from surveying them is the useful part: **the industry
converged on HTTP as a transport and on nothing at all as a schema.** There
is no REST standard for this waiting to be adopted. Anyone building what we
built made up an API too.

### Home Assistant's `media_player` — the de facto *vocabulary*

Not a wire protocol, but the closest thing to an agreed semantic model,
because every integration above eventually gets mapped onto it:
`media_play`, `media_pause`, `media_play_pause`, `media_stop`,
`volume_set`, `volume_mute`, `volume_up`/`down`, `play_media`, plus a
`supported_features` bitmask so a device can advertise the subset it does.

Our API is a strict subset of this vocabulary with the same meanings. It's a
reasonable sanity check that we named things the way the rest of the world
names them.

---

## The volume mismatch (the finding that actually matters)

Every standard surveyed defines volume the same way: an absolute level where
the maximum means **full scale**. MPD `setvol 0–100`. UPnP `SetVolume` with
a device-declared range. MPRIS `Volume` as a double where 1.0 is normal and
higher is allowed.

Ours is deliberately different (`notes/api.md`): the *request* is a
percentage of `max_volume`, and the *reported* `volume` is the effective
device value, so with `max_volume = 50` a request of 100 reports back 50.

That asymmetry is right for our website, which shows a percentage slider and
also displays the cap. It is wrong for any standard client, and the failure
is silent and confusing rather than loud:

- A remote sets volume to 100 and reads back 50, so the slider jumps to
  half. It sets 100 again. It reads 50 again. The UI looks broken.
- Worse, a client that treats the read-back as authoritative will fight
  itself.

So the rule for any future MPD/UPnP/MPRIS front-end: **report the requested
percentage, not the effective device value.** `setvol 100` → the cap, and
`status` says `volume: 100`. The remote's "maximum" is our cap, and it is
telling the truth about the only scale that client can express.

The safety invariant is unaffected either way — every one of these paths
still lands in `volume::effective_volume()` before any sample is scaled, and
that is the only function that produces a gain. A front-end changes what
number a client sees, never what the speakers get. But it's worth writing
the rule down now, because "just pass the number through" is the obvious
implementation and it's the wrong one.

Second-order consequence: `max_volume` has no representation in any of these
protocols. A standard client cannot discover it. That's fine — it just means
the cap is invisible rather than negotiable, which is arguably how a safety
limit should behave.

---

## Conclusion

**Keep the REST API.** It is single-consumer, loopback-only, trivially
debuggable with `curl`, and it expresses two things no standard does: a
station identified by its playlist URL (with the already-playing /
already-paused special cases the website relies on), and a volume scale
defined relative to a hard cap. Adopting a standard as the *primary*
interface would mean adding a D-Bus or SOAP dependency to make a PHP page
talk to a daemon on the same machine — strictly worse on every axis we care
about.

**If ecosystem reach becomes a goal**, add an MPD front-end as a *second*
interface, leaving the REST API as the website's. This confirms the leaning
already recorded in `notes/ideas.md` ("Make the player show up in Home
Assistant", option 2) and adds the reason that entry was missing: MPD is not
just one integration, it's the hub that `mpDris2` and `upmpdcli` bridge from,
so it buys MPRIS and UPnP without further work.

That is still gated on the same unresolved question as before — it means
something other than the local website talks to the daemon, which means
binding off loopback and needs the security conversation `notes/ideas.md`
flags. Nothing here changes that; the protocol choice was never the blocker.

---

## Sources

- [RFC 7826 — Real-Time Streaming Protocol 2.0](https://www.rfc-editor.org/rfc/rfc7826.html)
- [RFC 9457 — Problem Details for HTTP APIs](https://www.rfc-editor.org/rfc/rfc9457.html)
- [MPRIS D-Bus Interface Specification v2.2](https://specifications.freedesktop.org/mpris/latest/) — [Player interface](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html)
- [MPD protocol reference](https://mpd.readthedocs.io/en/stable/protocol.html) — [MPD clients](https://www.musicpd.org/clients/)
- [UPnP MediaRenderer:3 device spec (PDF)](https://upnp.org/specs/av/UPnP-av-MediaRenderer-v3-Device-20101231.pdf) — [UPnP AV Architecture:1 (PDF)](https://upnp.org/specs/av/UPnP-av-AVArchitecture-v1-20020625.pdf)
- [OpenHome / ohMedia](http://openhome.org/pages/develop/overview.html) — [Home Assistant OpenHome integration](https://www.home-assistant.io/integrations/openhome/)
- [upmpdcli — a UPnP MediaRenderer built on MPD](https://www.lesbonscomptes.com/upmpdcli/)
- [Nuclear's MPD-compatible server](https://docs.nuclearplayer.com/nuclear/integrations/mpd-server) — precedent for a partial implementation
- [Home Assistant media player entity](https://developers.home-assistant.io/docs/core/entity/media-player/) — [MPD integration](https://www.home-assistant.io/integrations/mpd/) — [DLNA DMR integration](https://www.home-assistant.io/integrations/dlna_dmr/)
- [Matter 1.2 Application Cluster Specification (PDF)](https://csa-iot.org/wp-content/uploads/2023/10/Matter-1.2-Application-Cluster-Specification.pdf)
- [Sonos Control API](https://docs.sonos.com/docs/control) — [Sonos Music API (SOAP)](https://docs.sonos.com/docs/smapi)
