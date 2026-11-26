use http::StatusCode;
use opencode_http::auth::{SESSION_TTL_SECONDS, authorized_cookie, issue_session, session_cookie_name, verify_session};

use crate::{UNAUTHORIZED_BODY, app, authorized, basic, encoded, ready_app, request, request_with, text};

const UNEXPIRED_NOW_MS: u64 = 4_000_000_000_000;

#[tokio::test]
async fn basic_credentials_authorize() {
    let app = ready_app();
    let response = app.handle(authorized("GET", "/openapi.json")).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn lowercase_scheme_authorizes() {
    let app = ready_app();
    let credential = format!("basic {}", encoded("opencode", "pw"));
    let http_request = request_with("GET", "/openapi.json", &[("authorization", credential.as_str())]);
    let response = app.handle(http_request).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_token_query_authorizes() {
    let app = ready_app();
    let credential = encoded("opencode", "pw");
    let response = app.handle(request("GET", &format!("/openapi.json?auth_token={credential}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn empty_auth_token_falls_back_to_the_header() {
    let app = ready_app();
    let credential = basic("opencode", "pw");
    let http_request = request_with("GET", "/openapi.json?auth_token=", &[("authorization", credential.as_str())]);
    let response = app.handle(http_request).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_credentials_are_unauthorized() {
    let app = ready_app();
    for credential in [basic("user", "pw"), basic("opencode", "no")] {
        let http_request = request_with("GET", "/openapi.json", &[("authorization", credential.as_str())]);
        let response = app.handle(http_request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn malformed_credentials_are_unauthorized() {
    let app = ready_app();
    for credential in ["Basic !!!", "Basic b3BlbmNvZGU="] {
        let http_request = request_with("GET", "/openapi.json", &[("authorization", credential)]);
        let response = app.handle(http_request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn missing_credentials_are_unauthorized() {
    let app = ready_app();
    let response = app.handle(request("GET", "/openapi.json")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    assert_eq!(
        response.headers().get("www-authenticate").and_then(|value| value.to_str().ok()),
        Some("Basic realm=\"Secure Area\"")
    );
    assert_eq!(text(response).await, UNAUTHORIZED_BODY);
}

#[tokio::test]
async fn challenge_follows_fetch_mode() {
    let app = ready_app();
    let absent = app.handle(request("GET", "/openapi.json")).await;
    assert!(absent.headers().contains_key("www-authenticate"));
    let navigate = app.handle(request_with("GET", "/openapi.json", &[("sec-fetch-mode", "navigate")])).await;
    assert!(navigate.headers().contains_key("www-authenticate"));
    let cors = app.handle(request_with("GET", "/openapi.json", &[("sec-fetch-mode", "cors")])).await;
    assert!(!cors.headers().contains_key("www-authenticate"));
}

#[tokio::test]
async fn ticketed_connects_skip_authentication_when_ready() {
    let app = ready_app();
    let pty = app.handle(request("GET", "/api/pty/an-id/connect?ticket=t")).await;
    assert_eq!(pty.status(), StatusCode::BAD_REQUEST);
    let persistent = app.handle(request("GET", "/api/experimental/persistent-pty/pty_unknown/connect?ticket=t")).await;
    assert_eq!(persistent.status(), StatusCode::FORBIDDEN);
    let mcp = app.handle(request("GET", "/api/experimental/client-mcp?ticket=t")).await;
    assert_eq!(mcp.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn connect_without_a_ticket_requires_authentication() {
    let app = ready_app();
    let response = app.handle(request("GET", "/api/pty/an-id/connect")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(response).await, UNAUTHORIZED_BODY);
}

#[tokio::test]
async fn tickets_do_not_skip_authentication_while_starting() {
    let app = app();
    let response = app.handle(request("GET", "/api/pty/an-id/connect?ticket=t")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(response).await, UNAUTHORIZED_BODY);
}

#[tokio::test]
async fn pairing_links_skip_authentication() {
    let app = ready_app();
    let connect = app.handle(request("GET", "/auth/connect/code")).await;
    assert_eq!(connect.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(connect).await, r#"{"_tag":"UnauthorizedError","message":"Pairing link expired or already used"}"#);
    let incomplete = app.handle(request("GET", "/auth/connect/")).await;
    assert_eq!(incomplete.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(incomplete).await, UNAUTHORIZED_BODY);
}

#[test]
fn session_tokens_round_trip() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    assert!(verify_session(&token, "pw", issued));
    assert!(token.starts_with(&format!("{}.", issued / 1_000 + SESSION_TTL_SECONDS)));
}

#[test]
fn session_tokens_stop_verifying_after_the_ttl() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    let expires = (issued / 1_000 + SESSION_TTL_SECONDS) * 1_000;
    assert!(verify_session(&token, "pw", expires - 1));
    assert!(!verify_session(&token, "pw", expires));
    assert!(!verify_session(&token, "pw", expires + 1_000));
}

#[test]
fn tampered_signatures_are_rejected() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    let (expiry, signature) = token.split_once('.').expect("a session token");
    let mut tampered = signature.to_owned();
    let first = tampered.remove(0);
    tampered.insert(0, if first == 'A' { 'B' } else { 'A' });
    assert!(!verify_session(&format!("{expiry}.{tampered}"), "pw", issued));
}

#[test]
fn tokens_with_three_parts_are_rejected() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    assert!(!verify_session(&format!("{token}.extra"), "pw", issued));
}

#[test]
fn non_numeric_expiries_are_rejected() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    let signature = token.split_once('.').expect("a session token").1;
    for expiry in ["", "abc", "1.5", "-1", " 1"] {
        assert!(!verify_session(&format!("{expiry}.{signature}"), "pw", issued));
    }
}

#[test]
fn a_different_password_with_the_same_expiry_is_rejected() {
    let issued = 1_700_000_000_000;
    let token = issue_session("pw", issued);
    assert!(!verify_session(&token, "other", issued));
}

#[test]
fn session_cookie_names_follow_the_host() {
    assert_eq!(session_cookie_name(Some("127.0.0.1:4096")), "opencode_session_4096");
    assert_eq!(session_cookie_name(Some("localhost")), "opencode_session");
    assert_eq!(session_cookie_name(Some("localhost:80")), "opencode_session");
}

#[test]
fn session_cookies_authorize_without_an_origin() {
    let token = issue_session("pw", UNEXPIRED_NOW_MS);
    let cookie = format!("opencode_session_4096={token}");
    let http_request = request_with("GET", "/api/info", &[("host", "127.0.0.1:4096"), ("cookie", cookie.as_str())]);
    assert!(authorized_cookie(&http_request, "pw"));
}

#[test]
fn session_cookies_authorize_same_origin_requests() {
    let token = issue_session("pw", UNEXPIRED_NOW_MS);
    let cookie = format!("opencode_session_4096={token}");
    let http_request = request_with(
        "GET",
        "/api/info",
        &[("host", "127.0.0.1:4096"), ("origin", "http://127.0.0.1:4096"), ("cookie", cookie.as_str())],
    );
    assert!(authorized_cookie(&http_request, "pw"));
}

#[test]
fn session_cookies_are_rejected_for_cross_origin_requests() {
    let token = issue_session("pw", UNEXPIRED_NOW_MS);
    let cookie = format!("opencode_session_4096={token}");
    let http_request = request_with(
        "GET",
        "/api/info",
        &[("host", "127.0.0.1:4096"), ("origin", "http://localhost:4096"), ("cookie", cookie.as_str())],
    );
    assert!(!authorized_cookie(&http_request, "pw"));
}

#[test]
fn empty_session_cookies_do_not_authorize() {
    assert!(!authorized_cookie(&request("GET", "/api/info"), "pw"));
    let empty = request_with("GET", "/api/info", &[("host", "localhost"), ("cookie", "opencode_session=")]);
    assert!(!authorized_cookie(&empty, "pw"));
}

#[tokio::test]
async fn session_tokens_authorize_basic_credentials() {
    let app = ready_app();
    let token = issue_session("pw", UNEXPIRED_NOW_MS);
    let credential = format!("Basic {}", encoded("opencode", &token));
    let http_request = request_with("GET", "/api/info", &[("authorization", credential.as_str())]);
    let response = app.handle(http_request).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn session_cookies_authorize_info_requests() {
    let app = ready_app();
    let token = issue_session("pw", UNEXPIRED_NOW_MS);
    let cookie = format!("opencode_session={token}");
    let http_request = request_with("GET", "/api/info", &[("host", "localhost"), ("cookie", cookie.as_str())]);
    let response = app.handle(http_request).await;
    assert_eq!(response.status(), StatusCode::OK);
}
