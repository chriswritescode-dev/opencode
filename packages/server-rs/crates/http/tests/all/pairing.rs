use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use http::StatusCode;
use opencode_http::app::App;
use opencode_http::auth::SESSION_TTL_SECONDS;
use opencode_http::pairing::{CODE_TTL_SECONDS, Pairing};
use opencode_http::response::Response;

use crate::{UNAUTHORIZED_BODY, authorized, basic, encoded, ready_app, request, request_with, text};

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

async fn issue_code(app: &App) -> String {
    let response = app.handle(authorized("POST", "/api/pair")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let issued: serde_json::Value = serde_json::from_str(&text(response).await).expect("pairing json");
    issued["code"].as_str().expect("a code").to_owned()
}

fn clock(millis: Arc<AtomicU64>) -> opencode_http::Clock {
    Arc::new(move || millis.load(Ordering::SeqCst))
}

#[test]
fn issue_returns_a_twenty_two_character_url_safe_code() {
    let issued = Pairing::new().issue();
    assert_eq!(issued.code.len(), 22);
    assert!(issued.code.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')));
    assert_eq!(issued.expires_in, 300.0);
}

#[test]
fn consume_is_single_use() {
    let pairing = Pairing::new();
    let issued = pairing.issue();
    assert!(pairing.consume(&issued.code));
    assert!(!pairing.consume(&issued.code));
    assert!(!pairing.consume("unknown"));
}

#[test]
fn an_expired_code_is_rejected() {
    let now = Arc::new(AtomicU64::new(1_000));
    let pairing = Pairing::with_clock(clock(now.clone()));
    let issued = pairing.issue();
    now.store(1_000 + CODE_TTL_SECONDS * 1_000, Ordering::SeqCst);
    assert!(!pairing.consume(&issued.code));
}

#[test]
fn inserting_at_capacity_evicts_the_oldest_code() {
    let pairing = Pairing::new();
    let first = pairing.issue();
    let mut newest = first.code.clone();
    for _ in 0..999 {
        newest = pairing.issue().code;
    }
    let overflow = pairing.issue();
    assert!(pairing.consume(&overflow.code));
    assert!(!pairing.consume(&first.code));
    assert!(pairing.consume(&newest));
}

#[tokio::test]
async fn pair_returns_a_code_for_authorized_requests() {
    let app = ready_app();
    let response = app.handle(authorized("POST", "/api/pair")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    let issued: serde_json::Value = serde_json::from_str(&text(response).await).expect("pairing json");
    let code = issued["code"].as_str().expect("a code");
    assert_eq!(code.len(), 22);
    assert!(code.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')));
    assert_eq!(issued["expires_in"], 300);

    let unauthorized = app.handle(request("POST", "/api/pair")).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(unauthorized).await, UNAUTHORIZED_BODY);
}

#[tokio::test]
async fn api_clients_exchange_a_code_for_a_session_token() {
    let app = ready_app();
    let before = opencode_http::now_ms() / 1_000 + SESSION_TTL_SECONDS;
    let code = issue_code(&app).await;
    let response = app.handle(request("GET", &format!("/auth/connect/{code}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    let session: serde_json::Value = serde_json::from_str(&text(response).await).expect("session json");
    let token = session["token"].as_str().expect("a token").to_owned();
    let after = opencode_http::now_ms() / 1_000 + SESSION_TTL_SECONDS;

    let expiry: u64 = token.split_once('.').expect("a session token").0.parse().expect("an expiry");
    assert!(expiry == before || expiry == after);

    let credential = basic("opencode", &token);
    let basic_response = app.handle(request_with("GET", "/api/info", &[("authorization", credential.as_str())])).await;
    assert_eq!(basic_response.status(), StatusCode::OK);

    let query = encoded("opencode", &token);
    let query_response = app.handle(request("GET", &format!("/api/info?auth_token={query}"))).await;
    assert_eq!(query_response.status(), StatusCode::OK);

    let cookie = format!("opencode_session={token}");
    let cookie_response =
        app.handle(request_with("GET", "/api/info", &[("host", "localhost"), ("cookie", cookie.as_str())])).await;
    assert_eq!(cookie_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn codes_are_single_use() {
    let app = ready_app();
    let code = issue_code(&app).await;
    let first = app.handle(request("GET", &format!("/auth/connect/{code}"))).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = app.handle(request("GET", &format!("/auth/connect/{code}"))).await;
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(second).await, r#"{"_tag":"UnauthorizedError","message":"Pairing link expired or already used"}"#);
}

#[tokio::test]
async fn browsers_get_a_session_cookie_and_a_redirect() {
    let app = ready_app();
    let code = issue_code(&app).await;
    let response = app
        .handle(request_with(
            "GET",
            &format!("/auth/connect/{code}"),
            &[("accept", "text/html"), ("host", "127.0.0.1:4096")],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(header(&response, "location"), Some("/"));
    let set_cookie = header(&response, "set-cookie").expect("a session cookie").to_owned();
    let token = set_cookie
        .strip_prefix("opencode_session_4096=")
        .and_then(|rest| rest.split(';').next())
        .expect("a session token");
    assert_eq!(set_cookie, format!("opencode_session_4096={token}; Max-Age=2592000; Path=/; HttpOnly; SameSite=Lax"));
    assert_eq!(text(response).await, "");

    let cookie = set_cookie.split(';').next().expect("a cookie pair");
    let info = app.handle(request_with("GET", "/api/info", &[("host", "127.0.0.1:4096"), ("cookie", cookie)])).await;
    assert_eq!(info.status(), StatusCode::OK);
}

#[tokio::test]
async fn pairing_failures_match_the_client_type() {
    let app = ready_app();
    let browser = app.handle(request_with("GET", "/auth/connect/unknown", &[("accept", "text/html")])).await;
    assert_eq!(browser.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(header(&browser, "content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(
        text(browser).await,
        "This pairing link expired or was already used. Run `opencode pair` to get a new one."
    );

    let api = app.handle(request("GET", "/auth/connect/unknown")).await;
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
    assert!(api.headers().contains_key("www-authenticate"));
    assert_eq!(text(api).await, r#"{"_tag":"UnauthorizedError","message":"Pairing link expired or already used"}"#);

    let cors = app.handle(request_with("GET", "/auth/connect/unknown", &[("sec-fetch-mode", "cors")])).await;
    assert_eq!(cors.status(), StatusCode::UNAUTHORIZED);
    assert!(!cors.headers().contains_key("www-authenticate"));
}
