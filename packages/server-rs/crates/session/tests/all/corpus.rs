use std::collections::BTreeSet;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use opencode_bus::feed::{EventFeed, FeedError, FeedSubscription, SUBSCRIBER_CAPACITY, frame};
use opencode_bus::{
    Bus, BusOptions, Definition, Event, LogInput, LogItem, PublishOptions, ReplayOptions, SerializedEvent,
    versioned_type,
};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{AbsolutePath, DURABLE_EVENTS, LocationRef, PUBLIC_EVENT_TYPES, WorkspaceID};
use opencode_session::{
    Cursor, Direction, LastAssistantInput, ListAnchor, ListInput, MessagesInput, Order, ProjectionSettings,
    SessionProjector, SessionStore,
};
use opencode_storage::dump::{Masks, dump};
use opencode_storage::{Database, DatabaseLocation};
use rusqlite::OptionalExtension;
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
async fn store() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("read the corpus masks");
    let mut count = 0;
    for fixture in corpus["fixtures"].as_array().expect("the corpus carries fixtures") {
        let name = fixture["name"].as_str().expect("a fixture carries a name");
        if fixture["group"].as_str().unwrap_or_else(|| panic!("fixture {name} carries no group")) != "store" {
            continue;
        }
        run_fixture(fixture, &masks).await;
        count += 1;
    }
    assert_eq!(count, 6, "the store group carries every ported fixture");
}

#[tokio::test]
async fn inline_update_sets_time_updated() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 0i64)), ..Default::default() });
    let settings = ProjectionSettings { part_rows: false, ..Default::default() };
    SessionProjector::register(&bus, Arc::new(move || settings));
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
    SessionProjector::register(&bus, Arc::new(ProjectionSettings::default));
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

#[test]
fn feed_record() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let record = run_feed_record();
    assert_field("feed", "feed", &record, &corpus["feed"]);
}

fn run_feed_record() -> Value {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let bus = Bus::new(Arc::new(database), BusOptions { now: Some(Arc::new(|| 1000)), ..Default::default() });
    let feed = EventFeed::new(&bus, 2);
    let mut all = feed.subscribe(None);
    let mut slow = feed.subscribe(None);
    let mut filtered = feed.subscribe(Some(vec!["agent.updated".to_string()]));

    let mut all_frames = Vec::new();
    let mut filtered_frames = Vec::new();
    for (id, r#type, data) in [
        ("evt_feed_0", "test.internal", json!({ "value": "hidden" })),
        ("evt_feed_1", "agent.updated", json!({})),
        ("evt_feed_2", "model.updated", json!({})),
        ("evt_feed_3", "provider.updated", json!({})),
        ("evt_feed_4", "credential.updated", json!({})),
        ("evt_feed_5", "integration.updated", json!({})),
    ] {
        bus.publish(
            &Definition::ephemeral(r#type),
            data,
            PublishOptions { id: Some(id.to_string()), ..Default::default() },
        )
        .expect("publish the feed event");
        drain_feed(&mut all, &mut all_frames);
        drain_feed(&mut filtered, &mut filtered_frames);
    }

    let mut slow_frames = Vec::new();
    let slow_outcome = loop {
        match slow.try_recv() {
            Some(Ok(frame)) => slow_frames.push(Value::String(frame)),
            Some(Err(FeedError::SubscriberOverflow { capacity })) => {
                break json!({ "tag": "EventFeed.SubscriberOverflow", "capacity": capacity });
            }
            Some(Err(error)) => panic!("the slow subscriber failed with {error}"),
            None => break json!({ "tag": "end" }),
        }
    };

    json!({
        "capacity": 2,
        "frames": { "all": all_frames, "slow": slow_frames, "filtered": filtered_frames },
        "outcomes": {
            "all": { "tag": "end" },
            "slow": slow_outcome,
            "filtered": { "tag": "end" },
        },
    })
}

fn drain_feed(subscription: &mut FeedSubscription, frames: &mut Vec<Value>) {
    while let Some(item) = subscription.try_recv() {
        match item {
            Ok(frame) => frames.push(Value::String(frame)),
            Err(error) => panic!("the feed subscriber failed with {error}"),
        }
    }
}

fn frame_types(key: &str) -> Vec<String> {
    if key != "all" {
        return key.split(',').map(str::to_string).collect();
    }
    PUBLIC_EVENT_TYPES
        .iter()
        .filter(|r#type| **r#type != "session.usage.updated")
        .map(|r#type| (*r#type).to_string())
        .collect()
}

fn merged_masks(global: &Masks, fixture: &Value) -> Masks {
    let mut masks = global.clone();
    let Some(local) = fixture.get("masks") else {
        return masks;
    };
    let local: Masks = serde_json::from_value(local.clone()).expect("read the fixture masks");
    for (table, columns) in local {
        masks.entry(table).or_default().extend(columns);
    }
    masks
}

async fn run_fixture(fixture: &Value, global_masks: &Masks) {
    let name = fixture["name"].as_str().expect("a fixture carries a name");
    let masks = merged_masks(global_masks, fixture);
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
    let _usage = SessionProjector::start(&bus, &database, Arc::new(move || settings));

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

    let feed = expected.is_none().then(|| EventFeed::new(&bus, SUBSCRIBER_CAPACITY));
    let mut subscriptions: Vec<(String, FeedSubscription)> = Vec::new();
    if let Some(feed) = &feed {
        let frames = fixture["frames"].as_object().unwrap_or_else(|| panic!("fixture {name} carries no frames"));
        for key in frames.keys() {
            subscriptions.push((key.clone(), feed.subscribe(Some(frame_types(key)))));
        }
    }

    let usage = Arc::new(Mutex::new(Vec::new()));
    let usage_sink = usage.clone();
    let expected_usage = Arc::new(AtomicUsize::new(0));
    let expected_sink = expected_usage.clone();
    let usage_database = database.clone();
    let _usage_listener = bus.listen(Box::new(move |event: &Event| {
        if event.r#type() == "session.usage.updated" {
            usage_sink.lock().expect("lock the usage sink").push(encode_json(event.data()));
        }
        if produces_usage(&usage_database, event) {
            expected_sink.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }));

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
        let mut waits = 0;
        while usage.lock().expect("lock the usage sink").len() < expected_usage.load(Ordering::SeqCst) {
            waits += 1;
            assert!(waits <= 1000, "fixture {name} step {index}: the usage publisher did not catch up");
            tokio::task::yield_now().await;
        }
    }

    if let Some(expected) = expected {
        assert!(failed, "fixture {name} records the error {expected:?} and the replay must reach it");
        for field in ["dump", "frames", "usage", "log", "storeResults"] {
            assert!(fixture[field].is_null(), "fixture {name} records no {field} once it fails");
        }
        return;
    }

    if let Some(expected_results) = fixture["storeResults"].as_array() {
        let queries = fixture["store"]
            .as_array()
            .unwrap_or_else(|| panic!("fixture {name} carries storeResults without store queries"));
        let store = SessionStore::new(database.clone());
        let actual_results = queries.iter().map(|query| run_store_query(&store, name, query)).collect::<Vec<_>>();
        assert_eq!(
            actual_results.len(),
            expected_results.len(),
            "fixture {name}: the store carries one result per query"
        );
        for (index, (actual, expected)) in actual_results.iter().zip(expected_results).enumerate() {
            let actual = serde_json::from_str::<Value>(actual).unwrap_or_else(|error| {
                panic!("fixture {name} store query {index}: the actual result is not JSON: {error}")
            });
            let expected = expected
                .as_str()
                .unwrap_or_else(|| panic!("fixture {name} store query {index}: the recorded result is not a string"));
            let expected = serde_json::from_str::<Value>(expected).unwrap_or_else(|error| {
                panic!("fixture {name} store query {index}: the recorded result is not JSON: {error}")
            });
            assert_field(name, &format!("store[{index}]"), &actual, &expected);
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
                    let value = serde_json::from_str::<Value>(&item.json()).expect("a log item is JSON");
                    body.push_str(&frame(&value));
                }
            }
            bodies.insert(after.map_or_else(|| "-1".to_string(), |value| value.to_string()), Value::String(body));
        }
        log.insert(session.clone(), Value::Object(bodies));
    }

    let snapshot = database.read(|connection| dump(connection, &masks)).expect("dump the projected database");

    let frames = Value::Object(
        subscriptions
            .iter_mut()
            .map(|(key, subscription)| {
                let mut frames = Vec::new();
                while let Some(item) = subscription.try_recv() {
                    match item {
                        Ok(frame) => frames.push(Value::String(frame)),
                        Err(FeedError::SubscriberOverflow { .. }) => {
                            panic!("fixture {name}: subscriber {key} overflowed")
                        }
                        Err(error) => panic!("fixture {name}: subscriber {key} failed: {error}"),
                    }
                }
                (key.clone(), Value::Array(frames))
            })
            .collect(),
    );
    let usage = Value::Array(usage.lock().expect("lock the usage sink").iter().cloned().map(Value::String).collect());

    assert_field(name, "frames", &frames, &fixture["frames"]);
    assert_field(name, "usage", &usage, &fixture["usage"]);
    assert_field(name, "log", &Value::Object(log), &fixture["log"]);
    assert_field(name, "dump", &snapshot, &fixture["dump"]);
}

fn produces_usage(database: &Arc<Database>, event: &Event) -> bool {
    let r#type = event.r#type();
    if !matches!(r#type, "session.step.ended" | "session.step.failed" | "session.usage.recorded") {
        return false;
    }
    let data = event.data();
    if r#type == "session.step.failed"
        && (data.get("cost").is_none_or(Value::is_null) || data.get("tokens").is_none_or(Value::is_null))
    {
        return false;
    }
    let Some(session_id) = data.get("sessionID").and_then(Value::as_str) else {
        return false;
    };
    database
        .read(|connection| {
            Ok(connection
                .query_row("SELECT 1 FROM session_v2 WHERE id = ?1", [session_id], |_| Ok(()))
                .optional()?
                .is_some())
        })
        .expect("read the session row for the usage count")
}

fn run_store_query(store: &SessionStore, fixture: &str, query: &Value) -> String {
    let kind = query["kind"].as_str().unwrap_or_else(|| panic!("fixture {fixture}: a store query carries no kind"));
    let input = query.get("input").unwrap_or(&Value::Null);
    match kind {
        "get" => match store.get(&required_string(query, "sessionID", fixture)) {
            Ok(Some(info)) => encode_json(&info),
            Ok(None) => "null".to_string(),
            Err(_) => database_error(),
        },
        "list" => match store.list(&list_input(input, fixture)) {
            Ok(values) => encode_json(&values),
            Err(_) => database_error(),
        },
        "messages" => match store.messages(&messages_input(input, fixture)) {
            Ok(values) => encode_json(&values),
            Err(error) => tagged_error(error.tag()),
        },
        "messagesWithRefs" => match store.messages_with_refs(&messages_input(input, fixture)) {
            Ok(values) => encode_json(&values),
            Err(error) => tagged_error(error.tag()),
        },
        "message" => match store.message(&required_string(query, "messageID", fixture)) {
            Ok(Some((session_id, message))) => {
                format!("{{\"sessionID\":{},\"message\":{}}}", encode_json(&session_id), encode_json(&message))
            }
            Ok(None) => "null".to_string(),
            Err(error) => tagged_error(error.tag()),
        },
        "messageWithRefs" => match store.message_with_refs(&required_string(query, "messageID", fixture)) {
            Ok(Some((session_id, message))) => {
                format!("{{\"sessionID\":{},\"message\":{}}}", encode_json(&session_id), encode_json(&message))
            }
            Ok(None) => "null".to_string(),
            Err(error) => tagged_error(error.tag()),
        },
        "lastAssistantAfter" => match store.last_assistant_after(&last_assistant_input(input, fixture)) {
            Ok(Some(assistant)) => encode_json(&assistant),
            Ok(None) => "null".to_string(),
            Err(error) => tagged_error(error.tag()),
        },
        "listSuspended" => match store.list_suspended() {
            Ok(ids) => encode_json(&ids),
            Err(_) => database_error(),
        },
        "claim" => match store.claim(&required_string(query, "sessionID", fixture)) {
            Ok(()) => "null".to_string(),
            Err(_) => database_error(),
        },
        "release" => match store.release(&required_string(query, "sessionID", fixture)) {
            Ok(()) => "null".to_string(),
            Err(_) => database_error(),
        },
        "releaseOrphanedChildClaims" => match store.release_orphaned_child_claims() {
            Ok(()) => "null".to_string(),
            Err(_) => database_error(),
        },
        "countResume" => match store.count_resume(&required_string(query, "sessionID", fixture)) {
            Ok(Some(count)) => count.to_string(),
            Ok(None) => "null".to_string(),
            Err(_) => database_error(),
        },
        kind => panic!("fixture {fixture}: unknown store query kind {kind}"),
    }
}

fn database_error() -> String {
    tagged_error("Database")
}

fn tagged_error(tag: &str) -> String {
    format!("{{\"error\":\"{tag}\"}}")
}

fn list_input(input: &Value, fixture: &str) -> ListInput {
    ListInput {
        directory: optional_string(input, "directory", fixture),
        workspace_id: optional_string(input, "workspaceID", fixture),
        project: optional_string(input, "project", fixture),
        subpath: optional_string(input, "subpath", fixture),
        search: optional_string(input, "search", fixture),
        limit: optional_i64(input, "limit", fixture),
        order: optional_order(input, fixture),
        parent_id: match input.get("parentID") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(parent_id)) => Some(Some(parent_id.clone())),
            Some(_) => panic!("fixture {fixture}: parentID must be a string or null"),
        },
        archived: optional_bool(input, "archived", fixture),
        anchor: match input.get("anchor") {
            None | Some(Value::Null) => None,
            Some(anchor) => Some(ListAnchor {
                id: required_string(anchor, "id", fixture),
                time: anchor
                    .get("time")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| panic!("fixture {fixture}: the anchor carries no time")),
                direction: direction(anchor.get("direction"), fixture),
            }),
        },
    }
}

fn messages_input(input: &Value, fixture: &str) -> MessagesInput {
    MessagesInput {
        session_id: required_string(input, "sessionID", fixture),
        limit: optional_i64(input, "limit", fixture),
        order: optional_order(input, fixture),
        r#type: optional_string(input, "type", fixture),
        cursor: match input.get("cursor") {
            None | Some(Value::Null) => None,
            Some(cursor) => Some(Cursor {
                id: required_string(cursor, "id", fixture),
                direction: direction(cursor.get("direction"), fixture),
            }),
        },
    }
}

fn last_assistant_input(input: &Value, fixture: &str) -> LastAssistantInput {
    LastAssistantInput {
        session_id: required_string(input, "sessionID", fixture),
        after_seq: optional_i64(input, "afterSeq", fixture),
    }
}

fn required_string(value: &Value, key: &str, fixture: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {fixture}: the store query carries no string {key}"))
        .to_string()
}

fn optional_string(value: &Value, key: &str, fixture: &str) -> Option<String> {
    match value.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => panic!("fixture {fixture}: {key} must be a string"),
    }
}

fn optional_i64(value: &Value, key: &str, fixture: &str) -> Option<i64> {
    match value.get(key) {
        None | Some(Value::Null) => None,
        Some(number) => Some(number.as_i64().unwrap_or_else(|| panic!("fixture {fixture}: {key} must be an integer"))),
    }
}

fn optional_bool(value: &Value, key: &str, fixture: &str) -> Option<bool> {
    match value.get(key) {
        None | Some(Value::Null) => None,
        Some(flag) => Some(flag.as_bool().unwrap_or_else(|| panic!("fixture {fixture}: {key} must be a boolean"))),
    }
}

fn optional_order(value: &Value, fixture: &str) -> Option<Order> {
    match value.get("order") {
        None | Some(Value::Null) => None,
        Some(order) => Some(order_value(order, fixture)),
    }
}

fn order_value(value: &Value, fixture: &str) -> Order {
    match value.as_str() {
        Some("asc") => Order::Asc,
        Some("desc") => Order::Desc,
        _ => panic!("fixture {fixture}: {value} is not a valid order"),
    }
}

fn direction(value: Option<&Value>, fixture: &str) -> Direction {
    match value.and_then(Value::as_str) {
        Some("previous") => Direction::Previous,
        Some("next") => Direction::Next,
        _ => panic!("fixture {fixture}: the store query carries no valid direction"),
    }
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
