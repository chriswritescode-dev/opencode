use std::collections::BTreeSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use opencode_bus::sse::data_frame;
use opencode_bus::{
    Bus, BusOptions, Definition, Event, ListenerHandle, LogInput, LogItem, PublishOptions, ReplayOptions,
    SerializedEvent, versioned_type,
};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{AbsolutePath, DURABLE_EVENTS, LocationRef, PUBLIC_EVENT_TYPES, WorkspaceID};
use opencode_session::{ProjectionSettings, SessionProjector};
use opencode_storage::dump::{Masks, dump};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Map, Value, json};

use crate::TempDir;

const CORPUS: &str = include_str!("../corpus/events.json");

#[tokio::test]
async fn session() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "session" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 11, "the session group carries every ported fixture");
}

#[tokio::test]
async fn messages() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "messages" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 8, "the messages group carries every ported fixture");
}

#[tokio::test]
async fn assistant() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "assistant" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 6, "the assistant group carries every ported fixture");
}

#[tokio::test]
async fn inline() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "inline" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 7, "the inline group carries every ported fixture");
}

#[tokio::test]
async fn inbox() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "inbox" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 6, "the inbox group carries every ported fixture");
}

#[tokio::test]
async fn instructions() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "instructions" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 4, "the instructions group carries every ported fixture");
}

#[tokio::test]
async fn media() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "media" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 9, "the media group carries every ported fixture");
}

#[tokio::test]
async fn fork_revert() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "fork-revert" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 6, "the fork-revert group carries every ported fixture");
}

#[tokio::test]
async fn inline_update_sets_time_updated() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 0i64)), ..Default::default() });
    let settings = ProjectionSettings { part_rows: false, ..Default::default() };
    let _projector = SessionProjector::register(&bus, &database, Arc::new(move || settings));
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1000, 1000)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
                 VALUES ('msg_1', 'ses_1', 'assistant', 0, 1000, 1000, \
                 '{\"time\":{\"created\":1000},\"agent\":\"build\",\"model\":{\"id\":\"gpt-5\",\"providerID\":\"openai\"},\"content\":[],\"type\":\"assistant\"}')",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, time_updated, data) \
                 VALUES ('msg_1', 'ses_1', 0, 'text', NULL, 1000, 1000, '{\"type\":\"text\",\"text\":\"first\"}')",
                [],
            )?;
            Ok(())
        })
        .expect("seed the session");

    let before = SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_millis() as i64;
    let definition = Definition::latest_durable("session.text.ended").expect("session.text.ended is durable");
    bus.publish(
        &definition,
        json!({ "sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "text": "done" }),
        PublishOptions::default(),
    )
    .expect("publish the text end");

    let (time_updated, data) = database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT time_updated, data FROM session_message WHERE id = 'msg_1'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?)
        })
        .expect("read the updated message");
    assert_ne!(time_updated, 1000, "the inline update bumps time_updated off the seeded value");
    assert!(time_updated >= before, "the inline update uses the wall clock at projection time");
    assert!(data.contains("\"content\":[]"), "the envelope stays empty while part rows hold the content");
}

#[test]
fn projector_registers_every_durable_type() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let _projector = SessionProjector::register(&bus, &database, Arc::new(ProjectionSettings::default));
    let mut registered = bus.projector_registrations();
    registered.sort();
    let mut expected = DURABLE_EVENTS
        .iter()
        .map(|(r#type, version, _)| (versioned_type(r#type, *version), 1usize))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(registered, expected, "each durable event type is registered exactly once");
}

#[test]
fn corpus_covers_every_durable_type() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let mut covered = BTreeSet::new();
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        for step in fixture["steps"].as_array().unwrap_or_else(|| panic!("fixture {name} carries no steps")) {
            let r#type = step["type"].as_str().unwrap_or_else(|| panic!("fixture {name}: a step carries no type"));
            match step["kind"].as_str().unwrap_or_else(|| panic!("fixture {name}: a step carries no kind")) {
                "publish" => {
                    let version = DURABLE_EVENTS
                        .iter()
                        .find(|(candidate, ..)| *candidate == r#type)
                        .map(|(_, version, _)| *version)
                        .unwrap_or_else(|| panic!("fixture {name}: unknown publish type {type}"));
                    covered.insert(versioned_type(r#type, version));
                }
                "replay" => {
                    covered.insert(r#type.to_string());
                }
                kind => panic!("fixture {name}: unknown step kind {kind}"),
            }
        }
    }
    for (r#type, version, _) in DURABLE_EVENTS {
        let expected = versioned_type(r#type, version);
        assert!(covered.contains(&expected), "the corpus does not cover {expected}");
    }
}

#[test]
fn corpus_has_no_unexpected_errors() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        let expect_error =
            fixture["expectError"].as_bool().unwrap_or_else(|| panic!("fixture {name} carries no expectError"));
        assert_eq!(
            fixture["error"].is_null(),
            !expect_error,
            "fixture {name}: the recorded error must be present if and only if expectError is true"
        );
    }
}

async fn run_fixture(fixture: &Value, masks: &Masks) {
    let name = fixture["name"].as_str().expect("a fixture carries a name");
    let expected = match &fixture["error"] {
        Value::Null => None,
        Value::String(error) => Some(error.as_str()),
        _ => panic!("fixture {name} carries an unexpected error value"),
    };

    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let settings = fixture["settings"].as_object().unwrap_or_else(|| panic!("fixture {name} carries no settings"));
    let settings = ProjectionSettings {
        part_rows: settings["partRows"]
            .as_bool()
            .unwrap_or_else(|| panic!("fixture {name} settings carry no partRows")),
        media_blob: settings["mediaBlob"]
            .as_bool()
            .unwrap_or_else(|| panic!("fixture {name} settings carry no mediaBlob")),
        fork_media: settings["forkMedia"]
            .as_bool()
            .unwrap_or_else(|| panic!("fixture {name} settings carry no forkMedia")),
    };
    let bus = Bus::new(
        database.clone(),
        BusOptions {
            persist: fixture["persist"].as_bool().unwrap_or_else(|| panic!("fixture {name} carries no persist")),
            now: Some(Arc::new(move || clock.load(Ordering::Relaxed))),
            ..Default::default()
        },
    );
    let _projector = SessionProjector::register(&bus, &database, Arc::new(move || settings));

    for statement in fixture["start"].as_array().unwrap_or_else(|| panic!("fixture {name} carries no start statements"))
    {
        let statement = statement.as_str().expect("a start entry is a statement");
        if let Err(error) = database.read(|connection| {
            connection.execute(statement, [])?;
            Ok(())
        }) {
            panic!("fixture {name}: start statement failed: {error}");
        }
    }

    let mut listeners: Vec<ListenerHandle> = Vec::new();
    let mut frame_sets: Vec<(String, Arc<Mutex<Vec<String>>>)> = Vec::new();
    let frames_expected = match expected {
        Some(_) => None,
        None => Some(fixture["frames"].as_object().unwrap_or_else(|| panic!("fixture {name} carries no frames"))),
    };
    for key in frames_expected.into_iter().flat_map(|frames| frames.keys()) {
        let filter = (key != "all").then(|| key.split(',').map(str::to_string).collect::<Vec<_>>());
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        listeners.push(bus.listen(Box::new(move |event: &Event| {
            let r#type = event.r#type();
            if !PUBLIC_EVENT_TYPES.contains(&r#type) {
                return Ok(());
            }
            if let Some(filter) = &filter
                && !filter.iter().any(|candidate| candidate.as_str() == r#type)
            {
                return Ok(());
            }
            if r#type == "session.usage.updated" {
                return Ok(());
            }
            sink.lock()
                .expect("lock the frame sink")
                .push(data_frame(&encode_json(&Value::Object(event.payload().clone()))));
            Ok(())
        })));
        frame_sets.push((key.clone(), frames));
    }

    let usage = Arc::new(Mutex::new(Vec::new()));
    let usage_sink = usage.clone();
    listeners.push(bus.listen(Box::new(move |event: &Event| {
        if event.r#type() == "session.usage.updated" {
            usage_sink.lock().expect("lock the usage sink").push(encode_json(event.data()));
        }
        Ok(())
    })));

    let steps = fixture["steps"].as_array().unwrap_or_else(|| panic!("fixture {name} carries no steps"));
    let mut failed = false;
    for (index, step) in steps.iter().enumerate() {
        let created = step["created"].as_i64().unwrap_or_else(|| panic!("fixture {name} step {index}: no created"));
        now.store(created, Ordering::Relaxed);
        let result = match step["kind"].as_str().unwrap_or_else(|| panic!("fixture {name} step {index}: no kind")) {
            "publish" => {
                let r#type = step["type"].as_str().unwrap_or_else(|| panic!("fixture {name} step {index}: no type"));
                let definition = Definition::latest_durable(r#type)
                    .unwrap_or_else(|| panic!("fixture {name} step {index}: unknown durable type {type}"));
                let options = PublishOptions {
                    id: Some(
                        step["id"].as_str().unwrap_or_else(|| panic!("fixture {name} step {index}: no id")).to_string(),
                    ),
                    metadata: step["metadata"].as_object().cloned(),
                    location: step["location"].as_object().map(|_| location(&step["location"], name, index)),
                    ..Default::default()
                };
                bus.publish(&definition, step["data"].clone(), options).map(|_| ())
            }
            "replay" => {
                let event = SerializedEvent {
                    id: step["id"].as_str().unwrap_or_else(|| panic!("fixture {name} step {index}: no id")).to_string(),
                    r#type: step["type"]
                        .as_str()
                        .unwrap_or_else(|| panic!("fixture {name} step {index}: no type"))
                        .to_string(),
                    created: Some(created),
                    seq: step["seq"].as_i64().unwrap_or_else(|| panic!("fixture {name} step {index}: no seq")),
                    aggregate_id: step["aggregateID"]
                        .as_str()
                        .unwrap_or_else(|| panic!("fixture {name} step {index}: no aggregateID"))
                        .to_string(),
                    data: step["data"].clone(),
                };
                let options =
                    ReplayOptions { publish: step["publish"].as_bool().unwrap_or(true), ..Default::default() };
                bus.replay(event, options)
            }
            kind => panic!("fixture {name} step {index}: unknown step kind {kind}"),
        };
        if let Err(error) = result {
            let Some(expected) = expected else {
                panic!("fixture {name} step {index}: failed: {error}");
            };
            assert_eq!(
                error.to_string(),
                expected,
                "fixture {name} step {index}: the failure carries the recorded error"
            );
            failed = true;
            break;
        }
    }

    if let Some(expected) = expected {
        assert!(failed, "fixture {name} records the error {expected:?} and the replay must reach it");
        for field in ["dump", "frames", "usage", "log"] {
            assert!(fixture[field].is_null(), "fixture {name} records no {field} once it fails");
        }
        return;
    }

    let mut sessions: Vec<String> = Vec::new();
    for step in steps {
        let session = if step["kind"].as_str() == Some("publish") {
            step["data"]["sessionID"].as_str().map(str::to_string)
        } else {
            step["aggregateID"].as_str().map(str::to_string)
        };
        if let Some(session) = session
            && !sessions.iter().any(|known| known.as_str() == session)
        {
            sessions.push(session);
        }
    }

    let log_after = fixture["logAfter"].as_array().unwrap_or_else(|| panic!("fixture {name} carries no logAfter"));
    let mut log = Map::new();
    for session in &sessions {
        let mut bodies = Map::new();
        for after in std::iter::once(None::<i64>).chain(log_after.iter().map(|value| value.as_i64())) {
            let mut stream = std::pin::pin!(bus.log(LogInput { aggregate_id: session.clone(), after, follow: false }));
            let mut body = String::new();
            while let Some(item) = stream.next().await {
                let item = item.unwrap_or_else(|error| panic!("fixture {name}: the log for {session} failed: {error}"));
                let keep = item.is_synced()
                    || matches!(&item, LogItem::Event(event)
                        if DURABLE_EVENTS.iter().any(|(r#type, ..)| *r#type == event.r#type()));
                if keep {
                    body.push_str(&data_frame(&item.json()));
                }
            }
            bodies.insert(after.map_or_else(|| "-1".to_string(), |value| value.to_string()), Value::String(body));
        }
        log.insert(session.clone(), Value::Object(bodies));
    }

    let snapshot = database.read(|connection| dump(connection, masks)).expect("dump the projected database");

    let frames = Value::Object(
        frame_sets
            .iter()
            .map(|(key, sink)| {
                let frames = sink.lock().expect("lock the frame sink");
                (key.clone(), Value::Array(frames.iter().cloned().map(Value::String).collect()))
            })
            .collect(),
    );
    let usage = Value::Array(usage.lock().expect("lock the usage sink").iter().cloned().map(Value::String).collect());

    assert_field(name, "frames", &frames, &fixture["frames"]);
    assert_field(name, "usage", &usage, &fixture["usage"]);
    assert_field(name, "log", &Value::Object(log), &fixture["log"]);
    assert_field(name, "dump", &snapshot, &fixture["dump"]);
}

fn location(value: &Value, fixture: &str, step: usize) -> LocationRef {
    LocationRef {
        directory: AbsolutePath(
            value["directory"]
                .as_str()
                .unwrap_or_else(|| panic!("fixture {fixture} step {step}: the location has no directory"))
                .to_string(),
        ),
        workspace_id: value["workspaceID"].as_str().map(|workspace| WorkspaceID(workspace.to_string())),
    }
}

fn assert_field(fixture: &str, field: &str, actual: &Value, expected: &Value) {
    if actual == expected {
        return;
    }
    panic!(
        "fixture {fixture} mismatch on {field}\nactual:\n{}\nexpected:\n{}",
        serde_json::to_string_pretty(actual).expect("serialize the actual value"),
        serde_json::to_string_pretty(expected).expect("serialize the expected value"),
    );
}
