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
use crate::status::{State, Status};
use crate::volume;

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
            match same_station_state(app, &request.playlist_url) {
                // Already playing this playlist: no-op, don't interrupt.
                Some(State::Playing) => return (200, status_body(app)),
                // Paused on this playlist: /play means resume it.
                Some(State::Paused) => {
                    app.player.send(Command::Resume);
                    return (200, status_body(app));
                }
                _ => {}
            }
            // The resolver does blocking network I/O; keep it off the
            // single-threaded runtime.
            let resolver = app.resolver.clone();
            let playlist_url = request.playlist_url.clone();
            let stream_url =
                match tokio::task::spawn_blocking(move || resolver(&playlist_url)).await {
                    Ok(Ok(url)) => url,
                    Ok(Err(err)) => return (502, error_body(&err.to_string())),
                    Err(err) => {
                        return (500, error_body(&format!("playlist resolver failed: {err}")));
                    }
                };
            app.player.send(Command::Play {
                playlist_url: request.playlist_url,
                stream_url,
            });
            (200, status_body(app))
        }
        (&Method::POST, "/stop") => {
            app.player.send(Command::Stop);
            (200, status_body(app))
        }
        (&Method::POST, "/pause") => match current_state(app) {
            State::Playing => {
                app.player.send(Command::Pause);
                (200, status_body(app))
            }
            State::Paused => (200, status_body(app)),
            State::Stopped => (409, error_body("nothing is playing")),
        },
        (&Method::POST, "/resume") => match current_state(app) {
            State::Paused => {
                app.player.send(Command::Resume);
                (200, status_body(app))
            }
            State::Playing => (200, status_body(app)),
            State::Stopped => (409, error_body("nothing is playing")),
        },
        (&Method::POST, "/volume") => {
            let request: VolumeRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return (400, error_body(&format!("invalid request body: {err}"))),
            };
            // The request is a percentage of max_volume: 100 means "as loud
            // as the cap allows". The stored/returned value is the effective
            // device volume.
            if request.volume > 100 {
                return (400, error_body("volume must be between 0 and 100"));
            }
            {
                let mut status = app.status.lock().expect("status lock poisoned");
                status.volume = volume::effective_volume(request.volume, status.max_volume);
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

async fn handle(
    request: Request<Incoming>,
    app: Arc<App>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let (code, body) = match read_body(request.into_body()).await {
        Ok(body) => route(&method, &path, &body, &app).await,
        Err(err) => (400, error_body(&format!("invalid request body: {err}"))),
    };
    let response = Response::builder()
        .status(code)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("valid response");
    Ok(response)
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
