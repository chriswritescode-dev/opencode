use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use opencode_bus::{Bus, BusError, BusOptions, Definition, DurableSpec, PublishInput, PublishOptions};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::json;

use crate::TempDir;

fn setup(persist: bool) -> (TempDir, Arc<Database>, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions { persist, ..Default::default() });
    (dir, database, bus)
}

fn durable() -> Definition {
    Definition::durable("test.counted", 1, "id")
}

fn row(database: &Database, id: &str) -> (String, String, i64, i64) {
    database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT type, aggregate_id, seq, created FROM event WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        })
        .expect("read the event row")
}

fn row_count(database: &Database, table: &str) -> i64 {
    database
        .read(|connection| Ok(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?))
        .expect("count the rows")
}

fn sequence(database: &Database, aggregate_id: &str) -> (i64, Option<String>) {
    database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT seq, owner_id FROM event_sequence WHERE aggregate_id = ?1",
                [aggregate_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .expect("read the sequence")
}

fn stored_sequences(database: &Database, aggregate_id: &str) -> Vec<i64> {
    database
        .read(|connection| {
            let mut statement = connection.prepare("SELECT seq FROM event WHERE aggregate_id = ?1 ORDER BY seq")?;
            let sequences = statement.query_map([aggregate_id], |row| row.get(0))?.collect::<Result<Vec<i64>, _>>()?;
            Ok(sequences)
        })
        .expect("read the stored sequences")
}

fn create_probe(database: &Database) {
    database
        .read(|connection| {
            connection.execute_batch("CREATE TABLE probe (value INTEGER)")?;
            Ok(())
        })
        .expect("create the probe table");
}

#[test]
fn definitions_resolve_the_durable_manifest() {
    let definition = Definition::durable_lookup("session.created.1").expect("the manifest has the event");
    assert_eq!(definition.r#type, "session.created", "the lookup returns the base type");
    assert_eq!(
        definition.durable,
        Some(DurableSpec { version: 1, aggregate: "sessionID" }),
        "the lookup returns the manifest version and aggregate"
    );
    assert_eq!(Definition::durable_lookup("session.created.2"), None, "a missing version is not resolved");
    assert_eq!(Definition::durable_lookup("unknown.event.1"), None, "an unknown type is not resolved");
}

#[test]
fn publish_sequences_each_aggregate() {
    let (_dir, database, bus) = setup(true);
    let first = bus
        .publish(&durable(), json!({ "id": "agg", "value": 1 }), PublishOptions::default())
        .expect("publish the first event");
    let second = bus
        .publish(&durable(), json!({ "id": "agg", "value": 2 }), PublishOptions::default())
        .expect("publish the second event");
    let other = bus
        .publish(&durable(), json!({ "id": "other", "value": 3 }), PublishOptions::default())
        .expect("publish to another aggregate");
    assert_eq!(first.durable().expect("a durable envelope").seq, 0, "the first event takes sequence 0");
    assert_eq!(second.durable().expect("a durable envelope").seq, 1, "the sequence increments per aggregate");
    assert_eq!(other.durable().expect("a durable envelope").seq, 0, "another aggregate starts at 0");
    assert_eq!(sequence(&database, "agg"), (1, None), "the aggregate keeps its latest sequence");
}

#[test]
fn publish_persists_the_versioned_row() {
    let (_dir, database, bus) = setup(true);
    let event = bus
        .publish(&durable(), json!({ "id": "agg", "value": 1 }), PublishOptions::default())
        .expect("publish the event");
    let (r#type, aggregate_id, seq, created) = row(&database, event.id());
    assert_eq!(r#type, "test.counted.1", "the row stores the versioned type");
    assert_eq!(aggregate_id, "agg", "the row stores the aggregate");
    assert_eq!(seq, 0, "the row stores the sequence");
    assert_eq!(created, event.created(), "the row stores the creation time");
    assert_eq!(event.durable().expect("a durable envelope").version, 1, "the envelope carries the version");
}

#[test]
fn publish_without_persist_skips_the_event_row() {
    let (_dir, database, bus) = setup(false);
    let event = bus
        .publish(&durable(), json!({ "id": "agg", "value": 1 }), PublishOptions::default())
        .expect("publish the event");
    assert_eq!(event.durable().expect("a durable envelope").seq, 0, "the envelope still carries the sequence");
    assert_eq!(row_count(&database, "event"), 0, "no payload row is stored");
    assert_eq!(sequence(&database, "agg"), (0, None), "the sequence is reserved without an owner");
}

#[test]
fn publish_runs_projectors_before_commit_hooks_and_listeners() {
    let (_dir, database, bus) = setup(true);
    create_probe(&database);
    let order = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::new(Mutex::new(None::<i64>));

    let projector_order = order.clone();
    bus.project(
        "test.counted.1",
        Box::new(move |connection, _event| {
            connection.execute("INSERT INTO probe (value) VALUES (1)", [])?;
            projector_order.lock().expect("lock the order").push("projector".to_string());
            Ok(())
        }),
    );

    let listener_order = order.clone();
    let _listener = bus.listen(Box::new(move |_event| {
        listener_order.lock().expect("lock the order").push("listener".to_string());
        Ok(())
    }));

    let seen = observed.clone();
    bus.publish(
        &durable(),
        json!({ "id": "agg", "value": 1 }),
        PublishOptions {
            commit: Some(Box::new(move |connection, _seq| {
                let count: i64 = connection.query_row("SELECT COUNT(*) FROM probe", [], |row| row.get(0))?;
                *seen.lock().expect("lock the observation") = Some(count);
                Ok(())
            })),
            ..Default::default()
        },
    )
    .expect("publish the event");

    assert_eq!(*observed.lock().expect("lock the observation"), Some(1), "the hook sees the projected row");
    assert_eq!(
        *order.lock().expect("lock the order"),
        vec!["projector".to_string(), "listener".to_string()],
        "the projector runs before the listener"
    );
}

#[test]
fn publish_rolls_back_when_a_commit_hook_fails() {
    let (_dir, database, bus) = setup(true);
    create_probe(&database);
    bus.project(
        "test.counted.1",
        Box::new(|connection, _event| {
            connection.execute("INSERT INTO probe (value) VALUES (1)", [])?;
            Ok(())
        }),
    );
    let error = bus
        .publish(
            &durable(),
            json!({ "id": "agg", "value": 1 }),
            PublishOptions {
                commit: Some(Box::new(|_connection, _seq| {
                    Err(BusError::InvalidDurableEvent {
                        r#type: "test.counted".to_string(),
                        message: "stop".to_string(),
                    })
                })),
                ..Default::default()
            },
        )
        .expect_err("the commit hook fails");
    assert_eq!(error.to_string(), "stop", "the hook error is returned");
    assert_eq!(row_count(&database, "probe"), 0, "the projected row is rolled back");
    assert_eq!(row_count(&database, "event"), 0, "the event row is rolled back");
    assert_eq!(row_count(&database, "event_sequence"), 0, "the sequence row is rolled back");
}

#[test]
fn publish_rejects_a_commit_hook_on_an_ephemeral_event() {
    let (_dir, _database, bus) = setup(true);
    let error = bus
        .publish(
            &Definition::ephemeral("test.message"),
            json!({ "id": "agg" }),
            PublishOptions { commit: Some(Box::new(|_connection, _seq| Ok(()))), ..Default::default() },
        )
        .expect_err("the hook is rejected");
    assert!(matches!(error, BusError::InvalidDurableEvent { .. }), "the error is invalid durable event: {error:?}");
    assert_eq!(error.to_string(), "Local commit hooks require a durable event");
}

#[test]
fn publish_all_commits_the_batch_atomically_and_in_order() {
    let (_dir, database, bus) = setup(true);
    let projected = Arc::new(Mutex::new(Vec::new()));
    let projector_order = projected.clone();
    bus.project(
        "test.counted.1",
        Box::new(move |_connection, event| {
            projector_order.lock().expect("lock the projector order").push(event.id().to_string());
            Ok(())
        }),
    );

    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener_database = database.clone();
    let listener_seen = seen.clone();
    let _listener = bus.listen(Box::new(move |event| {
        let seq: i64 = listener_database.read(|connection| {
            Ok(connection
                .query_row("SELECT seq FROM event_sequence WHERE aggregate_id = ?1", ["agg"], |row| row.get(0))?)
        })?;
        listener_seen.lock().expect("lock the listener order").push((event.id().to_string(), seq));
        Ok(())
    }));

    let events = bus
        .publish_all(vec![
            PublishInput {
                definition: durable(),
                data: json!({ "id": "agg", "value": 1 }),
                options: PublishOptions { id: Some("evt_first".to_string()), ..Default::default() },
            },
            PublishInput {
                definition: durable(),
                data: json!({ "id": "agg", "value": 2 }),
                options: PublishOptions { id: Some("evt_second".to_string()), ..Default::default() },
            },
        ])
        .expect("publish the batch");

    assert_eq!(
        events.iter().map(|event| event.durable().expect("a durable envelope").seq).collect::<Vec<_>>(),
        vec![0, 1],
        "the returned events carry consecutive sequences"
    );
    assert_eq!(
        *projected.lock().expect("lock the projector order"),
        vec!["evt_first".to_string(), "evt_second".to_string()],
        "projectors run in batch order"
    );
    assert_eq!(
        *seen.lock().expect("lock the listener order"),
        vec![("evt_first".to_string(), 1), ("evt_second".to_string(), 1)],
        "listeners see the final sequence after the commit"
    );
    assert_eq!(stored_sequences(&database, "agg"), vec![0, 1], "both rows are stored in order");
}

#[test]
fn publish_keeps_going_when_a_listener_fails() {
    let (_dir, database, bus) = setup(true);
    let later = Arc::new(AtomicBool::new(false));
    let _first = bus.listen(Box::new(|_event| {
        Err(BusError::InvalidDurableEvent {
            r#type: "test.counted".to_string(),
            message: "listener failed".to_string(),
        })
    }));
    let ran = later.clone();
    let _second = bus.listen(Box::new(move |_event| {
        ran.store(true, Ordering::Relaxed);
        Ok(())
    }));
    bus.publish(&durable(), json!({ "id": "agg", "value": 1 }), PublishOptions::default())
        .expect("a durable listener failure does not fail the publish");
    assert!(later.load(Ordering::Relaxed), "the later listener still runs");
    assert_eq!(row_count(&database, "event"), 1, "the event stays committed");
}

#[test]
fn publish_delivers_to_typed_and_all_subscribers() {
    let (_dir, _database, bus) = setup(true);
    let mut typed = bus.subscribe(&["test.counted"], None);
    let mut all = bus.subscribe_all(None);
    let counted = bus
        .publish(&durable(), json!({ "id": "agg", "value": 1 }), PublishOptions::default())
        .expect("publish the counted event");
    let message = bus
        .publish(
            &Definition::ephemeral("test.message"),
            json!({ "id": "agg", "text": "hello" }),
            PublishOptions::default(),
        )
        .expect("publish the message");
    assert_eq!(typed.try_recv().as_deref(), Some(&counted), "the typed subscriber receives the counted event");
    assert_eq!(typed.try_recv(), None, "the typed subscriber receives nothing else");
    assert_eq!(all.try_recv().as_deref(), Some(&counted), "the all subscriber receives the counted event");
    assert_eq!(all.try_recv().as_deref(), Some(&message), "the all subscriber receives the message");
    assert_eq!(all.try_recv(), None, "the all subscriber has nothing left");
}
