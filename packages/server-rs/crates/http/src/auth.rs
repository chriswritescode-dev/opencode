use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use ring::hmac;
use subtle::ConstantTimeEq;
use url::{Url, form_urlencoded};

const AUTH_TOKEN_QUERY: &str = "auth_token";
const REALM: &str = "Basic realm=\"Secure Area\"";
const SESSION_KEY_CONTEXT: &str = "opencode-session-v1";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub const SESSION_TTL_SECONDS: u64 = 2_592_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub username: String,
    pub password: String,
}

pub fn credential(request: &Request<Bytes>) -> Credential {
    let token = form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key.as_ref() == AUTH_TOKEN_QUERY)
        .filter(|(_, value)| !value.is_empty());
    if let Some((_, value)) = token {
        return decode(&value);
    }
    let encoded =
        request.headers().get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(basic_token);
    if let Some(encoded) = encoded {
        return decode(encoded);
    }
    empty()
}

pub fn authorized(credential: &Credential, password: &str) -> bool {
    credential.username == "opencode"
        && (bool::from(credential.password.as_bytes().ct_eq(password.as_bytes()))
            || verify_session(&credential.password, password, crate::now_ms()))
}

pub fn issue_session(password: &str, now_ms: u64) -> String {
    let expires = now_ms / 1000 + SESSION_TTL_SECONDS;
    let payload = expires.to_string();
    format!("{payload}.{}", sign(password, &payload))
}

pub fn verify_session(token: &str, password: &str, now_ms: u64) -> bool {
    let mut parts = token.split('.');
    let Some(expiry) = parts.next() else {
        return false;
    };
    let Some(signature) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    if expiry.is_empty() || !expiry.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(expires) = expiry.parse::<u64>() else {
        return false;
    };
    if expires > MAX_SAFE_INTEGER {
        return false;
    }
    if expires * 1000 <= now_ms {
        return false;
    }
    let expected = sign(password, expiry);
    if signature.len() != expected.len() {
        return false;
    }
    bool::from(signature.as_bytes().ct_eq(expected.as_bytes()))
}

pub fn session_cookie_name(host: Option<&str>) -> String {
    let port = Url::parse(&format!("http://{}", host.unwrap_or_default())).ok().and_then(|url| url.port());
    match port {
        Some(port) => format!("opencode_session_{port}"),
        None => "opencode_session".to_owned(),
    }
}

pub fn authorized_cookie(request: &Request<Bytes>, password: &str) -> bool {
    let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
    let Some(token) = cookie_value(request, &session_cookie_name(host)) else {
        return false;
    };
    if token.is_empty() {
        return false;
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let Some(host) = host else {
            return false;
        };
        let Some(origin) = origin.to_str().ok().and_then(|value| Url::parse(value).ok()) else {
            return false;
        };
        if url_host(&origin) != host {
            return false;
        }
    }
    verify_session(token, password, crate::now_ms())
}

pub fn challenge(request: &Request<Bytes>) -> bool {
    match request.headers().get("sec-fetch-mode") {
        Some(value) => value.to_str().ok() == Some("navigate"),
        None => true,
    }
}

pub fn unauthorized(request: &Request<Bytes>, message: &str) -> crate::response::Response {
    let body = serde_json::json!({ "_tag": "UnauthorizedError", "message": message });
    let mut response = crate::response::json(StatusCode::UNAUTHORIZED, &body);
    if challenge(request) {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(REALM));
    }
    response
}

fn sign(password: &str, payload: &str) -> String {
    let password_key = hmac::Key::new(hmac::HMAC_SHA256, password.as_bytes());
    let derived = hmac::sign(&password_key, SESSION_KEY_CONTEXT.as_bytes());
    let session_key = hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref());
    URL_SAFE_NO_PAD.encode(hmac::sign(&session_key, payload.as_bytes()).as_ref())
}

fn url_host(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

fn cookie_value<'a>(request: &'a Request<Bytes>, name: &str) -> Option<&'a str> {
    let header = request.headers().get(header::COOKIE)?.to_str().ok()?;
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

fn basic_token(value: &str) -> Option<&str> {
    let scheme = value.get(..5)?;
    if !scheme.eq_ignore_ascii_case("Basic") {
        return None;
    }
    let remainder = value.get(5..)?;
    if !remainder.starts_with(char::is_whitespace) {
        return None;
    }
    let remainder = remainder.trim_start();
    if remainder.is_empty() {
        return None;
    }
    Some(remainder)
}

fn decode(input: &str) -> Credential {
    let cleaned: String = input.chars().filter(|character| *character != '\r' && *character != '\n').collect();
    let Ok(bytes) = STANDARD.decode(cleaned) else {
        return empty();
    };
    let text = String::from_utf8_lossy(&bytes);
    let Some((username, password)) = text.split_once(':') else {
        return empty();
    };
    Credential { username: username.to_owned(), password: password.to_owned() }
}

fn empty() -> Credential {
    Credential { username: String::new(), password: String::new() }
}
