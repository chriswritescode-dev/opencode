use http::StatusCode;

use crate::{UNAUTHORIZED_BODY, app, authorized, basic, encoded, ready_app, request, request_with, text};

const PTY_CONNECT_BODY: &str =
    r#"{"_tag":"UnknownError","message":"opencode-server does not implement pty.connect yet"}"#;

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
    assert_eq!(pty.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(text(pty).await, PTY_CONNECT_BODY);
    let persistent = app.handle(request("GET", "/api/experimental/persistent-pty/an-id/connect?ticket=t")).await;
    assert_eq!(persistent.status(), StatusCode::NOT_IMPLEMENTED);
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
    assert_eq!(connect.status(), StatusCode::NOT_IMPLEMENTED);
    let incomplete = app.handle(request("GET", "/auth/connect/")).await;
    assert_eq!(incomplete.status(), StatusCode::UNAUTHORIZED);
}
