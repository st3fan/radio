//! The built-in website: the PHP site absorbed into radiod.
//!
//! One hyper server serves the page, the HTMX actions, the JSON API and
//! the embedded static assets. Templates and assets are compiled into the
//! binary; `--web-dir` loads them from disk instead for PHP-style
//! edit-and-reload during development. Form posts follow POST-redirect-GET
//! for plain browsers; HTMX requests get the refreshed page back directly
//! (the client swaps `#app` out of it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::player::Command;
use crate::server::App;
use crate::status::{AudioSource, State};

const SOMAFM_CHANNELS_URL: &str = "https://api.somafm.com/channels.json";
const CHANNELS_CACHE_TTL: Duration = Duration::from_secs(300);

/// What a web route produced; `server::handle` turns it into a response.
pub enum Reply {
    Html(u16, String),
    /// 303 See Other — POST-redirect-GET for non-HTMX form posts.
    Redirect(String),
    Asset(&'static str, Vec<u8>),
    /// AirPlay cover art: cacheable for a short while (the page busts
    /// the URL per track via `?v=N`, so polls stay cheap).
    Artwork(&'static str, Vec<u8>),
    NotFound,
}

/// Fetches the raw channels.json (blocking; runs on the blocking pool).
/// Injected so tests never touch the network.
pub type ChannelsFetcher = Box<dyn Fn() -> Result<String, String> + Send + Sync>;

pub struct Web {
    web_dir: Option<PathBuf>,
    fetcher: ChannelsFetcher,
    cache: Mutex<Option<(Instant, Arc<Vec<Channel>>)>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: String,
    pub title: String,
    pub description: String,
    pub genre: String,
    pub listeners: i64,
    #[serde(skip)]
    pub playlist_url: String,
    /// Channel artwork (SomaFM CDN), hotlinked like the rest of the
    /// channel data; shown only in the now-playing section.
    #[serde(skip)]
    pub image: String,
    /// Filled per render.
    pub is_current: bool,
}

fn default_fetcher() -> ChannelsFetcher {
    Box::new(|| {
        ureq::get(SOMAFM_CHANNELS_URL)
            .call()
            .map_err(|err| err.to_string())?
            .body_mut()
            .read_to_string()
            .map_err(|err| err.to_string())
    })
}

impl Web {
    pub fn new(web_dir: Option<PathBuf>) -> Web {
        Web::with_fetcher(web_dir, default_fetcher())
    }

    pub fn with_fetcher(web_dir: Option<PathBuf>, fetcher: ChannelsFetcher) -> Web {
        Web {
            web_dir,
            fetcher,
            cache: Mutex::new(None),
        }
    }

    /// The channel list, disk^H^H^H^Hmemory-cached for five minutes; a
    /// failed fetch falls back to the stale cache rather than an empty
    /// page (same behavior the PHP site had).
    async fn channels(self: &Arc<Self>) -> Option<Arc<Vec<Channel>>> {
        if let Some((fetched, list)) = self.cache.lock().expect("cache lock").clone()
            && fetched.elapsed() < CHANNELS_CACHE_TTL
        {
            return Some(list);
        }
        let this = self.clone();
        let fresh = tokio::task::spawn_blocking(move || (this.fetcher)())
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
        match fresh.ok().and_then(|raw| parse_channels(&raw)) {
            Some(list) => {
                let list = Arc::new(list);
                *self.cache.lock().expect("cache lock") = Some((Instant::now(), list.clone()));
                Some(list)
            }
            None => {
                eprintln!("radiod: web: fetching channels.json failed, using stale cache");
                self.cache
                    .lock()
                    .expect("cache lock")
                    .clone()
                    .map(|(_, list)| list)
            }
        }
    }
}

/// Parses channels.json: sorted by listeners descending, with the playlist
/// URL chosen like the PHP site did — highest-quality mp3, then any mp3,
/// then the conventional somafm.com/{id}.pls.
fn parse_channels(raw: &str) -> Option<Vec<Channel>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let entries = value.get("channels")?.as_array()?;
    let mut channels: Vec<Channel> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let mp3: Vec<&serde_json::Value> = entry
                .get("playlists")
                .and_then(|p| p.as_array())
                .map(|playlists| {
                    playlists
                        .iter()
                        .filter(|p| {
                            p.get("format").and_then(|f| f.as_str()) == Some("mp3")
                                && p.get("url").and_then(|u| u.as_str()).is_some()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let playlist_url = mp3
                .iter()
                .find(|p| p.get("quality").and_then(|q| q.as_str()) == Some("highest"))
                .or_else(|| mp3.first())
                .and_then(|p| p.get("url").and_then(|u| u.as_str()))
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://somafm.com/{id}.pls"));
            Some(Channel {
                image: text(entry, "largeimage")
                    .or_else(|| text(entry, "image"))
                    .unwrap_or_default(),
                title: text(entry, "title").unwrap_or_else(|| id.clone()),
                description: text(entry, "description").unwrap_or_default(),
                genre: text(entry, "genre").unwrap_or_default().replace('|', " · "),
                // SomaFM serves listeners as a JSON string.
                listeners: entry
                    .get("listeners")
                    .map(|l| l.as_i64().unwrap_or_else(|| text_i64(l)))
                    .unwrap_or(0),
                playlist_url,
                is_current: false,
                id,
            })
        })
        .collect();
    channels.sort_by_key(|channel| std::cmp::Reverse(channel.listeners));
    Some(channels)
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn text_i64(value: &serde_json::Value) -> i64 {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// A chosen channel-list ordering, carried in the query string so it
/// survives polls, actions and redirects. `None` is the default view
/// (listeners, busiest first, no indicator).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sort {
    key: SortKey,
    asc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SortKey {
    Station,
    Genre,
    Listeners,
}

impl SortKey {
    fn name(self) -> &'static str {
        match self {
            SortKey::Station => "station",
            SortKey::Genre => "genre",
            SortKey::Listeners => "listeners",
        }
    }

    /// The direction a column starts with when first clicked.
    fn first_click_asc(self) -> bool {
        !matches!(self, SortKey::Listeners)
    }
}

fn sort_from_query(pairs: &[(String, String)]) -> Option<Sort> {
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let key = match value("sort")? {
        "station" => SortKey::Station,
        "genre" => SortKey::Genre,
        "listeners" => SortKey::Listeners,
        _ => return None,
    };
    let asc = match value("dir") {
        Some("asc") => true,
        Some("desc") => false,
        _ => key.first_click_asc(),
    };
    Some(Sort { key, asc })
}

/// The query-string suffix that keeps a chosen sort alive across polls,
/// form posts and redirects ("" for the default view).
fn sort_query(sort: Option<Sort>) -> String {
    match sort {
        None => String::new(),
        Some(sort) => format!(
            "?sort={}&dir={}",
            sort.key.name(),
            if sort.asc { "asc" } else { "desc" }
        ),
    }
}

/// Routes a request to the website; `None` means "not a web path" and the
/// caller falls through to the JSON API.
pub async fn route(
    method: &hyper::Method,
    path: &str,
    query: Option<&str>,
    body: &str,
    hx_request: bool,
    app: &App,
) -> Option<Reply> {
    let pairs = query.map(form_pairs).unwrap_or_default();
    let sort = sort_from_query(&pairs);
    match (method, path) {
        (&hyper::Method::GET, "/") => {
            let error = pairs
                .into_iter()
                .find(|(k, _)| k == "error")
                .map(|(_, v)| v);
            Some(render_page(app, error, sort).await)
        }
        (&hyper::Method::POST, "/") => Some(handle_action(app, body, hx_request, sort).await),
        (&hyper::Method::GET, "/debug.html") => Some(render_debug_page(app)),
        (&hyper::Method::GET, "/airplay/artwork") => Some(serve_artwork(app)),
        (&hyper::Method::GET, _) => serve_asset(app, path),
        _ => None,
    }
}

/// The diagnostic page (plan 20260829-01): the same snapshot as the JSON
/// `GET /debug`, rendered as a self-refreshing table. `/debug.html`
/// because `/debug` is the JSON API's; both are unstable by contract.
fn render_debug_page(app: &App) -> Reply {
    let state = {
        let status = app.status.lock().expect("status lock poisoned");
        match status.state {
            State::Playing => "playing",
            State::Paused => "paused",
            State::Stopped => "stopped",
        }
    };
    let context = app.debug.snapshot(state);
    let mut env = minijinja::Environment::new();
    let source;
    let template = match &app.web.web_dir {
        Some(dir) => match std::fs::read_to_string(dir.join("debug.html")) {
            Ok(contents) => {
                source = contents;
                source.as_str()
            }
            Err(err) => return Reply::Html(500, format!("cannot read debug.html: {err}")),
        },
        None => include_str!("../web/debug.html"),
    };
    let render = env
        .add_template("debug.html", template)
        .and_then(|()| env.get_template("debug.html")?.render(context));
    match render {
        Ok(html) => Reply::Html(200, html),
        Err(err) => {
            eprintln!("radiod: web: debug template error: {err}");
            Reply::Html(500, format!("template error: {err}"))
        }
    }
}

/// The sender's cover art, only while an AirPlay session owns the
/// pipeline (outside one there is nothing current to show — 404).
fn serve_artwork(app: &App) -> Reply {
    let status = app.status.lock().expect("status lock poisoned");
    if status.source != AudioSource::Airplay {
        return Reply::NotFound;
    }
    match &status.airplay_artwork {
        Some(artwork) => {
            // The sender's content type, pinned to the values we expect;
            // anything else is still an image to the browser's sniffer.
            let content_type = match artwork.content_type.as_str() {
                "image/jpeg" => "image/jpeg",
                "image/png" => "image/png",
                _ => "application/octet-stream",
            };
            Reply::Artwork(content_type, artwork.data.as_ref().clone())
        }
        None => Reply::NotFound,
    }
}

/// The embedded assets. `--web-dir` overrides them from disk (dev).
fn serve_asset(app: &App, path: &str) -> Option<Reply> {
    let (name, content_type): (&str, &'static str) = match path {
        "/style.css" => ("style.css", "text/css"),
        "/htmx.min.js" => ("htmx.min.js", "text/javascript"),
        "/manifest.json" => ("manifest.json", "application/manifest+json"),
        "/icon-180.png" => ("icon-180.png", "image/png"),
        "/icon-192.png" => ("icon-192.png", "image/png"),
        "/icon-512.png" => ("icon-512.png", "image/png"),
        "/icon-maskable-512.png" => ("icon-maskable-512.png", "image/png"),
        _ => return None,
    };
    if let Some(dir) = &app.web.web_dir {
        return match std::fs::read(dir.join(name)) {
            Ok(bytes) => Some(Reply::Asset(content_type, bytes)),
            Err(_) => Some(Reply::NotFound),
        };
    }
    let bytes: &'static [u8] = match name {
        "style.css" => include_bytes!("../web/style.css"),
        "htmx.min.js" => include_bytes!("../web/htmx.min.js"),
        "manifest.json" => include_bytes!("../web/manifest.json"),
        "icon-180.png" => include_bytes!("../web/icon-180.png"),
        "icon-192.png" => include_bytes!("../web/icon-192.png"),
        "icon-512.png" => include_bytes!("../web/icon-512.png"),
        "icon-maskable-512.png" => include_bytes!("../web/icon-maskable-512.png"),
        _ => unreachable!(),
    };
    Some(Reply::Asset(content_type, bytes.to_vec()))
}

/// Handles a form action. Plain forms get POST-redirect-GET (a refresh
/// never repeats an action); HTMX gets the refreshed page directly.
async fn handle_action(app: &App, body: &str, hx_request: bool, sort: Option<Sort>) -> Reply {
    let form = form_pairs(body);
    let field = |name: &str| {
        form.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let result = match field("action").unwrap_or("") {
        "play" => action_play(app, field("channel").unwrap_or("")).await,
        "stop" => {
            app.player.send(Command::Stop);
            Ok(())
        }
        "pause" => match snapshot(app).0 {
            State::Playing => {
                app.player.send(Command::Pause);
                Ok(())
            }
            State::Paused => Ok(()),
            State::Stopped => Err("nothing is playing".to_string()),
        },
        "resume" => match snapshot(app).0 {
            State::Paused => {
                app.player.send(Command::Resume);
                Ok(())
            }
            State::Playing => Ok(()),
            State::Stopped => Err("nothing is playing".to_string()),
        },
        "mute" => set_muted(app, true),
        "unmute" => set_muted(app, false),
        "volume" => match field("volume").and_then(|v| v.parse::<u8>().ok()) {
            Some(volume) if volume <= 100 => {
                app.status.lock().expect("status lock poisoned").volume = volume;
                Ok(())
            }
            _ => Err("volume must be a number between 0 and 100".to_string()),
        },
        _ => Err("unknown action".to_string()),
    };

    let error = result.err();
    if hx_request {
        // Playback commands are asynchronous: give the player a moment to
        // switch before rendering, so the swapped-in page usually shows
        // the new state (the poll converges any stragglers).
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        render_page(app, error, sort).await
    } else {
        let base = sort_query(sort);
        match error {
            None if base.is_empty() => Reply::Redirect("/".to_string()),
            None => Reply::Redirect(format!("/{base}")),
            Some(message) => {
                let sep = if base.is_empty() { "?" } else { "&" };
                Reply::Redirect(format!("/{base}{sep}error={}", url_encode(&message)))
            }
        }
    }
}

fn set_muted(app: &App, muted: bool) -> Result<(), String> {
    app.status.lock().expect("status lock poisoned").muted = muted;
    Ok(())
}

fn snapshot(app: &App) -> (State, AudioSource) {
    let status = app.status.lock().expect("status lock poisoned");
    (status.state, status.source)
}

/// The play action: channel id → playlist URL (from our own channel data
/// only) → the same resolve-and-play path the JSON API uses.
async fn action_play(app: &App, channel_id: &str) -> Result<(), String> {
    if snapshot(app).1 == AudioSource::Airplay {
        return Err("airplay session active".to_string());
    }
    let channels = app
        .web
        .channels()
        .await
        .ok_or_else(|| "channel list is unavailable".to_string())?;
    let playlist_url = channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.playlist_url.clone())
        .ok_or_else(|| "unknown channel".to_string())?;
    crate::server::start_playlist(app, playlist_url)
        .await
        .map_err(|(_code, message)| message)
}

#[derive(Serialize)]
struct PageContext {
    /// Compiled-in crate version; the banner links to its release page.
    version: &'static str,
    /// Non-empty (the short git hash) for unofficial builds: the banner
    /// then reads RADIO DEV (<hash>) and links to the commit.
    dev_hash: &'static str,
    error: Option<String>,
    state: &'static str,
    prompt: &'static str,
    title: String,
    title_dim: bool,
    icy_name: String,
    volume: u8,
    volume_filled: usize,
    volume_up: u8,
    volume_down: u8,
    muted: bool,
    mixer_warning: Option<String>,
    airplay_active: bool,
    /// Artwork of the station that is playing (or paused); None renders
    /// the empty frame so the layout never moves.
    now_image: Option<String>,
    /// The AirPlay station line: "ARTIST — ALBUM" when the sender
    /// pushed metadata, else the negotiated stream ("44100 HZ · 2 CH ·
    /// AAC").
    airplay_stream: String,
    /// Versioned URL of the sender's cover art (`/airplay/artwork?v=N`),
    /// None while there is none — the airwaves mark fills the box then.
    airplay_artwork: Option<String>,
    channels: Option<Vec<Channel>>,
    /// "" for the default view, else "?sort=..&dir=.." — baked into the
    /// poll URL and form targets so the chosen order survives swaps.
    sort_query: String,
    columns: Vec<ColumnHeader>,
}

#[derive(Serialize)]
struct ColumnHeader {
    label: &'static str,
    href: String,
    /// "", " ▴" or " ▾".
    indicator: &'static str,
    class: &'static str,
}

fn column_headers(sort: Option<Sort>) -> Vec<ColumnHeader> {
    [
        (SortKey::Station, "STATION", ""),
        (SortKey::Genre, "GENRE", "col-genre"),
        (SortKey::Listeners, "LSNRS", "num"),
    ]
    .into_iter()
    .map(|(key, label, class)| {
        let chosen = sort.filter(|s| s.key == key);
        let next_asc = match chosen {
            Some(sort) => !sort.asc,
            None => key.first_click_asc(),
        };
        ColumnHeader {
            label,
            href: sort_query(Some(Sort { key, asc: next_asc })),
            indicator: match chosen {
                None => "",
                Some(Sort { asc: true, .. }) => " ▴",
                Some(Sort { asc: false, .. }) => " ▾",
            },
            class,
        }
    })
    .collect()
}

async fn render_page(app: &App, error: Option<String>, sort: Option<Sort>) -> Reply {
    let channels = app.web.channels().await;
    let context = {
        let status = app.status.lock().expect("status lock poisoned");
        let airplay_active = status.source == AudioSource::Airplay;
        let state = match status.state {
            State::Playing => "playing",
            State::Paused => "paused",
            State::Stopped => "stopped",
        };
        let prompt = if airplay_active {
            "STREAMING OPENAIRPLAY"
        } else {
            match status.state {
                State::Playing => "NOW PLAYING",
                State::Paused => "PAUSED",
                State::Stopped => "STANDBY",
            }
        };
        let mut title = status.icy_title.clone().unwrap_or_default();
        let mut title_dim = false;
        if airplay_active {
            // DMAP track metadata when the sender pushed any; the
            // placeholder only for senders that send none.
            match status.airplay_track.as_ref().and_then(|t| t.title.clone()) {
                Some(track_title) => title = track_title,
                None => {
                    title = "— NO TRACK INFO —".to_string();
                    title_dim = true;
                }
            }
        } else if status.state == State::Paused {
            // Stopped-but-remembered: the song is gone, the station is
            // kept — just the cursor blinking on an empty line.
            title = String::new();
        } else if title.is_empty() && status.state == State::Stopped {
            title = "— NO SIGNAL —".to_string();
            title_dim = true;
        }
        let now_image = channels.as_ref().and_then(|list| {
            let playing = status.playlist_url.as_deref()?;
            list.iter()
                .find(|c| c.playlist_url == playing && !c.image.is_empty())
                .map(|c| c.image.clone())
        });
        let channels = channels.map(|list| {
            let mut list: Vec<Channel> = list
                .iter()
                .map(|channel| {
                    let mut channel = channel.clone();
                    channel.is_current = !airplay_active
                        && status.playlist_url.as_deref() == Some(channel.playlist_url.as_str());
                    channel
                })
                .collect();
            if let Some(sort) = sort {
                match sort.key {
                    SortKey::Station => list.sort_by_key(|c| c.title.to_lowercase()),
                    SortKey::Genre => list.sort_by_key(|c| c.genre.to_lowercase()),
                    SortKey::Listeners => list.sort_by_key(|c| c.listeners),
                }
                if !sort.asc {
                    list.reverse();
                }
            }
            list
        });
        PageContext {
            version: env!("CARGO_PKG_VERSION"),
            dev_hash: env!("RADIOD_DEV_HASH"),
            error,
            state,
            prompt,
            title,
            title_dim,
            icy_name: status.icy_name.clone().unwrap_or_default(),
            volume: status.volume,
            volume_filled: volume_filled(status.volume),
            volume_up: (status.volume + 10).min(100),
            volume_down: status.volume.saturating_sub(10),
            muted: status.muted,
            mixer_warning: (status.mixer != "ok" && status.mixer != "disabled")
                .then(|| status.mixer.clone()),
            airplay_active,
            // ARTIST — ALBUM when the sender told us; the negotiated
            // stream line is the fallback for metadata-less senders.
            airplay_stream: status
                .airplay_track
                .as_ref()
                .and_then(|track| {
                    let parts: Vec<&str> = [track.artist.as_deref(), track.album.as_deref()]
                        .into_iter()
                        .flatten()
                        .collect();
                    (!parts.is_empty()).then(|| parts.join(" — "))
                })
                .or_else(|| {
                    status
                        .airplay
                        .map(|info| format!("{} HZ · {} CH · AAC", info.rate, info.channels))
                })
                .unwrap_or_default(),
            airplay_artwork: status
                .airplay_artwork
                .as_ref()
                .map(|artwork| format!("/airplay/artwork?v={}", artwork.version)),
            now_image,
            channels,
            sort_query: sort_query(sort),
            columns: column_headers(sort),
        }
    };
    match render_template(app, &context) {
        Ok(html) => Reply::Html(200, html),
        Err(err) => {
            eprintln!("radiod: web: template error: {err}");
            Reply::Html(500, format!("template error: {err}"))
        }
    }
}

fn render_template(app: &App, context: &PageContext) -> Result<String, minijinja::Error> {
    let mut env = minijinja::Environment::new();
    let source;
    let template = match &app.web.web_dir {
        // Dev: read from disk every render — edit, reload, no recompile.
        Some(dir) => {
            source = std::fs::read_to_string(dir.join("index.html")).map_err(|err| {
                minijinja::Error::new(minijinja::ErrorKind::TemplateNotFound, err.to_string())
            })?;
            source.as_str()
        }
        None => include_str!("../web/index.html"),
    };
    env.add_template("index.html", template)?;
    env.get_template("index.html")?.render(context)
}

/// How many of the volume bar's 20 segments are filled (nearest-rounded,
/// matching the old text rendering). The template turns each segment into
/// a button that sets that position's volume.
fn volume_filled(volume: u8) -> usize {
    let segments = 20usize;
    ((usize::from(volume.min(100)) * segments + 50) / 100).min(segments)
}

/// Minimal application/x-www-form-urlencoded parsing: enough for our own
/// forms (`action`, `channel`, `volume`) and the `error` query parameter.
fn form_pairs(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                if let (Some(h), Some(l)) = (
                    bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                    bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
                ) {
                    out.push((h * 16 + l) as u8);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn fixed_channels_fetcher() -> ChannelsFetcher {
        Box::new(|| {
            Ok(r#"{"channels": [
                {"id": "groovesalad", "title": "Groove Salad", "description": "chill", "genre": "ambient|electronica", "listeners": "250",
                 "image": "https://somafm.com/img/groovesalad120.png", "largeimage": "https://somafm.com/img3/groovesalad-400.jpg",
                 "playlists": [{"url": "https://api.somafm.com/groovesalad130.pls", "format": "mp3", "quality": "highest"},
                               {"url": "https://api.somafm.com/groovesalad.pls", "format": "mp3", "quality": "high"}]},
                {"id": "defcon", "title": "DEF CON Radio", "description": "hacking", "genre": "electronica", "listeners": "500",
                 "playlists": [{"url": "https://api.somafm.com/defcon.pls", "format": "aac", "quality": "highest"}]}
            ]}"#
            .to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_parse_sorts_and_picks_playlists() {
        let raw = (testing::fixed_channels_fetcher())().unwrap();
        let channels = parse_channels(&raw).unwrap();
        assert_eq!(channels.len(), 2);
        // Sorted by listeners descending.
        assert_eq!(channels[0].id, "defcon");
        // No mp3 playlist: conventional .pls fallback.
        assert_eq!(channels[0].playlist_url, "https://somafm.com/defcon.pls");
        // Highest-quality mp3 wins.
        assert_eq!(
            channels[1].playlist_url,
            "https://api.somafm.com/groovesalad130.pls"
        );
        assert_eq!(channels[1].genre, "ambient · electronica");
        assert_eq!(channels[1].listeners, 250);
    }

    #[test]
    fn volume_fill_counts_match_the_old_text_rendering() {
        assert_eq!(volume_filled(0), 0);
        assert_eq!(volume_filled(100), 20);
        assert_eq!(volume_filled(50), 10);
        assert_eq!(volume_filled(31), 6); // nearest, not ceiling
        assert_eq!(volume_filled(255), 20);
    }

    #[test]
    fn form_parsing_round_trips() {
        let pairs = form_pairs("action=play&channel=groovesalad&note=a+b%21");
        assert_eq!(pairs[0], ("action".to_string(), "play".to_string()));
        assert_eq!(pairs[1], ("channel".to_string(), "groovesalad".to_string()));
        assert_eq!(pairs[2], ("note".to_string(), "a b!".to_string()));
        assert_eq!(url_encode("a b!"), "a+b%21");
        assert_eq!(
            url_decode(&url_encode("nothing is playing")),
            "nothing is playing"
        );
    }
}
