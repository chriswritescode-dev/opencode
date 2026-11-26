use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::StatusCode;
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_http::state::ServerState;
use opencode_http::status::{State, Status};
use opencode_storage::DatabaseLocation;

use crate::{app, authorized, ready_app, text};

fn ready_app_with_state() -> Arc<App> {
    let (state, _) = ServerState::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the state");
    let state = Arc::new(state);
    let paths = GlobalPaths::from_vars(|name| {
        (name == "HOME").then(|| std::env::temp_dir().join("opencode-http-test-home").into_os_string())
    })
    .expect("global paths");
    let mut services = GlobalServices::new(
        state.db.clone(),
        paths,
        "/tmp/opencode-http-test".to_owned(),
        crate::offline_env(),
        state.bus.clone(),
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
        Arc::new(Vec::new()),
    );
    services.attach_sessions(state.bus.clone(), state.store.clone(), "test-version".to_owned());
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
        state: Some(state),
        services: Some(Arc::new(services)),
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
        "model.list" => Some(("GET", "/api/model")),
        "model.default" => Some(("GET", "/api/model/default")),
        "provider.list" => Some(("GET", "/api/provider")),
        "provider.get" => Some(("GET", "/api/provider/unknown")),
        "credential.list" => Some(("GET", "/api/credential")),
        "credential.create" => Some(("POST", "/api/credential")),
        "credential.update" => Some(("PATCH", "/api/credential/x")),
        "credential.remove" => Some(("DELETE", "/api/credential/x")),
        "credential.activate" => Some(("POST", "/api/credential/x/activate")),
        "integration.list" => Some(("GET", "/api/integration")),
        "integration.get" => Some(("GET", "/api/integration/unknown")),
        "experimental.integration.wellknown.add" => Some(("POST", "/api/experimental/integration/wellknown")),
        "integration.connect.key" => Some(("POST", "/api/integration/unknown/connect/key")),
        "integration.connect.external" => Some(("POST", "/api/integration/unknown/connect/external")),
        "integration.oauth.connect" => Some(("POST", "/api/integration/unknown/connect/oauth")),
        "integration.oauth.status" => Some(("GET", "/api/integration/unknown/connect/oauth/con_missing")),
        "integration.oauth.complete" => Some(("POST", "/api/integration/unknown/connect/oauth/con_missing/complete")),
        "integration.oauth.cancel" => Some(("DELETE", "/api/integration/unknown/connect/oauth/con_missing")),
        "integration.command.connect" => Some(("POST", "/api/integration/unknown/connect/command")),
        "integration.command.status" => Some(("GET", "/api/integration/unknown/connect/command/con_missing")),
        "integration.command.cancel" => Some(("DELETE", "/api/integration/unknown/connect/command/con_missing")),
        "experimental.usage.limits" => Some(("GET", "/api/experimental/usage/limits")),
        "agent.list" => Some(("GET", "/api/agent")),
        "agent.get" => Some(("GET", "/api/agent/build")),
        "command.list" => Some(("GET", "/api/command")),
        "skill.list" => Some(("GET", "/api/skill")),
        "form.list" => Some(("GET", "/api/form")),
        "permission.request.list" => Some(("GET", "/api/permission/request")),
        "permission.saved.list" => Some(("GET", "/api/permission/saved")),
        "permission.saved.update" => Some(("PATCH", "/api/permission/saved/psv_test")),
        "permission.saved.remove" => Some(("DELETE", "/api/permission/saved/psv_test")),
        "session.permission.create" => Some(("POST", "/api/session/ses_test/permission")),
        "session.permission.list" => Some(("GET", "/api/session/ses_test/permission")),
        "session.permission.get" => Some(("GET", "/api/session/ses_test/permission/per_test")),
        "session.permission.reply" => Some(("POST", "/api/session/ses_test/permission/per_test/reply")),
        "session.form.list" => Some(("GET", "/api/session/ses_test/form")),
        "session.form.create" => Some(("POST", "/api/session/ses_test/form")),
        "session.form.get" => Some(("GET", "/api/session/ses_test/form/frm_test")),
        "session.form.cancel" => Some(("DELETE", "/api/session/ses_test/form/frm_test")),
        "session.form.reply" => Some(("POST", "/api/session/ses_test/form/frm_test/reply")),
        "session.list" => Some(("GET", "/api/session")),
        "session.create" => Some(("POST", "/api/session")),
        "session.get" => Some(("GET", "/api/session/ses_test")),
        "session.remove" => Some(("DELETE", "/api/session/ses_test")),
        "session.update" => Some(("PATCH", "/api/session/ses_test")),
        "session.prompt" => Some(("POST", "/api/session/ses_test/prompt")),
        "session.message.list" => Some(("GET", "/api/session/ses_test/message")),
        "session.inbox.list" => Some(("GET", "/api/session/ses_test/inbox")),
        "session.switchAgent" => Some(("POST", "/api/session/ses_test/agent")),
        "session.switchModel" => Some(("POST", "/api/session/ses_test/model")),
        "session.active" => Some(("GET", "/api/session/active")),
        "experimental.session.wait" => Some(("POST", "/api/experimental/session/ses_test/wait")),
        "experimental.job.list" => Some(("GET", "/api/experimental/job")),
        "plugin.list" => Some(("GET", "/api/plugin")),
        "plugin.gui" => Some(("GET", "/api/plugin/gui?id=missing")),
        "plugin.check" => Some(("POST", "/api/plugin/check")),
        "plugin.update" => Some(("POST", "/api/plugin/update")),
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
    let response = app.handle(authorized("GET", "/api/experimental/session/stats")).await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(content_type(&response), Some("application/json"));
    assert_eq!(
        text(response).await,
        r#"{"_tag":"UnknownError","message":"opencode-server does not implement session.stats yet"}"#
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
    assert!(opencode_http::app::IMPLEMENTED.contains(&"model.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"model.default"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"provider.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"provider.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"credential.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"credential.create"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"credential.update"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"credential.remove"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"credential.activate"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.integration.wellknown.add"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.connect.key"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.connect.external"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.oauth.connect"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.oauth.status"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.oauth.complete"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.oauth.cancel"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.command.connect"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.command.status"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"integration.command.cancel"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.usage.limits"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"agent.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"agent.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"command.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"skill.list"));
    for operation in [
        "form.list",
        "permission.request.list",
        "permission.saved.list",
        "permission.saved.update",
        "permission.saved.remove",
        "session.permission.create",
        "session.permission.list",
        "session.permission.get",
        "session.permission.reply",
        "session.form.list",
        "session.form.create",
        "session.form.get",
        "session.form.cancel",
        "session.form.reply",
    ] {
        assert!(opencode_http::app::IMPLEMENTED.contains(&operation), "{operation}");
    }
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.create"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.get"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.update"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.remove"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.prompt"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.message.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.inbox.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.switchAgent"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.switchModel"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"session.active"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.session.wait"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"experimental.job.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"plugin.list"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"plugin.gui"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"plugin.check"));
    assert!(opencode_http::app::IMPLEMENTED.contains(&"plugin.update"));
    assert_eq!(opencode_http::app::IMPLEMENTED.len(), 83);
}
