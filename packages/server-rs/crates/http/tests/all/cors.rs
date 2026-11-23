use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use opencode_http::cors::{is_allowed_cors_origin, is_allowed_request_origin};
use opencode_http::response::Response;

use crate::{UNAUTHORIZED_BODY, app, authorized, body, ready_app, ready_app_with_cors, request_with, text};

fn header(response: &Response, name: header::HeaderName) -> Option<&str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

fn authorized_with_origin(method: &str, uri: &str, origin: &'static str) -> Request<Bytes> {
    let mut request = authorized(method, uri);
    request.headers_mut().insert(header::ORIGIN, HeaderValue::from_static(origin));
    request
}

#[test]
fn default_and_extra_origins_follow_the_typescript_allowlist() {
    let extras = vec!["http://192.168.1.10:3001".to_owned(), "https://example.com".to_owned()];
    assert!(!is_allowed_cors_origin(Some("http://192.168.1.10:3001"), &[]));
    assert!(is_allowed_cors_origin(Some("http://192.168.1.10:3001"), &extras));
    assert!(is_allowed_cors_origin(Some("https://example.com"), &extras));
    assert!(is_allowed_cors_origin(Some("http://localhost:3001"), &extras));
    assert!(is_allowed_cors_origin(Some("https://app.opencode.ai"), &extras));
    assert!(is_allowed_cors_origin(None, &extras));
    assert!(!is_allowed_cors_origin(Some("http://192.168.1.10:3002"), &extras));
    assert!(!is_allowed_cors_origin(Some("https://example.com.evil.test"), &extras));
    assert!(!is_allowed_cors_origin(Some("http://example.com"), &extras));
    assert!(!is_allowed_cors_origin(Some("null"), &extras));
    for origin in [
        "oc://renderer",
        "oc://renderer/anything",
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "https://opencode.ai",
        "https://a.b.opencode.ai",
        "http://127.0.0.1:4096",
    ] {
        assert!(is_allowed_cors_origin(Some(origin), &extras), "{origin}");
    }
    for origin in ["https://opencode.ai.evil", "https://APP.opencode.ai"] {
        assert!(!is_allowed_cors_origin(Some(origin), &extras), "{origin}");
    }
}

#[test]
fn same_host_requests_are_allowed_without_the_allowlist() {
    let extras = vec!["http://192.168.1.10:3001".to_owned()];
    assert!(!is_allowed_request_origin(Some("http://192.168.1.10:3001"), Some("192.168.1.10:1029"), &[]));
    assert!(is_allowed_request_origin(Some("http://192.168.1.10:3001"), Some("192.168.1.10:1029"), &extras));
    assert!(is_allowed_request_origin(Some("http://192.168.1.10:1029"), Some("192.168.1.10:1029"), &extras));
    assert!(!is_allowed_request_origin(Some("http://192.168.1.10:3002"), Some("192.168.1.10:1029"), &extras));
    assert!(is_allowed_request_origin(Some("http://example.com"), Some("example.com"), &extras));
    assert!(is_allowed_request_origin(Some("https://example.com:443"), Some("example.com"), &extras));
    assert!(is_allowed_request_origin(Some("http://[::1]:3000"), Some("[::1]:3000"), &extras));
    assert!(is_allowed_request_origin(Some(""), Some("example.com"), &extras));
    assert!(!is_allowed_request_origin(Some("null"), Some("null"), &extras));
}

#[tokio::test]
async fn preflight_answers_options_with_the_typescript_header_set() {
    let app = app();
    let request = request_with(
        "OPTIONS",
        "/api/info",
        &[
            ("origin", "http://localhost:3000"),
            ("access-control-request-method", "GET"),
            ("access-control-request-headers", "authorization"),
        ],
    );
    let response = app.handle(request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(header(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("http://localhost:3000"));
    assert_eq!(header(&response, header::ACCESS_CONTROL_ALLOW_METHODS), Some("GET, HEAD, PUT, PATCH, POST, DELETE"));
    assert_eq!(header(&response, header::ACCESS_CONTROL_ALLOW_HEADERS), Some("authorization"));
    assert_eq!(header(&response, header::ACCESS_CONTROL_MAX_AGE), Some("86400"));
    assert_eq!(header(&response, header::VARY), Some("Origin, Access-Control-Request-Headers"));
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn preflight_without_request_headers_sends_an_empty_allow_headers() {
    let app = app();
    let request = request_with("OPTIONS", "/api/info", &[("origin", "http://localhost:3000")]);
    let response = app.handle(request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(header(&response, header::ACCESS_CONTROL_ALLOW_HEADERS), Some(""));
    assert_eq!(header(&response, header::VARY), Some("Origin"));
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn preflight_from_an_untrusted_origin_does_not_echo_it() {
    let app = app();
    let request = request_with(
        "OPTIONS",
        "/api/info",
        &[("origin", "https://example.com"), ("access-control-request-headers", "authorization")],
    );
    let response = app.handle(request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(header(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert_eq!(header(&response, header::VARY), Some("Origin, Access-Control-Request-Headers"));
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn allowed_origins_add_the_response_header_and_vary() {
    let app = ready_app();
    let allowed = app.handle(authorized_with_origin("GET", "/api/info", "http://localhost:3000")).await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(header(&allowed, header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("http://localhost:3000"));
    assert_eq!(header(&allowed, header::VARY), Some("Origin, Accept-Encoding"));
    let untrusted = app.handle(authorized_with_origin("GET", "/api/info", "https://example.com")).await;
    assert_eq!(untrusted.status(), StatusCode::OK);
    assert!(header(&untrusted, header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert_eq!(header(&untrusted, header::VARY), Some("Origin, Accept-Encoding"));
}

#[tokio::test]
async fn configured_extra_origins_are_echoed() {
    let app = ready_app_with_cors(vec!["https://example.com".to_owned()]);
    let allowed = app.handle(authorized_with_origin("GET", "/api/info", "https://example.com")).await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(header(&allowed, header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("https://example.com"));
    let untrusted = app.handle(authorized_with_origin("GET", "/api/info", "https://other.example")).await;
    assert_eq!(untrusted.status(), StatusCode::OK);
    assert!(header(&untrusted, header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn the_empty_404_only_varies_on_origin() {
    let app = ready_app();
    let mut request = authorized("GET", "/missing");
    request.headers_mut().insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    let response = app.handle(request).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(header(&response, header::VARY), Some("Origin"));
    assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn cross_origin_requests_to_the_api_are_passed_through() {
    let app = app();
    let allowed = app.handle(request_with("GET", "/api/info", &[("origin", "http://localhost:3000")])).await;
    assert_eq!(allowed.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(header(&allowed, header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("http://localhost:3000"));
    assert_eq!(text(allowed).await, UNAUTHORIZED_BODY);
    let untrusted = app.handle(request_with("GET", "/api/info", &[("origin", "https://example.com")])).await;
    assert_eq!(untrusted.status(), StatusCode::UNAUTHORIZED);
    assert!(header(&untrusted, header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert_eq!(text(untrusted).await, UNAUTHORIZED_BODY);
}
