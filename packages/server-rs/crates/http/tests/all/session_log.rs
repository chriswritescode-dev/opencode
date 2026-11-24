use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use flate2::read::GzDecoder;
use http::StatusCode;
use http_body_util::BodyExt;
use opencode_bus::feed::{EventFeed, SUBSCRIBER_CAPACITY};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_http::app::{App, Options};
use opencode_http::state::ServerState;
use opencode_session::{ProjectionSettings, SessionProjector, SessionStore};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};

const CORPUS: &str = include_str!("../../../session/tests/corpus/events.json");

const PROJECT: &str = "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                       VALUES ('prj_1', '/repo', '[]', 1, 2)";

fn state(persist: bool) -> (Arc<ServerState>, Arc<AtomicI64>) {
    let (db, _) = Database::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the database");
    let db = Arc::new(db);
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let bus = Bus::new(
        db.clone(),
        BusOptions { persist, now: Some(Arc::new(move || clock.load(Ordering::Relaxed))), ..Default::default() },
    );
    let usage = SessionProjector::start(&bus, &db, Arc::new(ProjectionSettings::default));
    let feed = EventFeed::new(&bus, SUBSCRIBER_CAPACITY);
    let store = SessionStore::new(db.clone());
    let state = ServerState { db, bus, feed, store, usage, heartbeat: Duration::from_secs(60) };
    (Arc::new(state), now)
}

fn app(state: Arc<ServerState>) -> Arc<App> {
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp"),
        hostname: None,
        urls: Arc::new(Vec::new),
        state: Some(state),
    });
    app.status().ready();
    app
}

fn seed(state: &ServerState, sql: &str) {
    state
        .db
        .read(|connection| {
            connection.execute_batch(sql)?;
            Ok(())
        })
        .expect("seed the database");
}

fn content_type(response: &opencode_http::response::Response) -> Option<&str> {
    response.headers().get("content-type").and_then(|value| value.to_str().ok())
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

fn created_event() -> Value {
    json!({
        "sessionID": "ses_1",
        "projectID": "prj_1",
        "location": { "directory": "/repo" },
        "slug": "slug",
        "version": "1",
    })
}

fn renamed_event() -> Value {
    json!({ "sessionID": "ses_1", "title": "Renamed" })
}

fn publish(state: &ServerState, r#type: &str, id: &str, data: Value) {
    let definition = Definition::latest_durable(r#type).expect("a durable event type");
    state
        .bus
        .publish(&definition, data, PublishOptions { id: Some(id.to_owned()), ..Default::default() })
        .expect("publish the event");
}

#[tokio::test]
async fn unknown_session_is_a_session_not_found_error() {
    let (state, _) = state(false);
    let app = app(state);
    let response = app.handle(crate::authorized("GET", "/api/experimental/session/ses_missing/log")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/json"));
    assert_eq!(
        crate::text(response).await,
        r#"{"_tag":"SessionNotFoundError","sessionID":"ses_missing","message":"Session not found: ses_missing"}"#
    );
}

#[tokio::test]
async fn invalid_after_and_follow_are_query_errors() {
    let (state, _) = state(false);
    let app = app(state);
    for query in ["after=abc", "follow=maybe"] {
        let uri = format!("/api/experimental/session/ses_1/log?{query}");
        let response = app.handle(crate::authorized("GET", &uri)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let body = crate::text(response).await;
        let value: Value = serde_json::from_str(&body).expect("a JSON body");
        assert_eq!(value["_tag"], "InvalidRequestError", "{body}");
        assert_eq!(value["kind"], "Query", "{body}");
        assert!(value["message"].as_str().is_some_and(|message| !message.is_empty()), "{body}");
        assert!(body.ends_with(r#"","kind":"Query"}"#), "{body}");
    }
}

#[tokio::test]
async fn an_invalid_session_id_is_a_params_error() {
    let (state, _) = state(false);
    let app = app(state);
    let response = app.handle(crate::authorized("GET", "/api/experimental/session/foo/log")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = crate::text(response).await;
    let value: Value = serde_json::from_str(&body).expect("a JSON body");
    assert_eq!(value["_tag"], "InvalidRequestError", "{body}");
    assert_eq!(value["kind"], "Params", "{body}");
    assert!(value["message"].as_str().is_some_and(|message| !message.is_empty()), "{body}");
    assert!(body.ends_with(r#"","kind":"Params"}"#), "{body}");
}

#[tokio::test]
async fn a_non_persistent_session_log_is_only_the_synced_marker() {
    let (state, _) = state(false);
    seed(&state, PROJECT);
    publish(&state, "session.created", "evt_1", created_event());
    let app = app(state);
    let response = app.handle(crate::authorized("GET", "/api/experimental/session/ses_1/log")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(crate::text(response).await, "data: {\"type\":\"log.synced\",\"aggregateID\":\"ses_1\",\"seq\":0}\n\n");
}

#[tokio::test]
async fn a_replayed_fixture_log_matches_the_recorded_bodies() {
    let (state, now) = state(true);
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let fixture = corpus["fixtures"]
        .as_array()
        .expect("the corpus carries fixtures")
        .iter()
        .find(|fixture| fixture["name"] == "created-minimal")
        .expect("the created-minimal fixture");

    for statement in fixture["start"].as_array().expect("the fixture carries start statements") {
        seed(&state, statement.as_str().expect("a start entry is a statement"));
    }

    for step in fixture["steps"].as_array().expect("the fixture carries steps") {
        now.store(step["created"].as_i64().expect("a step carries created"), Ordering::Relaxed);
        let definition = Definition::latest_durable(step["type"].as_str().expect("a step carries type"))
            .expect("a durable step type");
        let options = PublishOptions {
            id: Some(step["id"].as_str().expect("a step carries id").to_owned()),
            metadata: step["metadata"].as_object().cloned(),
            ..Default::default()
        };
        state.bus.publish(&definition, step["data"].clone(), options).expect("publish the fixture step");
    }

    let app = app(state);
    let base = "/api/experimental/session/ses_1/log";
    for (after, expected) in fixture["log"]["ses_1"].as_object().expect("the fixture carries the session log") {
        let uri = if after == "-1" { base.to_owned() } else { format!("{base}?after={after}") };
        let response = app.handle(crate::authorized("GET", &uri)).await;
        assert_eq!(response.status(), StatusCode::OK, "{after}");
        assert_eq!(crate::text(response).await, expected.as_str().expect("a recorded body"), "{after}");
    }
}

#[tokio::test]
async fn follow_keeps_the_stream_open_and_delivers_a_later_event() {
    let (state, now) = state(true);
    seed(&state, PROJECT);
    now.store(1000, Ordering::Relaxed);
    publish(&state, "session.created", "evt_1", created_event());

    let app = app(state.clone());
    let mut response = app.handle(crate::authorized("GET", "/api/experimental/session/ses_1/log?follow=true")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut buffer = Vec::new();
    loop {
        let frame = next_frame(response.body_mut(), &mut buffer).await;
        if frame.contains("\"type\":\"log.synced\"") && frame.contains("\"seq\":0") {
            break;
        }
    }

    now.store(2000, Ordering::Relaxed);
    publish(&state, "session.renamed", "evt_2", renamed_event());

    let frame = tokio::time::timeout(Duration::from_secs(5), next_frame(response.body_mut(), &mut buffer))
        .await
        .expect("a later frame");
    assert!(frame.contains("\"type\":\"session.renamed\""), "{frame}");
    assert!(frame.contains("\"seq\":1"), "{frame}");
}

#[tokio::test]
async fn an_undecodable_row_ends_with_the_recorded_failure_frame() {
    let (state, _) = state(true);
    seed(&state, PROJECT);
    seed(
        &state,
        "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
         VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1, 1)",
    );
    seed(&state, "INSERT INTO event_sequence (aggregate_id, seq) VALUES ('ses_1', 0)");
    seed(
        &state,
        "INSERT INTO event (id, aggregate_id, seq, created, type, data) \
         VALUES ('evt_bad', 'ses_1', 0, 1000, 'session.created.1', '{')",
    );

    let app = app(state);
    let response = app.handle(crate::authorized("GET", "/api/experimental/session/ses_1/log")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("text/event-stream"));

    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let recorded = corpus["failure"]["body"].as_str().expect("the recorded failure body");
    let recorded = recorded.split_once("event: effect/http-api/stream/failure").expect("the recorded failure event").1;
    let recorded = recorded.strip_prefix("\ndata: ").expect("the recorded failure data");
    let recorded = recorded.strip_suffix("\n\n").expect("the recorded frame terminator");
    let mut cause: Value = serde_json::from_str(recorded).expect("the recorded cause");
    let message = serde_json::from_str::<Value>("{").expect_err("invalid JSON").to_string();
    cause[0]["defect"]["message"] = Value::String(message);
    let expected = opencode_bus::sse::event_frame("effect/http-api/stream/failure", &cause.to_string());
    assert_eq!(crate::text(response).await, expected);
}

#[tokio::test]
async fn gzip_compression_decodes_to_the_same_frames() {
    let (state, now) = state(true);
    seed(&state, PROJECT);
    now.store(1000, Ordering::Relaxed);
    publish(&state, "session.created", "evt_1", created_event());
    now.store(2000, Ordering::Relaxed);
    publish(&state, "session.renamed", "evt_2", renamed_event());

    let app = app(state);
    let uri = "/api/experimental/session/ses_1/log";
    let plain = crate::text(app.handle(crate::authorized("GET", uri)).await).await;
    let credential = crate::basic("opencode", "pw");
    let compressed = app
        .handle(crate::request_with("GET", uri, &[("authorization", credential.as_str()), ("accept-encoding", "gzip")]))
        .await;
    assert_eq!(compressed.status(), StatusCode::OK);
    assert_eq!(compressed.headers().get("content-encoding").and_then(|value| value.to_str().ok()), Some("gzip"));
    let mut decoded = Vec::new();
    GzDecoder::new(crate::body(compressed).await.as_ref()).read_to_end(&mut decoded).expect("gzip decompression");
    assert_eq!(String::from_utf8(decoded).expect("utf-8"), plain);
}
