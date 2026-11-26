use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::StatusCode;
use http_body_util::BodyExt;
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_http::state::ServerState;
use opencode_storage::DatabaseLocation;
use serde_json::Value;
use tokio::time::{Duration, timeout};

use crate::{authorized, temp_directory};

fn state() -> Arc<ServerState> {
    let (state, _) = ServerState::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the server state");
    Arc::new(state)
}

fn services(state: &ServerState, start: &Path, home: &Path) -> Arc<GlobalServices> {
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.as_os_str().to_owned())).expect("global paths");
    Arc::new(GlobalServices::new(
        state.db.clone(),
        paths,
        start.to_string_lossy().into_owned(),
        crate::offline_env(),
        state.bus.clone(),
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
        Arc::new(Vec::new()),
    ))
}

fn app(state: &Arc<ServerState>, services: Arc<GlobalServices>) -> Arc<App> {
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(Vec::new),
        state: Some(state.clone()),
        services: Some(services),
    });
    app.status().ready();
    app
}

fn write(project: &Path, relative: &str, content: &str) {
    let path = project.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the parent directory");
    }
    std::fs::write(&path, content).expect("write the file");
}

fn uri(path: &str, directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("{path}?location%5Bdirectory%5D={encoded}")
}

async fn json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&crate::text(response).await).expect("a JSON body")
}

async fn next_frame(body: &mut opencode_http::response::Body, buffer: &mut Vec<u8>) -> String {
    loop {
        if let Some(position) = buffer.windows(2).position(|pair| pair == b"\n\n") {
            let frame = buffer.drain(..position + 2).collect::<Vec<u8>>();
            return String::from_utf8(frame).expect("a utf-8 frame");
        }
        let frame = body.frame().await.expect("a body frame").expect("a frame");
        if let Some(data) = frame.data_ref() {
            buffer.extend_from_slice(data);
        }
    }
}

#[tokio::test]
async fn registry_routes_return_the_builtins_and_project_contributors() {
    let start = temp_directory("registry-start");
    let home = temp_directory("registry-home");
    write(&start, "opencode.json", r#"{"agent":{"scribe":{"description":"Writes the documentation"}}}"#);
    write(
        &start,
        ".opencode/agents/reviewer.md",
        "---\ndescription: Reviews code\nmode: subagent\n---\nReview carefully.\n",
    );
    write(&start, ".opencode/commands/hello.md", "---\ndescription: Say hello\n---\nHello $ARGUMENTS\n");
    let state = state();
    let app = app(&state, services(&state, &start, &home));

    let response = app.handle(authorized("GET", &uri("/api/agent", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["location"]["directory"], start.to_string_lossy().as_ref());
    let ids = body["data"]
        .as_array()
        .expect("the agent list")
        .iter()
        .map(|agent| agent["id"].as_str().expect("an agent id"))
        .collect::<Vec<_>>();
    for id in ["build", "general", "explore", "compaction", "title", "summary", "plan", "reviewer", "scribe"] {
        assert!(ids.contains(&id), "{id}");
    }

    let response = app.handle(authorized("GET", &uri("/api/agent/reviewer", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["id"], "reviewer");
    assert_eq!(body["data"]["description"], "Reviews code");
    assert_eq!(body["data"]["mode"], "subagent");

    let response = app.handle(authorized("GET", &uri("/api/agent/nope", &start))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "AgentNotFoundError");
    assert_eq!(body["agentID"], "nope");
    assert_eq!(body["message"], "Agent not found: nope");

    let response = app.handle(authorized("GET", &uri("/api/command", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let commands = body["data"].as_array().expect("the command list");
    let names = commands.iter().map(|command| command["name"].as_str().expect("a command name")).collect::<Vec<_>>();
    for name in ["init", "review", "hello"] {
        assert!(names.contains(&name), "{name}");
    }
    let hello = commands.iter().find(|command| command["name"] == "hello").expect("the hello command");
    assert_eq!(hello["description"], "Say hello");

    let response = app.handle(authorized("GET", &uri("/api/skill", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let ids = body["data"]
        .as_array()
        .expect("the skill list")
        .iter()
        .map(|skill| skill["id"].as_str().expect("a skill id"))
        .collect::<Vec<_>>();
    for id in ["opencode", "report"] {
        assert!(ids.contains(&id), "{id}");
    }

    let response = app.handle(authorized("GET", &uri("/api/reference", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"], serde_json::json!([]));
}

#[tokio::test]
async fn registry_updates_reach_the_event_stream_with_the_location() {
    let start = temp_directory("registry-event-start");
    let home = temp_directory("registry-event-home");
    let state = state();
    let app = app(&state, services(&state, &start, &home));

    let response = app.handle(authorized("GET", "/api/event")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body();
    let mut buffer = Vec::new();

    let response = app.handle(authorized("GET", &uri("/api/agent", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let directory = body["location"]["directory"].as_str().expect("the location directory").to_owned();

    let expected = ["agent.updated", "command.updated", "skill.updated", "reference.updated"];
    let seen = timeout(Duration::from_secs(15), async {
        let mut seen = HashSet::new();
        while seen.len() < expected.len() {
            let frame = next_frame(&mut stream, &mut buffer).await;
            let Some(data) = frame.strip_prefix("data: ") else {
                continue;
            };
            let event: Value = serde_json::from_str(data.trim_end()).expect("a JSON event");
            let Some(r#type) = event["type"].as_str() else {
                continue;
            };
            if !expected.contains(&r#type) {
                continue;
            }
            assert_eq!(event["location"]["directory"], directory.as_str());
            seen.insert(r#type.to_owned());
        }
        seen
    })
    .await
    .expect("the registry events");
    assert_eq!(seen.len(), expected.len());
}
