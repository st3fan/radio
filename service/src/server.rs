use std::io::Read;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tiny_http::{Header, Method, Response, Server};

use crate::player::{Command, Player};
use crate::pls::PlsError;
use crate::status::Status;

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
        _ => (404, error_body("not found")),
    }
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
        let status = Arc::new(Mutex::new(Status::initial(&Config::default())));
        let player = player::spawn(
            status.clone(),
            Box::new(TestSink::default()),
            Box::new(|_| Ok(Box::new(SineSource::new(false)))),
        );
        App {
            status,
            player,
            resolver,
        }
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
