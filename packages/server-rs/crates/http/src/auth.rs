use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use subtle::ConstantTimeEq;
use url::form_urlencoded;

const AUTH_TOKEN_QUERY: &str = "auth_token";
const REALM: &str = "Basic realm=\"Secure Area\"";

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
            || verify_session(&credential.password, password))
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

pub fn verify_session(_token: &str, _password: &str) -> bool {
    false
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
