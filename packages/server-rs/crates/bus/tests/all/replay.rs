use std::sync::{Arc, Mutex};

use opencode_bus::{
    Bus, BusOptions, Definition, Event, EventId, ListenerHandle, PublishOptions, ReplayOptions, SerializedEvent,
};
use opencode_storage::{Database, DatabaseLocation};
use rusqlite::params;
use serde_json::{Value, json};

use crate::TempDir;

fn setup(persist: bool) -> (TempDir, Arc<Database>, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions { persist, ..Default::default() });
    (dir, database, bus)
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

fn session_id() -> String {
    format!("ses_{}", opencode_storage::id::ascending())
}

fn renamed(session: &str, title: &str) -> Value {
    json!({ "sessionID": session, "title": title })
}

fn renamed_definition() -> Definition {
    Definition::durable("session.renamed", 1, "sessionID")
}

fn serialized(r#type: &str, id: &str, aggregate: &str, seq: i64, data: Value) -> SerializedEvent {
    SerializedEvent {
        id: id.to_string(),
        r#type: r#type.to_string(),
        created: Some(0),
        seq,
        aggregate_id: aggregate.to_string(),
        data,
    }
}

fn capture(bus: &Bus, versioned: &str) -> Arc<Mutex<Vec<Event>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let captured = received.clone();
    bus.project(
        versioned,
        Box::new(move |_connection, event| {
            captured.lock().expect("lock the projector events").push(event.clone());
            Ok(())
        }),
    );
    received
}

fn listen(bus: &Bus) -> (ListenerHandle, Arc<Mutex<Vec<Event>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let captured = received.clone();
    let handle = bus.listen(Box::new(move |event| {
        captured.lock().expect("lock the listened events").push(event.clone());
        Ok(())
    }));
    (handle, received)
}

#[test]
fn replay_runs_projectors_and_inserts_rows() {
    let (_dir, database, bus) = setup(true);
    let received = capture(&bus, "session.renamed.1");
    let session = session_id();
    let id = EventId::create();
    bus.replay(serialized("session.renamed.1", &id, &session, 0, renamed(&session, "hello")), ReplayOptions::default())
        .expect("replay the event");

    let events = received.lock().expect("lock the projector events");
    assert_eq!(events.len(), 1, "the projector runs once");
    assert_eq!(events[0].r#type(), "session.renamed", "the projector sees the base type");
    assert_eq!(events[0].data(), &renamed(&session, "hello"), "the projector sees the canonical data");
    let durable = events[0].durable().expect("a durable envelope");
    assert_eq!((durable.seq, durable.version), (0, 1), "the projector sees the durable envelope");
    drop(events);

    assert_eq!(
        row(&database, &id),
        ("session.renamed.1".to_string(), session.clone(), 0, 0),
        "the replay inserts the versioned row"
    );
    assert_eq!(sequence(&database, &session), (0, None), "the replay reserves the sequence");
}

#[test]
fn replay_drops_the_legacy_recent_field_and_is_idempotent() {
    let (_dir, _database, bus) = setup(true);
    let received = capture(&bus, "session.compaction.ended.1");
    let session = session_id();
    let id = EventId::create();
    let data = json!({ "sessionID": &session, "reason": "auto", "text": "summary", "recent": "[User]: kept" });

    bus.replay(serialized("session.compaction.ended.1", &id, &session, 0, data.clone()), ReplayOptions::default())
        .expect("replay the legacy event");
    bus.replay(serialized("session.compaction.ended.1", &id, &session, 0, data), ReplayOptions::default())
        .expect("replay the same event again");

    let events = received.lock().expect("lock the projector events");
    assert_eq!(events.len(), 1, "the stale duplicate does not project again");
    assert_eq!(
        events[0].data(),
        &json!({ "sessionID": &session, "reason": "auto", "text": "summary" }),
        "the projector data drops the legacy recent field"
    );
}

#[test]
fn replay_compares_stored_rows_in_their_upgraded_shape() {
    let (_dir, database, bus) = setup(true);
    let received = capture(&bus, "session.compaction.ended.1");
    let session = session_id();
    let id = EventId::create();
    let legacy = json!({ "sessionID": &session, "reason": "auto", "text": "summary", "recent": "[User]: kept" });
    database
        .read(|connection| {
            connection
                .execute("INSERT INTO event_sequence (aggregate_id, seq, owner_id) VALUES (?1, 0, NULL)", [&session])?;
            connection.execute(
                "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, 0, 0, ?3, ?4)",
                params![
                    &id,
                    &session,
                    "session.compaction.ended.1",
                    serde_json::to_string(&legacy).expect("serialize the stored data")
                ],
            )?;
            Ok(())
        })
        .expect("insert the stored legacy row");

    bus.replay(
        serialized(
            "session.compaction.ended.1",
            &id,
            &session,
            0,
            json!({ "sessionID": &session, "reason": "auto", "text": "summary" }),
        ),
        ReplayOptions { owner_id: Some("owner-a".to_string()), ..Default::default() },
    )
    .expect("the stale replay matches its upgraded row");

    assert!(received.lock().expect("lock the projector events").is_empty(), "the projector never runs");
    assert_eq!(
        sequence(&database, &session),
        (0, Some("owner-a".to_string())),
        "the replay claims the unowned sequence"
    );
    assert_eq!(row_count(&database, "event"), 1, "no row is inserted");
}

#[test]
fn replay_rejects_an_aggregate_mismatch_without_mutating_the_payload_aggregate() {
    let (_dir, database, bus) = setup(true);
    let payload = session_id();
    let envelope = session_id();
    bus.publish(&renamed_definition(), renamed(&payload, "seed"), PublishOptions::default()).expect("publish the seed");
    let received = capture(&bus, "session.renamed.1");

    let error = bus
        .replay(
            serialized("session.renamed.1", &EventId::create(), &envelope, 1, renamed(&payload, "replayed")),
            ReplayOptions::default(),
        )
        .expect_err("the aggregate mismatch is rejected");

    assert!(error.to_string().contains("Aggregate mismatch"), "the error reports the mismatch: {error}");
    assert!(received.lock().expect("lock the projector events").is_empty(), "the projector never runs");
    assert_eq!(stored_sequences(&database, &payload).len(), 1, "the payload aggregate keeps its row");
    assert_eq!(sequence(&database, &payload), (0, None), "the payload aggregate keeps its sequence");
}

#[test]
fn replay_rejects_a_sequence_gap() {
    let (_dir, _database, bus) = setup(true);
    let session = session_id();
    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "first")),
        ReplayOptions::default(),
    )
    .expect("replay the first event");

    let error = bus
        .replay(
            serialized("session.renamed.1", &EventId::create(), &session, 5, renamed(&session, "bad")),
            ReplayOptions::default(),
        )
        .expect_err("the gap is rejected");

    assert!(error.to_string().contains("Sequence mismatch"), "the error reports the mismatch: {error}");
}

#[test]
fn replay_rejects_an_unknown_event_type() {
    let (_dir, _database, bus) = setup(true);
    let aggregate = session_id();
    let error = bus
        .replay(serialized("unknown.event.1", &EventId::create(), &aggregate, 0, json!({})), ReplayOptions::default())
        .expect_err("the unknown type is rejected");

    assert!(
        error.to_string().contains("Unknown durable event type unknown.event.1"),
        "the error names the type: {error}"
    );
}

#[test]
fn replay_dispatches_projectors_by_exact_version() {
    let (_dir, _database, bus) = setup(true);
    let first = capture(&bus, "session.renamed.1");
    let second = capture(&bus, "session.renamed.2");
    let session = session_id();

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "hello")),
        ReplayOptions::default(),
    )
    .expect("replay the v1 event");

    assert_eq!(first.lock().expect("lock the v1 projector events").len(), 1, "the v1 projector runs");
    assert!(second.lock().expect("lock the v2 projector events").is_empty(), "the v2 projector never runs");
}

#[test]
fn replay_upgrades_legacy_content_parts_before_projection() {
    let (_dir, _database, bus) = setup(true);
    let received = capture(&bus, "session.message.content.updated.1");
    let session = session_id();
    let message = format!("msg_{}", opencode_storage::id::ascending());

    bus.replay(
        serialized(
            "session.message.content.updated.1",
            &EventId::create(),
            &session,
            0,
            json!({
                "sessionID": &session,
                "messageID": &message,
                "content": [{ "type": "text", "text": "hello", "state": { "x": 1 } }],
            }),
        ),
        ReplayOptions::default(),
    )
    .expect("replay the legacy content event");

    let events = received.lock().expect("lock the projector events");
    assert_eq!(
        events[0].data()["content"][0],
        json!({ "type": "text", "text": "hello", "native": { "x": 1 } }),
        "the legacy state becomes native"
    );
}

#[test]
fn replay_claim_fences_other_owners() {
    let (_dir, database, bus) = setup(true);
    let session = session_id();
    bus.publish(&renamed_definition(), renamed(&session, "seed"), PublishOptions::default()).expect("publish the seed");
    bus.claim(&session, "owner-a").expect("claim the aggregate");
    let received = capture(&bus, "session.renamed.1");

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 1, renamed(&session, "ignored")),
        ReplayOptions { owner_id: Some("owner-b".to_string()), ..Default::default() },
    )
    .expect("the fenced replay is skipped");

    assert!(received.lock().expect("lock the projector events").is_empty(), "the projector never runs");
    assert_eq!(stored_sequences(&database, &session).len(), 1, "no row is inserted");
    assert_eq!(sequence(&database, &session), (0, Some("owner-a".to_string())), "the claim is unchanged");
}

#[test]
fn replay_strict_owner_fences_an_exact_replay() {
    let (_dir, _database, bus) = setup(true);
    let session = session_id();
    let id = EventId::create();
    bus.replay(
        serialized("session.renamed.1", &id, &session, 0, renamed(&session, "owned")),
        ReplayOptions { owner_id: Some("owner-a".to_string()), ..Default::default() },
    )
    .expect("replay the owned event");

    let error = bus
        .replay(
            serialized("session.renamed.1", &id, &session, 0, renamed(&session, "owned")),
            ReplayOptions { owner_id: Some("owner-b".to_string()), strict_owner: true, ..Default::default() },
        )
        .expect_err("the owner conflict is rejected");

    assert!(error.to_string().contains("Replay owner mismatch"), "the error reports the mismatch: {error}");
}

#[test]
fn replay_claims_an_unowned_sequence() {
    let (_dir, database, bus) = setup(true);
    let session = session_id();

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "owned")),
        ReplayOptions { owner_id: Some("owner-1".to_string()), ..Default::default() },
    )
    .expect("replay the event");

    assert_eq!(sequence(&database, &session), (0, Some("owner-1".to_string())), "the replay claims the sequence");
}

#[test]
fn replay_claims_an_existing_unowned_sequence_before_fencing_another_owner() {
    let (_dir, database, bus) = setup(true);
    let session = session_id();
    bus.publish(&renamed_definition(), renamed(&session, "local"), PublishOptions::default())
        .expect("publish the seed");

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 1, renamed(&session, "claimed")),
        ReplayOptions { owner_id: Some("owner-1".to_string()), ..Default::default() },
    )
    .expect("replay the claiming event");
    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 2, renamed(&session, "fenced")),
        ReplayOptions { owner_id: Some("owner-2".to_string()), ..Default::default() },
    )
    .expect("the fenced replay is skipped");

    assert_eq!(stored_sequences(&database, &session), vec![0, 1], "only the accepted row is stored");
    assert_eq!(
        sequence(&database, &session),
        (1, Some("owner-1".to_string())),
        "the claiming replay owns the sequence"
    );
}

#[test]
fn replay_strict_owner_rejects_an_owner_conflict() {
    let (_dir, _database, bus) = setup(true);
    let session = session_id();
    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "claimed")),
        ReplayOptions { owner_id: Some("owner-1".to_string()), ..Default::default() },
    )
    .expect("replay the owned event");

    let error = bus
        .replay(
            serialized("session.renamed.1", &EventId::create(), &session, 1, renamed(&session, "conflict")),
            ReplayOptions { owner_id: Some("owner-2".to_string()), strict_owner: true, ..Default::default() },
        )
        .expect_err("the owner conflict is rejected");

    assert!(error.to_string().contains("Replay owner mismatch"), "the error reports the mismatch: {error}");
}

#[test]
fn replay_publishes_an_accepted_event_once_and_suppresses_a_stale_duplicate() {
    let (_dir, _database, bus) = setup(true);
    let (_listener, received) = listen(&bus);
    let session = session_id();
    let id = EventId::create();

    bus.replay(
        serialized("session.renamed.1", &id, &session, 0, renamed(&session, "replayed")),
        ReplayOptions { publish: true, ..Default::default() },
    )
    .expect("replay the event");
    bus.replay(
        serialized("session.renamed.1", &id, &session, 0, renamed(&session, "replayed")),
        ReplayOptions { publish: true, ..Default::default() },
    )
    .expect("replay the same event again");

    let events = received.lock().expect("lock the listened events");
    assert_eq!(events.len(), 1, "only the accepted replay is published");
    assert_eq!(events[0].id(), id, "the listener sees the replayed event");
    let durable = events[0].durable().expect("a durable envelope");
    assert_eq!((durable.seq, durable.version), (0, 1), "the listener sees the durable envelope");
    assert_eq!(events[0].data(), &renamed(&session, "replayed"), "the listener sees the canonical data");
}

#[test]
fn replay_rejects_a_divergent_stale_replay_without_publishing() {
    let (_dir, _database, bus) = setup(true);
    let (_listener, received) = listen(&bus);
    let session = session_id();
    let id = EventId::create();

    bus.replay(
        serialized("session.renamed.1", &id, &session, 0, renamed(&session, "original")),
        ReplayOptions { publish: true, ..Default::default() },
    )
    .expect("replay the event");
    let error = bus
        .replay(
            serialized("session.renamed.1", &id, &session, 0, renamed(&session, "divergent")),
            ReplayOptions { publish: true, ..Default::default() },
        )
        .expect_err("the divergence is rejected");

    assert!(error.to_string().contains("Replay diverged"), "the error reports the divergence: {error}");
    assert_eq!(received.lock().expect("lock the listened events").len(), 1, "only the accepted replay is published");
}

#[test]
fn replay_rejects_an_event_id_reused_at_another_sequence() {
    let (_dir, _database, bus) = setup(true);
    let session = session_id();
    let id = EventId::create();

    bus.replay(serialized("session.renamed.1", &id, &session, 0, renamed(&session, "first")), ReplayOptions::default())
        .expect("replay the first event");
    let error = bus
        .replay(
            serialized("session.renamed.1", &id, &session, 1, renamed(&session, "second")),
            ReplayOptions::default(),
        )
        .expect_err("the reused id is rejected");

    assert!(error.to_string().contains(&format!("Event {id} already exists")), "the error names the event: {error}");
}

#[test]
fn replay_from_a_different_owner_changes_and_publishes_nothing() {
    let (_dir, database, bus) = setup(true);
    let (_listener, received) = listen(&bus);
    let session = session_id();

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "first")),
        ReplayOptions { owner_id: Some("owner-1".to_string()), ..Default::default() },
    )
    .expect("replay the owned event");
    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 1, renamed(&session, "ignored")),
        ReplayOptions { publish: true, owner_id: Some("owner-2".to_string()), ..Default::default() },
    )
    .expect("the fenced replay is skipped");

    assert_eq!(stored_sequences(&database, &session), vec![0], "the fenced replay inserts no row");
    assert_eq!(sequence(&database, &session), (0, Some("owner-1".to_string())), "the claim is unchanged");
    assert!(received.lock().expect("lock the listened events").is_empty(), "the fenced replay publishes nothing");
}

#[test]
fn replay_after_remove_starts_a_new_sequence() {
    let (_dir, _database, bus) = setup(true);
    let session = session_id();
    bus.publish(&renamed_definition(), renamed(&session, "seed"), PublishOptions::default()).expect("publish the seed");
    bus.remove(&session).expect("remove the aggregate");
    let received = capture(&bus, "session.renamed.1");

    bus.replay(
        serialized("session.renamed.1", &EventId::create(), &session, 0, renamed(&session, "replayed")),
        ReplayOptions::default(),
    )
    .expect("replay after the removal");

    let events = received.lock().expect("lock the projector events");
    assert_eq!(events[0].data(), &renamed(&session, "replayed"), "the projector sees the replayed data");
}
