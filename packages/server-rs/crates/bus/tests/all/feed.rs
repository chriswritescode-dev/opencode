use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use opencode_bus::feed::{Encode, EventFeed, FeedError, SUBSCRIBER_CAPACITY, frame};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};

use crate::TempDir;

fn setup() -> (TempDir, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, Bus::new(Arc::new(database), BusOptions::default()))
}

fn publish(bus: &Bus, r#type: &'static str, id: &str, data: Value) {
    bus.publish(
        &Definition::ephemeral(r#type),
        data,
        PublishOptions { id: Some(id.to_string()), ..Default::default() },
    )
    .expect("publish the ephemeral event");
}

fn typed_encoder() -> (Arc<AtomicUsize>, Encode) {
    let encodes = Arc::new(AtomicUsize::new(0));
    let counter = encodes.clone();
    let encode: Encode = Arc::new(move |event| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(event.r#type().to_string())
    });
    (encodes, encode)
}

#[test]
fn frame_encodes_the_payload() {
    let payload = json!({ "id": "evt_wire", "created": 1, "type": "agent.updated", "data": {} });
    assert_eq!(frame(&payload), "data: {\"id\":\"evt_wire\",\"created\":1,\"type\":\"agent.updated\",\"data\":{}}\n\n");
}

#[test]
fn encodes_once_for_every_subscriber() {
    let (_dir, bus) = setup();
    let (encodes, encode) = typed_encoder();
    let feed = EventFeed::with_encoder(&bus, SUBSCRIBER_CAPACITY, encode);
    let mut first = feed.subscribe(None);
    let mut second = feed.subscribe(None);

    publish(&bus, "agent.updated", "evt_example", json!({}));

    assert_eq!(first.try_recv(), Some(Ok("agent.updated".to_string())));
    assert_eq!(second.try_recv(), Some(Ok("agent.updated".to_string())));
    assert_eq!(encodes.load(Ordering::Relaxed), 1);
}

#[test]
fn filtered_subscribers_receive_only_their_types() {
    let (_dir, bus) = setup();
    let feed = EventFeed::with_encoder(&bus, SUBSCRIBER_CAPACITY, typed_encoder().1);
    let mut filtered = feed.subscribe(Some(vec!["credential.switched".to_string()]));
    let mut all = feed.subscribe(None);

    publish(&bus, "agent.updated", "evt_ignored", json!({}));
    publish(&bus, "credential.switched", "evt_switched", json!({ "credentialID": "cred_test" }));

    assert_eq!(filtered.try_recv(), Some(Ok("credential.switched".to_string())));
    assert_eq!(filtered.try_recv(), None);
    assert_eq!(all.try_recv(), Some(Ok("agent.updated".to_string())));
    assert_eq!(all.try_recv(), Some(Ok("credential.switched".to_string())));
    assert_eq!(all.try_recv(), None);
}

#[test]
fn slow_subscribers_overflow_without_failing_others() {
    let (_dir, bus) = setup();
    let encode: Encode = Arc::new(|event| Ok(event.id().to_string()));
    let feed = EventFeed::with_encoder(&bus, 1, encode);
    let mut slow = feed.subscribe(None);
    let mut fast = feed.subscribe(None);

    publish(&bus, "agent.updated", "evt_one", json!({}));
    assert_eq!(fast.try_recv(), Some(Ok("evt_one".to_string())));
    publish(&bus, "agent.updated", "evt_two", json!({}));
    assert_eq!(fast.try_recv(), Some(Ok("evt_two".to_string())));
    publish(&bus, "agent.updated", "evt_three", json!({}));
    assert_eq!(fast.try_recv(), Some(Ok("evt_three".to_string())));
    assert_eq!(fast.try_recv(), None);

    assert_eq!(slow.try_recv(), Some(Ok("evt_one".to_string())));
    assert_eq!(slow.try_recv(), Some(Err(FeedError::SubscriberOverflow { capacity: 1 })));
    assert_eq!(slow.try_recv(), None);
}

#[test]
fn public_subscribers_receive_global_credential_events() {
    let (_dir, bus) = setup();
    let feed = EventFeed::with_encoder(&bus, SUBSCRIBER_CAPACITY, typed_encoder().1);
    let mut all = feed.subscribe(None);

    publish(&bus, "credential.switched", "evt_switched", json!({ "credentialID": "cred_test" }));

    assert_eq!(all.try_recv(), Some(Ok("credential.switched".to_string())));
    assert_eq!(all.try_recv(), None);
}

#[test]
fn internal_events_are_filtered_before_capacity() {
    let (_dir, bus) = setup();
    let feed = EventFeed::with_encoder(&bus, 1, typed_encoder().1);
    let mut all = feed.subscribe(None);

    publish(&bus, "test.internal", "evt_internal_one", json!({ "value": "one" }));
    publish(&bus, "test.internal", "evt_internal_two", json!({ "value": "two" }));
    publish(&bus, "agent.updated", "evt_public", json!({}));

    assert_eq!(all.try_recv(), Some(Ok("agent.updated".to_string())));
    assert_eq!(all.try_recv(), None);
}

#[test]
fn encoding_failure_disconnects_current_subscribers_and_keeps_later_ones() {
    let (_dir, bus) = setup();
    let encode: Encode = Arc::new(|event| {
        if event.id() == "evt_bad" {
            return Err("invalid event".to_string());
        }
        Ok(event.id().to_string())
    });
    let feed = EventFeed::with_encoder(&bus, SUBSCRIBER_CAPACITY, encode);
    let mut current = feed.subscribe(None);

    publish(&bus, "agent.updated", "evt_bad", json!({}));

    assert_eq!(
        current.try_recv(),
        Some(Err(FeedError::EncodingError {
            event_id: "evt_bad".to_string(),
            event_type: "agent.updated".to_string(),
            cause: "invalid event".to_string(),
        }))
    );
    assert_eq!(current.try_recv(), None);

    let mut next = feed.subscribe(None);
    publish(&bus, "agent.updated", "evt_good", json!({}));
    assert_eq!(next.try_recv(), Some(Ok("evt_good".to_string())));
    assert_eq!(next.try_recv(), None);
}
