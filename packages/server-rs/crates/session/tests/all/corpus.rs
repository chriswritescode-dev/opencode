use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use opencode_bus::sse::data_frame;
use opencode_bus::{
    Bus, BusError, BusOptions, Definition, Event, ListenerHandle, LogInput, LogItem, PublishOptions, ReplayOptions,
    SerializedEvent,
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
async fn unported_durable_type_fails() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 0i64)), ..Default::default() });
    let _projector = SessionProjector::register(&bus, &database, Arc::new(ProjectionSettings::default));

    let definition =
        Definition::latest_durable("session.inbox.enqueued").expect("the manifest carries session.inbox.enqueued");
    let error = bus
        .publish(&definition, json!({ "sessionID": "ses_1" }), PublishOptions::default())
        .expect_err("an unported durable type is rejected");
    match error {
        BusError::InvalidDurableEvent { message, .. } => {
            assert_eq!(message, "not ported yet", "the projector reports the missing port");
        }
        other => panic!("expected an invalid durable event, got {other:?}"),
    }

    let events: i64 = database
        .read(|connection| Ok(connection.query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?))
        .expect("count the event rows");
    assert_eq!(events, 0, "the rejected publish rolls back the event row");
}

async fn run_fixture(fixture: &Value, masks: &Masks) {
    let name = fixture["name"].as_str().expect("a fixture carries a name");
    match &fixture["error"] {
        Value::Null => {}
        Value::String(error) => panic!("fixture {name} could not be replayed: {error}"),
        _ => panic!("fixture {name} carries an unexpected error value"),
    }

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
    for key in fixture["frames"].as_object().unwrap_or_else(|| panic!("fixture {name} carries no frames")).keys() {
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
    for (index, step) in steps.iter().enumerate() {
        let created = step["created"].as_i64().unwrap_or_else(|| panic!("fixture {name} step {index}: no created"));
        now.store(created, Ordering::Relaxed);
        match step["kind"].as_str().unwrap_or_else(|| panic!("fixture {name} step {index}: no kind")) {
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
                bus.publish(&definition, step["data"].clone(), options)
                    .unwrap_or_else(|error| panic!("fixture {name} step {index}: publish failed: {error}"));
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
                    .unwrap_or_else(|error| panic!("fixture {name} step {index}: replay failed: {error}"));
            }
            kind => panic!("fixture {name} step {index}: unknown step kind {kind}"),
        }
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
