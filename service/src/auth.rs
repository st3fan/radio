//! The optional web-UI password gate.
//!
//! When `[web] password` is set in the config, every route except the
//! login flow and the static assets requires a session cookie. The cookie
//! is a stateless HMAC-SHA256 bearer token keyed by the password (never
//! the password itself), so the daemon holds no session state: it is
//! restart-proof, any number of browsers can be logged in at once, and
//! changing the password invalidates every outstanding cookie for free.
//!
//! This is casual gating of LAN traffic over plain HTTP, not transport
//! security — the password and cookie cross the wire in the clear, which
//! is the accepted threat model.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const COOKIE_NAME: &str = "radiod";
/// The fixed message the password signs. Versioned so a future change to
/// the scheme also changes the token (and thus invalidates old cookies).
const TOKEN_MESSAGE: &[u8] = b"radiod-web-session-v1";
const MAX_AGE_SECS: u64 = 31536000; // one year

pub struct Auth {
    /// The token a correct password earns; `None` while the feature is off.
    token: Option<String>,
}

impl Auth {
    pub fn new(password: Option<String>) -> Auth {
        Auth {
            token: password.map(|password| token_for(&password)),
        }
    }

    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    /// The `Set-Cookie` value for a successful login.
    pub fn login_cookie(&self) -> String {
        cookie_value(self.token.as_deref().unwrap_or_default())
    }

    /// The `Set-Cookie` value that forgets the session (the lock button).
    pub fn logout_cookie(&self) -> String {
        format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
    }

    /// Whether the request's `Cookie` header carries a valid token.
    pub fn check(&self, cookie_header: Option<&str>) -> bool {
        match (self.token.as_deref(), cookie_header) {
            (Some(token), Some(header)) => find_cookie(header, COOKIE_NAME)
                .map(|candidate| constant_time_eq(candidate, token))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Whether a submitted plaintext password is the configured one.
    pub fn verify(&self, password: &str) -> bool {
        match &self.token {
            Some(token) => constant_time_eq(&token_for(password), token),
            None => false,
        }
    }
}

/// Paths the gate always lets through: the login flow and the static
/// assets the login page itself needs to render.
pub fn open_path(path: &str) -> bool {
    matches!(
        path,
        "/login"
            | "/logout"
            | "/style.css"
            | "/theme.js"
            | "/htmx.min.js"
            | "/manifest.json"
            | "/icon-180.png"
            | "/icon-192.png"
            | "/icon-512.png"
            | "/icon-maskable-512.png"
    )
}

fn token_for(password: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC accepts any key length");
    mac.update(TOKEN_MESSAGE);
    hex(&mac.finalize().into_bytes())
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn cookie_value(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={MAX_AGE_SECS}")
}

/// The `radiod` value out of a `Cookie` header, if present.
fn find_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').map(str::trim).find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Constant-time string comparison: no early exit on the first difference,
/// so a timing read cannot leak the prefix of the token.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(pw: Option<&str>) -> Auth {
        Auth::new(pw.map(str::to_string))
    }

    #[test]
    fn disabled_when_no_password() {
        assert!(!auth(None).enabled());
        assert!(!auth(None).check(Some("radiod=x")));
        assert!(!auth(None).verify("hunter2"));
    }

    #[test]
    fn token_is_stable_and_hex_encoded() {
        let a = auth(Some("hunter2"));
        let b = auth(Some("hunter2"));
        let token = a.token.as_deref().unwrap();
        assert_eq!(token, b.token.as_deref().unwrap());
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn cookie_value_carries_the_token() {
        let a = auth(Some("hunter2"));
        let value = a.login_cookie();
        assert!(value.starts_with("radiod="));
        assert!(value.contains("Path=/"));
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Lax"));
        assert!(value.contains("Max-Age=31536000"));
        assert!(!value.contains("Secure"), "we serve HTTP, not HTTPS");
    }

    #[test]
    fn logout_cookie_clears_the_name() {
        assert_eq!(
            auth(Some("hunter2")).logout_cookie(),
            "radiod=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
        );
    }

    #[test]
    fn check_accepts_the_valid_cookie_only() {
        let a = auth(Some("hunter2"));
        let token = a.token.clone().unwrap();
        let header = format!("theme=amber; radiod={token}; jh=1");

        assert!(a.check(Some(&header)));
        assert!(!a.check(Some("radiod=deadbeef")));
        assert!(!a.check(Some("other=cookie")));
        assert!(!a.check(None));
        // A different password never produces this token.
        assert!(!auth(Some("wrong")).check(Some(&header)));
    }

    #[test]
    fn verify_matches_the_configured_password() {
        let a = auth(Some("hunter2"));
        assert!(a.verify("hunter2"));
        assert!(!a.verify("hunter3"));
        assert!(!a.verify(""));
    }

    #[test]
    fn open_path_lists_the_login_and_assets_only() {
        for path in [
            "/login",
            "/logout",
            "/style.css",
            "/theme.js",
            "/htmx.min.js",
        ] {
            assert!(open_path(path), "{path} should be open");
        }
        for path in ["/", "/status", "/debug", "/play", "/volume", "/favicon.ico"] {
            assert!(!open_path(path), "{path} should be gated");
        }
    }

    #[test]
    fn constant_time_eq_compares_exactly() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
    }
}
