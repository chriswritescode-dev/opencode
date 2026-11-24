use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use opencode_bus::{
    Bus, BusError, BusOptions, Definition, Event, EventId, LogInput, LogItem, PublishInput, PublishOptions,
    ReplayOptions, SerializedEvent, Subscription,
};
use opencode_contract::generated::{AbsolutePath, LocationRef, WorkspaceID};
use opencode_storage::{Database, DatabaseLocation};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::TempDir;

const SESSION: &str = "ses_routing";
const PROJECT: &str = "prj_global";

fn setup() -> (TempDir, Arc<Database>, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions { persist: true, ..Default::default() });
    (dir, database, bus)
}

fn location(directory: &str) -> LocationRef {
    LocationRef { directory: AbsolutePath(directory.to_string()), workspace_id: None }
}

fn base() -> LocationRef {
    location("/a")
}

fn other() -> LocationRef {
    location("/b")
}

fn other_workspace() -> LocationRef {
    LocationRef { directory: AbsolutePath("/a".to_string()), workspace_id: Some(WorkspaceID("wrk_other".to_string())) }
}

fn location_json(reference: &LocationRef) -> Value {
    match &reference.workspace_id {
        Some(workspace) => {
            json!({ "directory": reference.directory.0.clone(), "workspaceID": workspace.0.clone() })
        }
        None => json!({ "directory": reference.directory.0.clone() }),
    }
}

fn seed(database: &Database, reference: &LocationRef) {
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES (?1, ?2, '[]', 0, 0)",
                params![PROJECT, reference.directory.0.clone()],
            )?;
            connection.execute(
                "INSERT INTO session_v2 \
                 (id, project_id, workspace_id, slug, directory, version, time_created, time_updated) \
                 VALUES (?1, ?2, ?3, 'routing', ?4, 'test', 0, 0)",
                params![
                    SESSION,
                    PROJECT,
                    reference.workspace_id.as_ref().map(|workspace| workspace.0.clone()),
                    reference.directory.0.clone()
                ],
            )?;
            Ok(())
        })
        .expect("seed the session");
}

fn renamed_definition() -> Definition {
    Definition::durable("session.renamed", 1, "sessionID")
}

fn moved_definition() -> Definition {
    Definition::durable("session.moved", 1, "sessionID")
}

fn created_definition() -> Definition {
    Definition::durable("session.created", 1, "sessionID")
}

fn forked_definition() -> Definition {
    Definition::durable("session.forked", 2, "sessionID")
}

fn deleted_definition() -> Definition {
    Definition::durable("session.deleted", 2, "sessionID")
}

fn succeeded_definition() -> Definition {
    Definition::durable("session.execution.succeeded", 1, "sessionID")
}

fn delta_definition() -> Definition {
    Definition::ephemeral("session.text.delta")
}

fn global_definition() -> Definition {
    Definition::ephemeral("test.routing.global")
}

fn done_definition() -> Definition {
    Definition::ephemeral("test.routing.done")
}

fn renamed_data(title: &str) -> Value {
    json!({ "sessionID": SESSION, "title": title })
}

fn moved_data(reference: &LocationRef) -> Value {
    json!({ "sessionID": SESSION, "location": location_json(reference), "projectID": PROJECT })
}

fn delta_data() -> Value {
    json!({
        "sessionID": SESSION,
        "assistantMessageID": "msg_routing",
        "ordinal": 0,
        "delta": "text"
    })
}

fn publish(bus: &Bus, definition: &Definition, data: Value) -> Event {
    bus.publish(definition, data, PublishOptions::default()).expect("publish the event")
}

fn drain(subscription: &mut Subscription) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = subscription.try_recv() {
        events.push((*event).clone());
    }
    events
}

fn ids(events: &[Event]) -> Vec<String> {
    events.iter().map(|event| event.id().to_string()).collect()
}

fn rollback() -> BusError {
    BusError::InvalidDurableEvent { r#type: "test.routing.rollback".to_string(), message: "rollback".to_string() }
}

#[test]
fn workspace_only_moves_reach_both_owners() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other_workspace()));

    let moved = bus
        .publish(
            &moved_definition(),
            moved_data(&other_workspace()),
            PublishOptions { location: Some(base()), ..Default::default() },
        )
        .expect("publish the move");
    let after = publish(&bus, &delta_definition(), delta_data());
    let same = publish(&bus, &moved_definition(), moved_data(&other_workspace()));
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(drain(&mut first), vec![moved.clone(), done.clone()], "the old owner keeps the move and the end");
    assert_eq!(
        drain(&mut second),
        vec![moved.clone(), after, same, done],
        "the destination receives the move, later events and the end"
    );
    assert_eq!(moved.location(), Some(base()), "routing leaves the published envelope unchanged");
}

#[test]
fn forks_route_through_the_parent() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    for mode in ["publish", "batch", "replay"] {
        let child = format!("ses_{}", opencode_storage::id::ascending());
        let mut first = bus.subscribe_all(Some(base()));
        let mut second = bus.subscribe_all(Some(other()));
        let payload = json!({
            "sessionID": child.clone(),
            "parentID": SESSION,
            "boundary": { "type": "before", "messageID": "msg_routing" }
        });
        let event_id = EventId::create();
        match mode {
            "publish" => {
                bus.publish(
                    &forked_definition(),
                    payload.clone(),
                    PublishOptions { id: Some(event_id.clone()), ..Default::default() },
                )
                .expect("publish the fork");
            }
            "batch" => {
                bus.publish_all(vec![PublishInput {
                    definition: forked_definition(),
                    data: payload.clone(),
                    options: PublishOptions { id: Some(event_id.clone()), ..Default::default() },
                }])
                .expect("publish the fork in a batch");
            }
            _ => {
                bus.replay(
                    SerializedEvent {
                        id: event_id.clone(),
                        r#type: "session.forked.2".to_string(),
                        created: None,
                        seq: 0,
                        aggregate_id: child.clone(),
                        data: payload.clone(),
                    },
                    ReplayOptions { publish: true, ..Default::default() },
                )
                .expect("replay the fork");
            }
        }
        let after = publish(&bus, &succeeded_definition(), json!({ "sessionID": child }));
        let done = publish(&bus, &done_definition(), json!({}));

        assert_eq!(
            ids(&drain(&mut first)),
            vec![event_id.clone(), after.id().to_string(), done.id().to_string()],
            "{mode}: the parent owner receives the fork, the child's next event and the end"
        );
        assert_eq!(
            ids(&drain(&mut second)),
            vec![done.id().to_string()],
            "{mode}: the other location receives only the end"
        );
    }
}

#[tokio::test]
async fn existing_sessions_keep_public_events_unchanged() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));
    let mut workspace = bus.subscribe_all(Some(other_workspace()));
    let mut global = bus.subscribe_all(None);
    let listened = Arc::new(Mutex::new(Vec::new()));
    let captured = listened.clone();
    let _listener = bus.listen(Box::new(move |event| {
        captured.lock().expect("lock the listened events").push(event.clone());
        Ok(())
    }));

    let renamed = publish(&bus, &renamed_definition(), renamed_data("first"));
    let text = publish(&bus, &delta_definition(), delta_data());
    let broadcast = publish(&bus, &global_definition(), json!({ "sessionID": SESSION }));
    let explicit = bus
        .publish(
            &succeeded_definition(),
            json!({ "sessionID": SESSION }),
            PublishOptions { location: Some(other()), ..Default::default() },
        )
        .expect("publish with an explicit location");
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(
        drain(&mut first),
        vec![renamed.clone(), text.clone(), broadcast.clone(), done.clone()],
        "the session owner receives its unlocated events and the broadcast"
    );
    assert_eq!(
        drain(&mut second),
        vec![broadcast.clone(), explicit.clone(), done.clone()],
        "the other location receives the broadcast, the explicit event and the end"
    );
    assert_eq!(
        drain(&mut workspace),
        vec![broadcast.clone(), done.clone()],
        "another workspace sees only the broadcast and the end"
    );
    assert_eq!(
        drain(&mut global),
        vec![renamed.clone(), text.clone(), broadcast.clone(), explicit.clone(), done.clone()],
        "the global subscriber sees everything"
    );
    assert_eq!(
        *listened.lock().expect("lock the listened events"),
        vec![renamed.clone(), text.clone(), broadcast, explicit, done],
        "listeners see the same events"
    );
    assert!(renamed.location().is_none(), "routing never adds an envelope location to a renamed event");
    assert!(text.location().is_none(), "routing never adds an envelope location to an ephemeral event");
    assert!(!renamed.payload().contains_key("location"), "the published payload keeps no location field");

    let history =
        bus.log(LogInput { aggregate_id: SESSION.to_string(), after: None, follow: false }).collect::<Vec<_>>().await;
    let events = history
        .into_iter()
        .map(|item| item.expect("the history read succeeds"))
        .filter_map(|item| match item {
            LogItem::Event(event) => Some(event),
            LogItem::Synced { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "the log holds the two durable session events");
    for event in events {
        assert!(event.location().is_none(), "the stored history carries no location");
    }
}

#[test]
fn typed_subscriptions_share_the_routing() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut typed = bus.subscribe(&["session.renamed"], Some(other()));
    let mut multiple = bus.subscribe(&["session.renamed", "test.routing.done"], Some(other()));

    publish(&bus, &renamed_definition(), renamed_data("wrong location"));
    publish(&bus, &moved_definition(), moved_data(&other()));
    let expected = publish(&bus, &renamed_definition(), renamed_data("destination"));
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(drain(&mut typed), vec![expected.clone()], "the typed subscriber routes like the all subscriber");
    assert_eq!(drain(&mut multiple), vec![expected, done], "the multi-type subscriber routes each type the same way");
}

#[test]
fn routing_snapshots_across_creation_and_moves() {
    let (_dir, _database, bus) = setup();
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));

    let created = publish(
        &bus,
        &created_definition(),
        json!({
            "sessionID": SESSION,
            "location": location_json(&base()),
            "projectID": PROJECT,
            "slug": "routing",
            "version": "test"
        }),
    );
    let before = publish(&bus, &renamed_definition(), renamed_data("before"));
    let moved = publish(&bus, &moved_definition(), moved_data(&other()));
    let after = publish(&bus, &delta_definition(), delta_data());
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(
        drain(&mut first),
        vec![created, before, moved.clone(), done.clone()],
        "the original owner keeps the events published before the move"
    );
    assert_eq!(drain(&mut second), vec![moved.clone(), after, done], "the destination keeps the events after it");
    assert!(moved.location().is_none(), "the routed move has no envelope location");
}

#[test]
fn deletion_routes_before_the_projector_removes_ownership() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    bus.project(
        "session.deleted.2",
        Box::new(|connection, event| {
            let session = event.data()["sessionID"].as_str().expect("a session event carries its session ID");
            connection.execute("DELETE FROM session_v2 WHERE id = ?1", [session])?;
            Ok(())
        }),
    );
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));
    let mut global = bus.subscribe_all(None);

    let deleted = publish(&bus, &deleted_definition(), json!({ "sessionID": SESSION }));
    let missing = publish(&bus, &delta_definition(), delta_data());
    let done = publish(&bus, &done_definition(), json!({}));

    let stored: Option<String> = database
        .read(|connection| {
            Ok(connection
                .query_row("SELECT id FROM session_v2 WHERE id = ?1", [SESSION], |row| row.get(0))
                .optional()?)
        })
        .expect("read the session row");
    assert_eq!(stored, None, "the projector removed the session");
    assert_eq!(drain(&mut first), vec![deleted.clone(), done.clone()], "the owner still receives its deletion");
    assert_eq!(drain(&mut second), vec![done.clone()], "the other location receives only the end");
    assert_eq!(
        drain(&mut global),
        vec![deleted, missing, done],
        "the global subscriber sees the deletion and the now unrouted event"
    );
}

#[test]
fn a_batch_moves_and_deletes_with_routing_per_event() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));

    let events = bus
        .publish_all(vec![
            PublishInput {
                definition: renamed_definition(),
                data: renamed_data("before"),
                options: PublishOptions::default(),
            },
            PublishInput {
                definition: moved_definition(),
                data: moved_data(&other()),
                options: PublishOptions::default(),
            },
            PublishInput {
                definition: renamed_definition(),
                data: renamed_data("after"),
                options: PublishOptions::default(),
            },
            PublishInput {
                definition: deleted_definition(),
                data: json!({ "sessionID": SESSION }),
                options: PublishOptions::default(),
            },
        ])
        .expect("publish the batch");
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(
        drain(&mut first),
        vec![events[0].clone(), events[1].clone(), done.clone()],
        "the original owner receives the batch up to its move"
    );
    assert_eq!(
        drain(&mut second),
        vec![events[1].clone(), events[2].clone(), events[3].clone(), done],
        "the destination receives the move, the following events and the deletion"
    );
}

#[test]
fn rolled_back_moves_keep_ownership() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));
    let before = publish(&bus, &delta_definition(), delta_data());

    let single = bus.publish(
        &moved_definition(),
        moved_data(&other()),
        PublishOptions { commit: Some(Box::new(|_connection, _seq| Err(rollback()))), ..Default::default() },
    );
    let batch = bus.publish_all(vec![
        PublishInput { definition: moved_definition(), data: moved_data(&other()), options: PublishOptions::default() },
        PublishInput {
            definition: renamed_definition(),
            data: renamed_data("rollback"),
            options: PublishOptions {
                commit: Some(Box::new(|_connection, _seq| Err(rollback()))),
                ..Default::default()
            },
        },
    ]);
    let after = publish(&bus, &delta_definition(), delta_data());
    let done = publish(&bus, &done_definition(), json!({}));

    assert!(single.is_err(), "the single move rolls back");
    assert!(batch.is_err(), "the batch move rolls back");
    assert_eq!(drain(&mut first), vec![before, after, done.clone()], "the owner keeps its events");
    assert_eq!(drain(&mut second), vec![done], "the destination receives only the end");
}

#[test]
fn replay_updates_ownership_and_filters_published_replays() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    publish(&bus, &delta_definition(), delta_data());
    let mut first = bus.subscribe_all(Some(base()));
    let mut second = bus.subscribe_all(Some(other()));

    bus.replay(
        SerializedEvent {
            id: EventId::create(),
            r#type: "session.moved.1".to_string(),
            created: None,
            seq: 0,
            aggregate_id: SESSION.to_string(),
            data: moved_data(&other()),
        },
        ReplayOptions::default(),
    )
    .expect("replay the move silently");
    let after = publish(&bus, &delta_definition(), delta_data());
    let replay_id = EventId::create();
    bus.replay(
        SerializedEvent {
            id: replay_id.clone(),
            r#type: "session.renamed.1".to_string(),
            created: None,
            seq: 1,
            aggregate_id: SESSION.to_string(),
            data: renamed_data("replayed"),
        },
        ReplayOptions { publish: true, ..Default::default() },
    )
    .expect("replay the rename and publish it");
    let done = publish(&bus, &done_definition(), json!({}));

    assert_eq!(drain(&mut first), vec![done.clone()], "the old owner has no further events");
    let received = drain(&mut second);
    assert_eq!(
        ids(&received),
        vec![after.id().to_string(), replay_id, done.id().to_string()],
        "the destination receives the silent replay's follow-ups and the published replay"
    );
    assert!(received[1].location().is_none(), "the published replay keeps no envelope location");
}

#[test]
fn status_events_stay_unrouted() {
    let (_dir, database, bus) = setup();
    seed(&database, &base());
    let mut other_location = bus.subscribe_all(Some(other()));

    let status = bus
        .publish(
            &Definition::ephemeral("session.status"),
            json!({ "sessionID": SESSION, "status": { "type": "busy" } }),
            PublishOptions::default(),
        )
        .expect("publish the status event");
    let idle = bus
        .publish(&Definition::ephemeral("session.idle"), json!({ "sessionID": SESSION }), PublishOptions::default())
        .expect("publish the idle event");

    assert_eq!(
        drain(&mut other_location),
        vec![status, idle],
        "status events are not SessionEvent members, so they reach every location"
    );
}
