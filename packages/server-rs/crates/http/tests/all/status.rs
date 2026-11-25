use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::StatusCode;
use opencode_http::app::{App, Options};
use opencode_http::state::ServerState;
use opencode_http::status::{State, Status};
use opencode_storage::DatabaseLocation;

use crate::{app, authorized, ready_app, text};

fn ready_app_with_state() -> Arc<App> {
    let (state, _) = ServerState::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the state");
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
        state: Some(Arc::new(state)),
        services: Some(crate::services()),
    });
    app.status().ready();
    app
}

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
    let app = ready_app_with_state();
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
        "event.subscribe" => Some(("GET", "/api/event")),
        "experimental.session.log" => Some(("GET", "/api/experimental/session/ses_test/log")),
        "location.get" => Some(("GET", "/api/location")),
        "location.reload" => Some(("POST", "/api/location/reload")),
        "debug.heapDump" => Some(("POST", "/api/debug/heap-dump")),
        "debug.location.list" => Some(("GET", "/api/debug/location")),
        "debug.location.evict" => Some(("DELETE", "/api/debug/location")),
        "project.list" => Some(("GET", "/api/project")),
        "project.update" => Some(("PATCH", "/api/project/unknown")),
        "vcs.init" => Some(("POST", "/api/vcs/init?provider=unknown")),
        "vcs.get" => Some(("GET", "/api/vcs")),
        "vcs.base" => Some(("GET", "/api/vcs/base")),
        "vcs.status" => Some(("GET", "/api/vcs/status")),
        "vcs.branch.list" => Some(("GET", "/api/vcs/branch")),
        "vcs.diff" => Some(("GET", "/api/vcs/diff?mode=working")),
        "reference.list" => Some(("GET", "/api/reference")),
        "worktree.list" => Some(("GET", "/api/worktree?projectID=unknown")),
        "worktree.create" => Some(("POST", "/api/worktree")),
        "worktree.remove" => Some(("DELETE", "/api/worktree")),
        "worktree.refresh" => Some(("POST", "/api/worktree/refresh")),
        "config.get" => Some(("GET", "/api/config")),
        "config.shells" => Some(("GET", "/api/config/shell")),
        "experimental.config.update" => Some(("PATCH", "/api/experimental/config")),
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
fn implemented_lists_the_ported_operations() {
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.info"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.pair"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"server.connect"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"event.subscribe"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.session.log"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"location.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"location.reload"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"debug.heapDump"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"debug.location.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"debug.location.evict"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"project.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"project.update"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.init"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.base"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.status"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.branch.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"vcs.diff"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"reference.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"worktree.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"worktree.create"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"worktree.remove"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"worktree.refresh"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"config.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"config.shells"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.config.update"));
    assert_eq!(opencode_http::app::IMPLEMENTED.len(), 26);
}
