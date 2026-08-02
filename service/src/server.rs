use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::TcpListener;

use crate::player::{Command, Player};
use crate::pls::PlsError;
use crate::status::{AudioSource, State, Status};

/// Maximum accepted request body; our biggest body is one playlist URL.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Resolves a playlist URL to a stream URL. Injected so routing tests do not
/// touch the network; production wires in `pls::resolve`. It runs on the
/// blocking pool (the fetch is blocking `ureq`), hence `Sync` and `Arc`.
pub type Resolver = Arc<dyn Fn(&str) -> Result<String, PlsError> + Send + Sync>;

pub struct App {
    pub status: Arc<Mutex<Status>>,
    pub player: Player,
    pub resolver: Resolver,
    pub web: Arc<crate::web::Web>,
}

#[derive(Deserialize)]
struct PlayRequest {
    playlist_url: String,
}

#[derive(Deserialize)]
struct VolumeRequest {
    volume: u8,
}

fn error_body(message: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "error": message })).expect("error serializes")
}

/// The outcome of routing a request: an HTTP status code and a JSON body.
/// Kept free of hyper's request/response types so routing is unit-testable.
async fn route(method: &Method, url: &str, body: &str, app: &App) -> (u16, String) {
    match (method, url) {
        (&Method::GET, "/status") => (200, status_body(app)),
        (&Method::POST, "/play") => {
            let request: PlayRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return (400, error_body(&format!("invalid request body: {err}"))),
            };
            // The sender owns the pipeline during AirPlay; /stop is the
            // local override, /play is refused.
            if airplay_active(app) {
                return (409, error_body("airplay session active"));
            }
            match start_playlist(app, request.playlist_url).await {
                Ok(()) => (200, status_body(app)),
                Err((code, message)) => (code, error_body(&message)),
            }
        }
        (&Method::POST, "/stop") => {
            app.player.send(Command::Stop);
            (200, status_body(app))
        }
        (&Method::POST, "/pause") => {
            if airplay_active(app) {
                return (409, error_body("airplay session active"));
            }
            match current_state(app) {
                State::Playing => {
                    app.player.send(Command::Pause);
                    (200, status_body(app))
                }
                State::Paused => (200, status_body(app)),
                State::Stopped => (409, error_body("nothing is playing")),
            }
        }
        (&Method::POST, "/resume") => {
            if airplay_active(app) {
                return (409, error_body("airplay session active"));
            }
            match current_state(app) {
                State::Paused => {
                    app.player.send(Command::Resume);
                    (200, status_body(app))
                }
                State::Playing => (200, status_body(app)),
                State::Stopped => (409, error_body("nothing is playing")),
            }
        }
        (&Method::POST, "/volume") => {
            let request: VolumeRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return (400, error_body(&format!("invalid request body: {err}"))),
            };
            if request.volume > 100 {
                return (400, error_body("volume must be between 0 and 100"));
            }
            {
                let mut status = app.status.lock().expect("status lock poisoned");
                status.volume = request.volume;
            }
            (200, status_body(app))
        }
        (&Method::POST, "/mute") => {
            set_muted(app, true);
            (200, status_body(app))
        }
        (&Method::POST, "/unmute") => {
            set_muted(app, false);
            (200, status_body(app))
        }
        _ => (404, error_body("not found")),
    }
}

/// Resolve-and-play, shared by the JSON API and the website's play action:
/// same-station no-op/resume, then the blocking playlist fetch off the
/// runtime, then the Play command.
pub async fn start_playlist(app: &App, playlist_url: String) -> Result<(), (u16, String)> {
    match same_station_state(app, &playlist_url) {
        // Already playing this playlist: no-op, don't interrupt.
        Some(State::Playing) => return Ok(()),
        // Paused on this playlist: play means resume it.
        Some(State::Paused) => {
            app.player.send(Command::Resume);
            return Ok(());
        }
        _ => {}
    }
    let resolver = app.resolver.clone();
    let url = playlist_url.clone();
    let stream_url = match tokio::task::spawn_blocking(move || resolver(&url)).await {
        Ok(Ok(url)) => url,
        Ok(Err(err)) => return Err((502, err.to_string())),
        Err(err) => return Err((500, format!("playlist resolver failed: {err}"))),
    };
    app.player.send(Command::Play {
        playlist_url,
        stream_url,
    });
    Ok(())
}

/// The volume value is untouched by muting, so unmute restores it.
fn set_muted(app: &App, muted: bool) {
    let mut status = app.status.lock().expect("status lock poisoned");
    status.muted = muted;
}

/// Returns the current state if the given playlist is the current station.
/// Used to make /play a no-op (playing) or a resume (paused) for the same
/// station instead of re-fetching the playlist and interrupting the stream.
fn same_station_state(app: &App, playlist_url: &str) -> Option<State> {
    let status = app.status.lock().expect("status lock poisoned");
    (status.playlist_url.as_deref() == Some(playlist_url)).then_some(status.state)
}

fn current_state(app: &App) -> State {
    app.status.lock().expect("status lock poisoned").state
}

fn airplay_active(app: &App) -> bool {
    app.status.lock().expect("status lock poisoned").source == AudioSource::Airplay
}

fn status_body(app: &App) -> String {
    let status = app.status.lock().expect("status lock poisoned");
    serde_json::to_string(&*status).expect("status serializes")
}

/// Reads the request body, capped at MAX_BODY_BYTES.
async fn read_body(body: Incoming) -> Result<String, String> {
    let bytes = Limited::new(body, MAX_BODY_BYTES)
        .collect()
        .await
        .map_err(|err| err.to_string())?
        .to_bytes();
    String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
}

fn respond(code: u16, content_type: &str, body: Vec<u8>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(code)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .body(Full::new(Bytes::from(body)))
        .expect("valid response")
}

async fn handle(
    request: Request<Incoming>,
    app: Arc<App>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let hx_request = request.headers().contains_key("hx-request");
    let body = match read_body(request.into_body()).await {
        Ok(body) => body,
        Err(err) => {
            return Ok(respond(
                400,
                "application/json",
                error_body(&format!("invalid request body: {err}")).into_bytes(),
            ));
        }
    };

    // The website first (page, actions, assets), then the JSON API.
    if let Some(reply) =
        crate::web::route(&method, &path, query.as_deref(), &body, hx_request, &app).await
    {
        return Ok(match reply {
            crate::web::Reply::Html(code, html) => {
                respond(code, "text/html; charset=utf-8", html.into_bytes())
            }
            crate::web::Reply::Redirect(location) => Response::builder()
                .status(303)
                .header(hyper::header::LOCATION, location)
                .body(Full::new(Bytes::new()))
                .expect("valid response"),
            crate::web::Reply::Asset(content_type, bytes) => respond(200, content_type, bytes),
            crate::web::Reply::NotFound => respond(
                404,
                "application/json",
                error_body("not found").into_bytes(),
            ),
        });
    }

    let (code, body) = route(&method, &path, &body, &app).await;
    Ok(respond(code, "application/json", body.into_bytes()))
}

/// Accept loop. One task per connection — that is for HTTP keep-alive, not
/// concurrency: one caller (the PHP site), trivial handlers, and the whole
/// control plane shares the single runtime thread.
pub async fn serve(listener: TcpListener, app: Arc<App>) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                eprintln!("radiod: failed to accept connection: {err}");
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service = service_fn(|request| handle(request, app.clone()));
            if let Err(err) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                eprintln!("radiod: connection error: {err}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::player;
    use crate::sink::testing::TestSink;
    use crate::source::SineSource;
    use crate::status::State;
    use std::time::Duration;

    fn test_app(resolver: Resolver) -> App {
        test_app_with_sink(resolver).0
    }

    fn test_app_with_sink(resolver: Resolver) -> (App, TestSink) {
        let status = Arc::new(Mutex::new(Status::initial(&Config::default())));
        let sink = TestSink::default();
        let player = player::spawn(
            status.clone(),
            Box::new(sink.clone()),
            Box::new(|_| Ok(Box::new(SineSource::new()))),
        );
        (
            App {
                status,
                player,
                resolver,
                web: Arc::new(crate::web::Web::with_fetcher(
                    None,
                    crate::web::testing::fixed_channels_fetcher(),
                )),
            },
            sink,
        )
    }

    fn ok_resolver() -> Resolver {
        Arc::new(|_| Ok("https://example.com/stream".to_string()))
    }

    fn wait_for_state(app: &App, state: State) {
        for _ in 0..500 {
            if app.status.lock().unwrap().state == state {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("state not reached within 1s");
    }

    #[tokio::test]
    async fn get_status_returns_status_json() {
        let app = test_app(ok_resolver());
        let (code, body) = route(&Method::GET, "/status", "", &app).await;
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "stopped");
        assert_eq!(value["volume"], 50);
        assert_eq!(value["mixer"], "disabled");
    }

    #[tokio::test]
    async fn play_starts_playback() {
        let app = test_app(ok_resolver());
        let (code, _) = route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Playing);
        let (_, body) = route(&Method::GET, "/status", "", &app).await;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["playlist_url"], "https://example.com/x.pls");
        assert_eq!(value["stream_url"], "https://example.com/stream");
        route(&Method::POST, "/stop", "", &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn stop_stops_playback() {
        let app = test_app(ok_resolver());
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);
        let (code, _) = route(&Method::POST, "/stop", "", &app).await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn playing_the_same_playlist_again_is_a_noop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let resolves = Arc::new(AtomicUsize::new(0));
        let counter = resolves.clone();
        let app = test_app(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("https://example.com/stream".to_string())
        }));

        let body = r#"{"playlist_url": "https://example.com/x.pls"}"#;
        route(&Method::POST, "/play", body, &app).await;
        wait_for_state(&app, State::Playing);
        assert_eq!(resolves.load(Ordering::SeqCst), 1);

        // Same playlist again: still playing, and the playlist was not
        // re-fetched — the request never reached the resolver or player.
        let (code, response) = route(&Method::POST, "/play", body, &app).await;
        assert_eq!(code, 200);
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["state"], "playing");
        assert_eq!(value["playlist_url"], "https://example.com/x.pls");

        // A different playlist still switches.
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/y.pls"}"#,
            &app,
        )
        .await;
        for _ in 0..500 {
            if app.status.lock().unwrap().playlist_url.as_deref()
                == Some("https://example.com/y.pls")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(resolves.load(Ordering::SeqCst), 2);
        route(&Method::POST, "/stop", "", &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn pause_and_resume_round_trip() {
        let app = test_app(ok_resolver());
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);

        let (code, _) = route(&Method::POST, "/pause", "", &app).await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Paused);
        {
            // Paused keeps the station visible — that's the contract.
            let status = app.status.lock().unwrap();
            assert_eq!(
                status.playlist_url.as_deref(),
                Some("https://example.com/x.pls")
            );
            assert_eq!(
                status.stream_url.as_deref(),
                Some("https://example.com/stream")
            );
        }

        // Pause while paused: no-op 200.
        let (code, _) = route(&Method::POST, "/pause", "", &app).await;
        assert_eq!(code, 200);
        assert_eq!(app.status.lock().unwrap().state, State::Paused);

        let (code, _) = route(&Method::POST, "/resume", "", &app).await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Playing);

        // Resume while playing: no-op 200.
        let (code, _) = route(&Method::POST, "/resume", "", &app).await;
        assert_eq!(code, 200);
        assert_eq!(app.status.lock().unwrap().state, State::Playing);

        route(&Method::POST, "/stop", "", &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn pause_and_resume_while_stopped_are_409() {
        let app = test_app(ok_resolver());
        for path in ["/pause", "/resume"] {
            let (code, response) = route(&Method::POST, path, "", &app).await;
            assert_eq!(code, 409, "path {path}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["error"], "nothing is playing");
        }
    }

    #[tokio::test]
    async fn play_same_playlist_while_paused_resumes_without_refetch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let resolves = Arc::new(AtomicUsize::new(0));
        let counter = resolves.clone();
        let app = test_app(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("https://example.com/stream".to_string())
        }));

        let body = r#"{"playlist_url": "https://example.com/x.pls"}"#;
        route(&Method::POST, "/play", body, &app).await;
        wait_for_state(&app, State::Playing);
        route(&Method::POST, "/pause", "", &app).await;
        wait_for_state(&app, State::Paused);

        // /play for the paused station resumes it; the playlist is not
        // fetched again.
        let (code, _) = route(&Method::POST, "/play", body, &app).await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Playing);
        assert_eq!(resolves.load(Ordering::SeqCst), 1);

        route(&Method::POST, "/stop", "", &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn play_different_playlist_while_paused_switches() {
        let app = test_app(ok_resolver());
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);
        route(&Method::POST, "/pause", "", &app).await;
        wait_for_state(&app, State::Paused);

        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/y.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);
        assert_eq!(
            app.status.lock().unwrap().playlist_url.as_deref(),
            Some("https://example.com/y.pls")
        );
    }

    #[tokio::test]
    async fn volume_route_sets_the_volume() {
        let app = test_app(ok_resolver());
        // Volume is a plain 0-100 value; the loudness ceiling lives in the
        // hardware mixer, not in this mapping.
        for requested in [0u8, 30, 50, 80, 100] {
            let body = format!(r#"{{"volume": {requested}}}"#);
            let (code, response) = route(&Method::POST, "/volume", &body, &app).await;
            assert_eq!(code, 200, "requested {requested}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["volume"], requested, "requested {requested}");
        }
        assert_eq!(app.status.lock().unwrap().volume, 100);
    }

    #[tokio::test]
    async fn volume_out_of_range_or_malformed_is_400() {
        let app = test_app(ok_resolver());
        for body in [
            r#"{"volume": 101}"#,
            r#"{"volume": 300}"#,
            r#"{"volume": -1}"#,
            r#"{"volume": "loud"}"#,
            r#"{"volume": 25.5}"#,
            "{}",
            "",
        ] {
            let (code, response) = route(&Method::POST, "/volume", body, &app).await;
            assert_eq!(code, 400, "body {body:?}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert!(value["error"].is_string());
        }
        // Failed requests must not have changed the volume.
        assert_eq!(app.status.lock().unwrap().volume, 50);
    }

    #[tokio::test]
    async fn mute_and_unmute_leave_volume_untouched() {
        let app = test_app(ok_resolver());
        route(&Method::POST, "/volume", r#"{"volume": 40}"#, &app).await;
        let effective = 40; // volume is plain now — no cap scaling

        let (code, response) = route(&Method::POST, "/mute", "", &app).await;
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["muted"], true);
        assert_eq!(value["volume"], effective);

        let (code, response) = route(&Method::POST, "/unmute", "", &app).await;
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["muted"], false);
        assert_eq!(value["volume"], effective);
    }

    #[tokio::test]
    async fn volume_and_mute_take_effect_mid_play() {
        // Skip past samples that may already be in flight in the chunk the
        // player was writing when the route ran.
        const SKIP: usize = 4 * 2048;

        let (app, sink) = test_app_with_sink(ok_resolver());
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);

        let tail_after = |start: usize| {
            for _ in 0..500 {
                if sink.samples.lock().unwrap().len() >= start + 3 * SKIP {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let samples = sink.samples.lock().unwrap();
            assert!(samples.len() >= start + 3 * SKIP, "sink stopped filling");
            samples[start + SKIP..].to_vec()
        };

        // Drop the volume to 10: the tail must scale down to at most 10 %
        // of full scale, but not to silence.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::POST, "/volume", r#"{"volume": 10}"#, &app).await;
        assert_eq!(app.status.lock().unwrap().volume, 10);
        let tail = tail_after(mark);
        let bound = (0.10 * f32::from(i16::MAX)) as i32 + 1;
        let peak = tail.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(peak <= bound, "peak {peak} exceeds bound {bound}");
        assert!(peak > 0, "unexpected silence");

        // Mute: the tail must be pure silence.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::POST, "/mute", "", &app).await;
        let tail = tail_after(mark);
        assert!(tail.iter().all(|s| *s == 0), "muted audio is not silent");

        // Unmute: audio returns at the remembered volume.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::POST, "/unmute", "", &app).await;
        let tail = tail_after(mark);
        let peak = tail.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(peak > 0 && peak <= bound, "peak {peak} after unmute");

        route(&Method::POST, "/stop", "", &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn wrong_method_on_volume_routes_is_404() {
        let app = test_app(ok_resolver());
        for path in ["/volume", "/mute", "/unmute"] {
            let (code, _) = route(&Method::GET, path, "", &app).await;
            assert_eq!(code, 404, "path {path}");
        }
    }

    #[tokio::test]
    async fn play_with_malformed_body_is_400() {
        let app = test_app(ok_resolver());
        for body in ["", "{}", "not json", r#"{"playlist_url": 42}"#] {
            let (code, response) = route(&Method::POST, "/play", body, &app).await;
            assert_eq!(code, 400, "body {body:?}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert!(value["error"].is_string());
        }
        assert_eq!(app.status.lock().unwrap().state, State::Stopped);
    }

    #[tokio::test]
    async fn play_with_failing_playlist_is_502() {
        let app = test_app(Arc::new(|url| {
            Err(PlsError::Fetch(format!("connection refused: {url}")))
        }));
        let (code, response) = route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        assert_eq!(code, 502);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value["error"].as_str().unwrap().contains("cannot fetch"));
        assert_eq!(app.status.lock().unwrap().state, State::Stopped);
    }

    #[tokio::test]
    async fn airplay_session_gates_transport_routes_with_409() {
        let app = test_app(ok_resolver());
        route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);

        // An AirPlay stream takes over the pipeline.
        let (bridge_sink, source) = crate::airplay::bridge(44100, 2);
        app.player.send(Command::AirplayStarted { source });
        for _ in 0..500 {
            if app.status.lock().unwrap().source == crate::status::AudioSource::Airplay {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        for path in ["/pause", "/resume"] {
            let (code, response) = route(&Method::POST, path, "", &app).await;
            assert_eq!(code, 409, "path {path}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["error"], "airplay session active");
        }
        let (code, response) = route(
            &Method::POST,
            "/play",
            r#"{"playlist_url": "https://example.com/y.pls"}"#,
            &app,
        )
        .await;
        assert_eq!(code, 409);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"], "airplay session active");

        // The master volume stays available during AirPlay...
        let (code, _) = route(&Method::POST, "/volume", r#"{"volume": 30}"#, &app).await;
        assert_eq!(code, 200);
        // ...and /stop remains the local override.
        let (code, _) = route(&Method::POST, "/stop", "", &app).await;
        assert_eq!(code, 200);
        wait_for_state(&app, State::Stopped);
        drop(bridge_sink);
    }

    async fn web(
        method: &Method,
        path: &str,
        query: Option<&str>,
        body: &str,
        hx: bool,
        app: &App,
    ) -> crate::web::Reply {
        crate::web::route(method, path, query, body, hx, app)
            .await
            .expect("a web route")
    }

    #[tokio::test]
    async fn website_page_renders_from_live_status_and_channels() {
        let app = test_app(ok_resolver());
        let crate::web::Reply::Html(code, html) =
            web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        assert_eq!(code, 200);
        assert!(html.contains("SOMAFM TUNER"));
        assert!(html.contains("— NO SIGNAL —"));
        assert!(html.contains("STANDBY"));
        // Channels from the injected fetcher, sorted by listeners.
        assert!(html.contains("DEF CON Radio"));
        assert!(html.contains("Groove Salad"));
        // The error query parameter surfaces on the page.
        let crate::web::Reply::Html(_, html) =
            web(&Method::GET, "/", Some("error=boom"), "", false, &app).await
        else {
            panic!("expected html");
        };
        assert!(html.contains("! ERROR: boom"));
    }

    #[tokio::test]
    async fn website_form_actions_follow_post_redirect_get() {
        let app = test_app(ok_resolver());
        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            None,
            "action=volume&volume=30",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert_eq!(location, "/");
        assert_eq!(app.status.lock().unwrap().volume, 30);

        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            None,
            "action=volume&volume=oops",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert!(location.starts_with("/?error="), "location {location}");
        assert_eq!(app.status.lock().unwrap().volume, 30, "unchanged on error");
    }

    #[tokio::test]
    async fn website_htmx_actions_return_the_refreshed_page() {
        let app = test_app(ok_resolver());
        let crate::web::Reply::Html(code, html) =
            web(&Method::POST, "/", None, "action=mute", true, &app).await
        else {
            panic!("expected html");
        };
        assert_eq!(code, 200);
        assert!(html.contains("· MUTED"));
        assert!(app.status.lock().unwrap().muted);
    }

    #[tokio::test]
    async fn website_play_action_resolves_channel_playlists() {
        let app = test_app(ok_resolver());
        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            None,
            "action=play&channel=groovesalad",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert_eq!(location, "/");
        wait_for_state(&app, State::Playing);
        // The highest-quality mp3 playlist from the channel data.
        assert_eq!(
            app.status.lock().unwrap().playlist_url.as_deref(),
            Some("https://api.somafm.com/groovesalad130.pls")
        );
        // The current channel is marked on the page.
        let crate::web::Reply::Html(_, html) = web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        assert!(html.contains("[ON AIR]"));

        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            None,
            "action=play&channel=nope",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert!(location.contains("unknown+channel"), "location {location}");

        web(&Method::POST, "/", None, "action=stop", false, &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn website_shows_artwork_for_the_playing_station_only() {
        let app = test_app(ok_resolver());
        // Standby: the art box renders as an empty frame, no image.
        let crate::web::Reply::Html(_, html) = web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        assert!(html.contains(r#"class="art empty""#));
        assert!(
            html.contains("LoneDJsquare400.jpg"),
            "the lone DJ holds the empty frame"
        );

        web(
            &Method::POST,
            "/",
            None,
            "action=play&channel=groovesalad",
            false,
            &app,
        )
        .await;
        wait_for_state(&app, State::Playing);
        let crate::web::Reply::Html(_, html) = web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        // minijinja entity-escapes slashes in attributes; match on the
        // slash-free part of the URL.
        assert!(html.contains("groovesalad-400.jpg"), "largeimage preferred");
        assert!(
            !html.contains("LoneDJsquare400.jpg"),
            "placeholder replaced"
        );
        // Only in the now-playing section: exactly one image on the page.
        assert_eq!(html.matches("<img").count(), 1);

        web(&Method::POST, "/", None, "action=stop", false, &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn website_channel_sorting_is_server_side_and_sticky() {
        let app = test_app(ok_resolver());
        let page = |html: String| html;
        let order = |html: &str| {
            let defcon = html.find("DEF CON Radio").expect("defcon on page");
            let groove = html.find("Groove Salad").expect("groove on page");
            (defcon, groove)
        };

        // Default: listeners busiest-first, no indicators anywhere.
        let crate::web::Reply::Html(_, html) = web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        let (defcon, groove) = order(&html);
        assert!(defcon < groove, "default is busiest-first");
        assert!(html.contains("STATION</a>"), "no indicator until clicked");
        assert!(!html.contains('▴') && !html.contains('▾'));

        // Genre ascending flips the pair and shows the indicator; the
        // chosen sort is baked into the poll URL and form targets.
        let crate::web::Reply::Html(_, html) = web(
            &Method::GET,
            "/",
            Some("sort=genre&dir=asc"),
            "",
            false,
            &app,
        )
        .await
        else {
            panic!("expected html");
        };
        let html = page(html);
        let (defcon, groove) = order(&html);
        assert!(groove < defcon, "ambient sorts before electronica");
        assert!(html.contains("GENRE ▴"));
        assert!(html.contains("sort=genre"), "sort baked into the page");

        // Listeners ascending: quietest first, v/^ tracks direction.
        let crate::web::Reply::Html(_, html) = web(
            &Method::GET,
            "/",
            Some("sort=listeners&dir=asc"),
            "",
            false,
            &app,
        )
        .await
        else {
            panic!("expected html");
        };
        let (defcon, groove) = order(&html);
        assert!(groove < defcon, "quietest first");
        assert!(html.contains("LSNRS ▴"));

        // Actions keep the chosen sort: HTMX response stays sorted, and
        // the plain-form redirect carries it.
        let crate::web::Reply::Html(_, html) = web(
            &Method::POST,
            "/",
            Some("sort=genre&dir=asc"),
            "action=mute",
            true,
            &app,
        )
        .await
        else {
            panic!("expected html");
        };
        assert!(html.contains("GENRE ▴"));
        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            Some("sort=genre&dir=asc"),
            "action=unmute",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert_eq!(location, "/?sort=genre&dir=asc");

        // Nonsense params mean the default view, not an error.
        let crate::web::Reply::Html(_, html) =
            web(&Method::GET, "/", Some("sort=nope&dir=up"), "", false, &app).await
        else {
            panic!("expected html");
        };
        let (defcon, groove) = order(&html);
        assert!(defcon < groove);
    }

    #[tokio::test]
    async fn website_assets_are_embedded() {
        let app = test_app(ok_resolver());
        for (path, content_type) in [
            ("/style.css", "text/css"),
            ("/htmx.min.js", "text/javascript"),
            ("/manifest.json", "application/manifest+json"),
            ("/icon-180.png", "image/png"),
        ] {
            let crate::web::Reply::Asset(ct, bytes) =
                web(&Method::GET, path, None, "", false, &app).await
            else {
                panic!("expected asset for {path}");
            };
            assert_eq!(ct, content_type, "path {path}");
            assert!(!bytes.is_empty(), "path {path}");
        }
        // Unknown paths fall through to the JSON API's 404.
        assert!(
            crate::web::route(&Method::GET, "/nope.css", None, "", false, &app)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn website_shows_airplay_and_blocks_tuning() {
        let app = test_app(ok_resolver());
        let (_bridge_sink, source) = crate::airplay::bridge(44100, 2);
        app.player.send(Command::AirplayStarted { source });
        for _ in 0..500 {
            if app.status.lock().unwrap().source == crate::status::AudioSource::Airplay {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let crate::web::Reply::Html(_, html) = web(&Method::GET, "/", None, "", false, &app).await
        else {
            panic!("expected html");
        };
        assert!(html.contains("AIRPLAY ACTIVE"));
        assert!(html.contains("— AIRPLAY —"));
        assert!(
            !html.contains("[PLAY]"),
            "tune buttons hidden during airplay"
        );

        let crate::web::Reply::Redirect(location) = web(
            &Method::POST,
            "/",
            None,
            "action=play&channel=defcon",
            false,
            &app,
        )
        .await
        else {
            panic!("expected redirect");
        };
        assert!(
            location.contains("airplay+session+active"),
            "location {location}"
        );

        web(&Method::POST, "/", None, "action=stop", false, &app).await;
        wait_for_state(&app, State::Stopped);
    }

    #[tokio::test]
    async fn unknown_path_is_json_404() {
        let app = test_app(ok_resolver());
        let (code, body) = route(&Method::GET, "/nope", "", &app).await;
        assert_eq!(code, 404);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"], "not found");
    }

    #[tokio::test]
    async fn wrong_method_on_status_is_404() {
        let app = test_app(ok_resolver());
        let (code, _) = route(&Method::POST, "/status", "", &app).await;
        assert_eq!(code, 404);
    }
}
