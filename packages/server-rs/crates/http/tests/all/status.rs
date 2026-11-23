use http::StatusCode;
use opencode_http::status::{State, Status};

use crate::{app, authorized, ready_app, text};

fn retry_after(response: &opencode_http::response::Response) -> Option<&str> {
    response.headers().get("retry-after").and_then(|value| value.to_str().ok())
}

fn content_type(response: &opencode_http::response::Response) -> Option<&str> {
    response.headers().get("content-type").and_then(|value| value.to_str().ok())
}

#[test]
fn status_transitions_match_the_typescript_service_status() {
    let status = Status::new();
    assert_eq!(status.current(), State::Starting);
    status.ready();
    assert_eq!(status.current(), State::Ready);
    status.fail();
    assert_eq!(status.current(), State::Ready);
    status.begin_stopping();
    assert_eq!(status.current(), State::Stopping);
    status.begin_stopping();
    assert_eq!(status.current(), State::Stopping);

    let failed = Status::new();
    failed.fail();
    assert_eq!(failed.current(), State::Failed);
    failed.ready();
    assert_eq!(failed.current(), State::Failed);
}

#[tokio::test]
async fn unavailable_states_match_the_typescript_bodies() {
    let service = app();
    let starting = service.handle(authorized("GET", "/api/session")).await;
    assert_eq!(starting.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_after(&starting), Some("1"));
    assert_eq!(text(starting).await, r#"{"code":"service_starting"}"#);

    service.status().begin_stopping();
    let stopping = service.handle(authorized("GET", "/api/session")).await;
    assert_eq!(stopping.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_after(&stopping), Some("1"));
    assert_eq!(text(stopping).await, r#"{"code":"service_stopping"}"#);

    let failed = app();
    failed.status().fail();
    let response = failed.handle(authorized("GET", "/api/session")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(retry_after(&response).is_none());
    assert_eq!(
        text(response).await,
        r#"{"code":"service_failed","message":"The background service could not start.","action":"Run `opencode service restart` after checking the service logs."}"#
    );
}

#[tokio::test]
async fn openapi_is_served_verbatim() {
    let app = ready_app();
    let response = app.handle(authorized("GET", "/openapi.json")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));
    assert_eq!(text(response).await, opencode_contract::SERVED_OPENAPI_JSON);
}

#[tokio::test]
async fn openapi_answers_head_and_normalized_paths() {
    let app = ready_app();
    for (method, uri) in [("HEAD", "/openapi.json/"), ("GET", "/OpenAPI.JSON"), ("GET", "//openapi.json")] {
        let response = app.handle(authorized(method, uri)).await;
        assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
    }
}

#[tokio::test]
async fn every_implemented_operation_answers_a_sample_request() {
    let app = ready_app();
    for operation in opencode_http::app::IMPLEMENTED {
        let (method, uri) = sample_request(operation).unwrap_or_else(|| panic!("no sample request for {operation}"));
        let response = app.handle(authorized(method, uri)).await;
        assert_ne!(response.status(), StatusCode::NOT_IMPLEMENTED, "{operation}");
    }
}

fn sample_request(operation: &str) -> Option<(&'static str, &'static str)> {
    match operation {
        "server.info" => Some(("GET", "/api/info")),
        "server.pair" => Some(("POST", "/api/pair")),
        "server.connect" => Some(("GET", "/auth/connect/x")),
        _ => None,
    }
}

#[tokio::test]
async fn unknown_paths_are_an_empty_404() {
    let app = ready_app();
    let response = app.handle(authorized("GET", "/api/service/stop")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(text(response).await, "");
}

#[tokio::test]
async fn unimplemented_routes_are_501() {
    let app = ready_app();
    let response = app.handle(authorized("GET", "/api/session")).await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(content_type(&response), Some("application/json"));
    assert_eq!(
        text(response).await,
        r#"{"_tag":"UnknownError","message":"opencode-server does not implement session.list yet"}"#
    );
}

#[test]
fn implemented_lists_server_info_pair_and_connect() {
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.info"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.pair"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.connect"));
    assert_eq!(opencode_http::app::IMPLEMENTED.len(), 3);
}
