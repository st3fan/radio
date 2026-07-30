use std::sync::Mutex;

use tiny_http::{Header, Method, Response, Server};

use crate::status::Status;

/// The outcome of routing a request: an HTTP status code and a JSON body.
/// Kept free of tiny_http types so routing is unit-testable.
fn route(method: &Method, url: &str, status: &Mutex<Status>) -> (u16, String) {
    match (method, url) {
        (Method::Get, "/status") => {
            let status = status.lock().expect("status lock poisoned");
            let body = serde_json::to_string(&*status).expect("status serializes");
            (200, body)
        }
        _ => (404, r#"{"error":"not found"}"#.to_string()),
    }
}

/// Blocking accept loop; requests are handled sequentially. One caller (the
/// PHP site) and trivial handlers — no thread pool until profiling says
/// otherwise.
pub fn serve(server: &Server, status: &Mutex<Status>) {
    let content_type: Header = "Content-Type: application/json"
        .parse()
        .expect("valid header");
    for request in server.incoming_requests() {
        let (code, body) = route(request.method(), request.url(), status);
        let response = Response::from_string(body)
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

    fn stub_status() -> Mutex<Status> {
        Mutex::new(Status::initial(&Config::default()))
    }

    #[test]
    fn get_status_returns_status_json() {
        let status = stub_status();
        let (code, body) = route(&Method::Get, "/status", &status);
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "stopped");
        assert_eq!(value["volume"], 25);
        assert_eq!(value["max_volume"], 50);
    }

    #[test]
    fn unknown_path_is_json_404() {
        let status = stub_status();
        let (code, body) = route(&Method::Get, "/nope", &status);
        assert_eq!(code, 404);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"], "not found");
    }

    #[test]
    fn wrong_method_on_status_is_404() {
        let status = stub_status();
        let (code, _) = route(&Method::Post, "/status", &status);
        assert_eq!(code, 404);
    }
}
