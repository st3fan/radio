use std::io::Read;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tiny_http::{Header, Method, Response, Server};

use crate::player::{Command, Player};
use crate::pls::PlsError;
use crate::status::{State, Status};
use crate::volume;

/// Maximum accepted request body; our biggest body is one playlist URL.
const MAX_BODY_BYTES: u64 = 64 * 1024;

/// Resolves a playlist URL to a stream URL. Injected so routing tests do not
/// touch the network; production wires in `pls::resolve`.
pub type Resolver = Box<dyn Fn(&str) -> Result<String, PlsError> + Send>;

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
/// Kept free of tiny_http types so routing is unit-testable.
fn route(method: &Method, url: &str, body: &str, app: &App) -> (u16, String) {
    match (method, url) {
        (Method::Get, "/status") => (200, status_body(app)),
        (Method::Post, "/play") => {
            let request: PlayRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return (400, error_body(&format!("invalid request body: {err}"))),
            };
            if already_playing(app, &request.playlist_url) {
                return (200, status_body(app));
            }
            let stream_url = match (app.resolver)(&request.playlist_url) {
                Ok(url) => url,
                Err(err) => return (502, error_body(&err.to_string())),
            };
            app.player.send(Command::Play {
                playlist_url: request.playlist_url,
                stream_url,
            });
            (200, status_body(app))
        }
        (Method::Post, "/stop") => {
            app.player.send(Command::Stop);
            (200, status_body(app))
        }
        (Method::Post, "/volume") => {
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
        (Method::Post, "/mute") => {
            set_muted(app, true);
            (200, status_body(app))
        }
        (Method::Post, "/unmute") => {
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

/// Playing a playlist that is already playing is a no-op — don't interrupt
/// the stream (or re-fetch the playlist) just to start the same station.
fn already_playing(app: &App, playlist_url: &str) -> bool {
    let status = app.status.lock().expect("status lock poisoned");
    status.state == State::Playing && status.playlist_url.as_deref() == Some(playlist_url)
}

fn status_body(app: &App) -> String {
    let status = app.status.lock().expect("status lock poisoned");
    serde_json::to_string(&*status).expect("status serializes")
}

/// Blocking accept loop; requests are handled sequentially. One caller (the
/// PHP site) and trivial handlers — no thread pool until profiling says
/// otherwise.
pub fn serve(server: &Server, app: &App) {
    let content_type: Header = "Content-Type: application/json"
        .parse()
        .expect("valid header");
    for mut request in server.incoming_requests() {
        let mut body = String::new();
        if let Err(err) = request
            .as_reader()
            .take(MAX_BODY_BYTES)
            .read_to_string(&mut body)
        {
            eprintln!("radiod: failed to read request body: {err}");
            continue;
        }
        let (code, response_body) = route(request.method(), request.url(), &body, app);
        let response = Response::from_string(response_body)
            .with_status_code(code)
            .with_header(content_type.clone());
        if let Err(err) = request.respond(response) {
            eprintln!("radiod: failed to send response: {err}");
        }
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
        Box::new(|_| Ok("https://example.com/stream".to_string()))
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

    #[test]
    fn get_status_returns_status_json() {
        let app = test_app(ok_resolver());
        let (code, body) = route(&Method::Get, "/status", "", &app);
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "stopped");
        assert_eq!(value["volume"], 25);
        assert_eq!(value["max_volume"], 50);
    }

    #[test]
    fn play_starts_playback() {
        let app = test_app(ok_resolver());
        let (code, _) = route(
            &Method::Post,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        );
        assert_eq!(code, 200);
        wait_for_state(&app, State::Playing);
        let (_, body) = route(&Method::Get, "/status", "", &app);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["playlist_url"], "https://example.com/x.pls");
        assert_eq!(value["stream_url"], "https://example.com/stream");
        route(&Method::Post, "/stop", "", &app);
        wait_for_state(&app, State::Stopped);
    }

    #[test]
    fn stop_stops_playback() {
        let app = test_app(ok_resolver());
        route(
            &Method::Post,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        );
        wait_for_state(&app, State::Playing);
        let (code, _) = route(&Method::Post, "/stop", "", &app);
        assert_eq!(code, 200);
        wait_for_state(&app, State::Stopped);
    }

    #[test]
    fn playing_the_same_playlist_again_is_a_noop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let resolves = Arc::new(AtomicUsize::new(0));
        let counter = resolves.clone();
        let app = test_app(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("https://example.com/stream".to_string())
        }));

        let body = r#"{"playlist_url": "https://example.com/x.pls"}"#;
        route(&Method::Post, "/play", body, &app);
        wait_for_state(&app, State::Playing);
        assert_eq!(resolves.load(Ordering::SeqCst), 1);

        // Same playlist again: still playing, and the playlist was not
        // re-fetched — the request never reached the resolver or player.
        let (code, response) = route(&Method::Post, "/play", body, &app);
        assert_eq!(code, 200);
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["state"], "playing");
        assert_eq!(value["playlist_url"], "https://example.com/x.pls");

        // A different playlist still switches.
        route(
            &Method::Post,
            "/play",
            r#"{"playlist_url": "https://example.com/y.pls"}"#,
            &app,
        );
        for _ in 0..500 {
            if app.status.lock().unwrap().playlist_url.as_deref()
                == Some("https://example.com/y.pls")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(resolves.load(Ordering::SeqCst), 2);
        route(&Method::Post, "/stop", "", &app);
        wait_for_state(&app, State::Stopped);
    }

    #[test]
    fn volume_route_scales_percentage_of_max_volume() {
        let app = test_app(ok_resolver());
        // Default max_volume is 50; requests are percentages of it.
        for (requested, effective) in [(0u8, 0u64), (30, 15), (50, 25), (80, 40), (100, 50)] {
            let body = format!(r#"{{"volume": {requested}}}"#);
            let (code, response) = route(&Method::Post, "/volume", &body, &app);
            assert_eq!(code, 200, "requested {requested}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["volume"], effective, "requested {requested}");
        }
        assert_eq!(app.status.lock().unwrap().volume, 50);
    }

    #[test]
    fn volume_out_of_range_or_malformed_is_400() {
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
            let (code, response) = route(&Method::Post, "/volume", body, &app);
            assert_eq!(code, 400, "body {body:?}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert!(value["error"].is_string());
        }
        // Failed requests must not have changed the volume.
        assert_eq!(app.status.lock().unwrap().volume, 25);
    }

    #[test]
    fn mute_and_unmute_leave_volume_untouched() {
        let app = test_app(ok_resolver());
        route(&Method::Post, "/volume", r#"{"volume": 40}"#, &app);
        let effective = 20; // 40% of the default max_volume 50

        let (code, response) = route(&Method::Post, "/mute", "", &app);
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["muted"], true);
        assert_eq!(value["volume"], effective);

        let (code, response) = route(&Method::Post, "/unmute", "", &app);
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["muted"], false);
        assert_eq!(value["volume"], effective);
    }

    #[test]
    fn volume_and_mute_take_effect_mid_play() {
        // Skip past samples that may already be in flight in the chunk the
        // player was writing when the route ran.
        const SKIP: usize = 4 * 2048;

        let (app, sink) = test_app_with_sink(ok_resolver());
        route(
            &Method::Post,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        );
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

        // Drop the volume to 10% of the cap (an effective device volume of
        // 5): the tail must scale down, but not to silence.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::Post, "/volume", r#"{"volume": 10}"#, &app);
        assert_eq!(app.status.lock().unwrap().volume, 5);
        let tail = tail_after(mark);
        let bound = (0.05 * f32::from(i16::MAX)) as i32 + 1;
        let peak = tail.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(peak <= bound, "peak {peak} exceeds bound {bound}");
        assert!(peak > 0, "unexpected silence");

        // Mute: the tail must be pure silence.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::Post, "/mute", "", &app);
        let tail = tail_after(mark);
        assert!(tail.iter().all(|s| *s == 0), "muted audio is not silent");

        // Unmute: audio returns at the remembered volume.
        let mark = sink.samples.lock().unwrap().len();
        route(&Method::Post, "/unmute", "", &app);
        let tail = tail_after(mark);
        let peak = tail.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(peak > 0 && peak <= bound, "peak {peak} after unmute");

        route(&Method::Post, "/stop", "", &app);
        wait_for_state(&app, State::Stopped);
    }

    #[test]
    fn wrong_method_on_volume_routes_is_404() {
        let app = test_app(ok_resolver());
        for path in ["/volume", "/mute", "/unmute"] {
            let (code, _) = route(&Method::Get, path, "", &app);
            assert_eq!(code, 404, "path {path}");
        }
    }

    #[test]
    fn play_with_malformed_body_is_400() {
        let app = test_app(ok_resolver());
        for body in ["", "{}", "not json", r#"{"playlist_url": 42}"#] {
            let (code, response) = route(&Method::Post, "/play", body, &app);
            assert_eq!(code, 400, "body {body:?}");
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert!(value["error"].is_string());
        }
        assert_eq!(app.status.lock().unwrap().state, State::Stopped);
    }

    #[test]
    fn play_with_failing_playlist_is_502() {
        let app = test_app(Box::new(|url| {
            Err(PlsError::Fetch(format!("connection refused: {url}")))
        }));
        let (code, response) = route(
            &Method::Post,
            "/play",
            r#"{"playlist_url": "https://example.com/x.pls"}"#,
            &app,
        );
        assert_eq!(code, 502);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value["error"].as_str().unwrap().contains("cannot fetch"));
        assert_eq!(app.status.lock().unwrap().state, State::Stopped);
    }

    #[test]
    fn unknown_path_is_json_404() {
        let app = test_app(ok_resolver());
        let (code, body) = route(&Method::Get, "/nope", "", &app);
        assert_eq!(code, 404);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"], "not found");
    }

    #[test]
    fn wrong_method_on_status_is_404() {
        let app = test_app(ok_resolver());
        let (code, _) = route(&Method::Post, "/status", "", &app);
        assert_eq!(code, 404);
    }
}
