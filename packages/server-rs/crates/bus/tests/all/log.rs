use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use opencode_bus::{Bus, BusError, BusOptions, Definition, Event, LogInput, LogItem, PublishOptions};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};

use crate::TempDir;

fn setup(persist: bool) -> (TempDir, Arc<Bus>) {
    setup_with(BusOptions { persist, ..Default::default() })
}

fn setup_with(options: BusOptions) -> (TempDir, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let bus = Bus::new(Arc::new(database), options);
    (dir, bus)
}

fn renamed_definition() -> Definition {
    Definition::durable("session.renamed", 1, "sessionID")
}

fn session_id() -> String {
    format!("ses_{}", opencode_storage::id::ascending())
}

fn renamed(session: &str, title: &str) -> Value {
    json!({ "sessionID": session, "title": title })
}

fn publish(bus: &Bus, session: &str, title: &str) {
    bus.publish(&renamed_definition(), renamed(session, title), PublishOptions::default())
        .expect("publish the renamed event");
}

fn seq(event: &Event) -> i64 {
    event.durable().expect("a log event carries a durable envelope").seq
}

fn into_event(item: LogItem) -> Event {
    let LogItem::Event(event) = item else {
        panic!("expected a log event, got the synced marker");
    };
    event
}

fn sequence_of(item: &LogItem) -> i64 {
    match item {
        LogItem::Event(event) => seq(event),
        LogItem::Synced { .. } => panic!("expected a log event, got the synced marker"),
    }
}

async fn next_before_timeout<S>(stream: &mut S) -> Option<Result<LogItem, BusError>>
where
    S: Stream<Item = Result<LogItem, BusError>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the log stream yields an item before the timeout")
}

async fn next_item<S>(stream: &mut S) -> Result<LogItem, BusError>
where
    S: Stream<Item = Result<LogItem, BusError>> + Unpin,
{
    next_before_timeout(stream).await.expect("the log stream has another item")
}

async fn drain<S>(stream: S) -> Result<Vec<LogItem>, BusError>
where
    S: Stream<Item = Result<LogItem, BusError>>,
{
    let mut stream = std::pin::pin!(stream);
    let mut items = Vec::new();
    while let Some(item) = next_before_timeout(&mut stream).await {
        items.push(item?);
    }
    Ok(items)
}

async fn take_events<S>(stream: S, count: usize) -> Result<Vec<Event>, BusError>
where
    S: Stream<Item = Result<LogItem, BusError>>,
{
    let mut stream = std::pin::pin!(stream);
    let mut events = Vec::new();
    while events.len() < count {
        if let LogItem::Event(event) = next_item(&mut stream).await? {
            events.push(event);
        }
    }
    Ok(events)
}

#[tokio::test]
async fn log_replays_after_a_sequence_then_tails_new_events() {
    let (_dir, bus) = setup(true);
    let session = session_id();
    publish(&bus, &session, "zero");
    publish(&bus, &session, "one");

    let mut stream = std::pin::pin!(bus.log(LogInput { aggregate_id: session.clone(), after: Some(0), follow: true }));

    let replayed = into_event(next_item(&mut stream).await.expect("the log yields the replayed event"));
    assert_eq!(replayed.r#type(), "session.renamed", "the replay yields the durable event");
    assert_eq!(seq(&replayed), 1, "the replay starts after the requested sequence");
    assert_eq!(replayed.data()["title"], json!("one"), "the replay yields the event after the cursor");

    let marker = next_item(&mut stream).await.expect("the log yields the replay-to-live marker");
    assert_eq!(
        marker,
        LogItem::Synced { aggregate_id: session.clone(), seq: Some(1) },
        "the marker sits at the replay-to-live boundary"
    );

    publish(&bus, &session, "two");

    let tailed = into_event(next_item(&mut stream).await.expect("the log tails the new event"));
    assert_eq!(seq(&tailed), 2, "the tail yields the next sequence");
    assert_eq!(tailed.r#type(), "session.renamed", "the tail yields the durable event");
    assert_eq!(tailed.data()["title"], json!("two"), "the tail yields the new event");
}

#[tokio::test]
async fn log_catches_an_event_committed_during_replay_handoff() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let session = session_id();
    let slot = Arc::new(Mutex::new(None::<Arc<Bus>>));
    let fired = Arc::new(AtomicBool::new(false));
    let seam: Arc<dyn Fn(&str) + Send + Sync> = {
        let slot = slot.clone();
        let fired = fired.clone();
        let session = session.clone();
        Arc::new(move |_aggregate_id: &str| {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let bus = slot.lock().expect("lock the bus slot").clone();
            let Some(bus) = bus else {
                return;
            };
            publish(&bus, &session, "one");
        })
    };
    let bus = Bus::new(
        Arc::new(database),
        BusOptions { persist: true, before_aggregate_read: Some(seam), ..Default::default() },
    );
    *slot.lock().expect("lock the bus slot") = Some(bus.clone());

    publish(&bus, &session, "zero");

    let mut stream = std::pin::pin!(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: true }));

    let replayed = into_event(next_item(&mut stream).await.expect("the log yields the historical event"));
    assert_eq!(seq(&replayed), 0, "the historical event keeps its sequence");
    assert_eq!(replayed.data()["title"], json!("zero"), "the historical event keeps its title");

    let marker = next_item(&mut stream).await.expect("the log yields the synced marker");
    assert_eq!(
        marker,
        LogItem::Synced { aggregate_id: session.clone(), seq: Some(0) },
        "the historical read is bounded before the mid-read publish"
    );

    let tailed =
        into_event(next_item(&mut stream).await.expect("the log yields the event committed during the handoff"));
    assert_eq!(seq(&tailed), 1, "the handoff event takes the next sequence");
    assert_eq!(tailed.data()["title"], json!("one"), "the log catches the event committed during replay");
}

#[tokio::test]
async fn log_drains_every_event_from_coalesced_wakes() {
    let (_dir, bus) = setup(true);
    let session = session_id();
    let count = 64;
    let stream = bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: true });

    let (events, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(take_events(stream, count), async {
            tokio::task::yield_now().await;
            for index in 0..count {
                publish(&bus, &session, &index.to_string());
            }
        })
    })
    .await
    .expect("the coalesced log drains within the timeout");
    let events = events.expect("the log stream succeeds");

    assert_eq!(events.len(), count, "every committed event is drained");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(seq(event), index as i64, "event {index} keeps its sequence");
        assert_eq!(event.r#type(), "session.renamed", "event {index} is the durable event");
        assert_eq!(event.data()["title"], json!(index.to_string()), "event {index} keeps its title");
    }
}

#[tokio::test]
async fn log_omits_live_only_events() {
    let (_dir, bus) = setup(true);
    let session = session_id();
    let mut stream = std::pin::pin!(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: true }));

    let marker = next_item(&mut stream).await.expect("the log yields the initial synced marker");
    assert_eq!(marker, LogItem::Synced { aggregate_id: session.clone(), seq: None }, "the empty log is marked synced");

    bus.publish(&Definition::ephemeral("test.message"), json!({ "text": "live only" }), PublishOptions::default())
        .expect("publish the live-only event");
    publish(&bus, &session, "durable");

    let event = into_event(next_item(&mut stream).await.expect("the log yields the durable event"));
    assert_eq!(event.r#type(), "session.renamed", "the live-only event never enters the durable log");
    assert_eq!(seq(&event), 0, "the durable event keeps its sequence");
    assert_eq!(event.data()["title"], json!("durable"), "the durable event is delivered");
}

#[tokio::test]
async fn log_replays_across_configured_pages() {
    let (_dir, bus) = setup_with(BusOptions { persist: true, log_read_page_size: 2, ..Default::default() });
    let session = session_id();
    for title in ["zero", "one", "two", "three", "four"] {
        publish(&bus, &session, title);
    }

    let items = drain(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: false }))
        .await
        .expect("the log drains");

    assert_eq!(items.len(), 6, "five events across pages and then the marker");
    assert_eq!(
        items.iter().take(5).map(sequence_of).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4],
        "the pages replay in aggregate order"
    );
    assert_eq!(
        items.last(),
        Some(&LogItem::Synced { aggregate_id: session.clone(), seq: Some(4) }),
        "the marker reports the last replayed sequence"
    );
}

#[tokio::test]
async fn log_without_follow_ends_with_the_synced_marker() {
    let (_dir, bus) = setup(true);
    let session = session_id();
    publish(&bus, &session, "zero");
    publish(&bus, &session, "one");

    let items = drain(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: false }))
        .await
        .expect("the log drains");

    assert_eq!(items.len(), 3, "two events and the synced marker");
    assert_eq!(sequence_of(&items[0]), 0, "the first event keeps its sequence");
    assert_eq!(sequence_of(&items[1]), 1, "the second event keeps its sequence");
    let last = items.last().expect("the marker is the last item");
    assert!(last.is_synced(), "the log ends with the synced marker");
    assert_eq!(
        last,
        &LogItem::Synced { aggregate_id: session.clone(), seq: Some(1) },
        "the marker reports the last replayed sequence"
    );
    assert_eq!(
        last.json(),
        format!(r#"{{"type":"log.synced","aggregateID":"{session}","seq":1}}"#),
        "the marker serializes its cursor"
    );
}

#[tokio::test]
async fn log_marker_omits_seq_for_an_empty_log_and_keeps_the_cursor() {
    let (_dir, bus) = setup(true);
    let session = session_id();

    let empty = drain(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: false }))
        .await
        .expect("the empty log drains");
    assert_eq!(
        empty,
        vec![LogItem::Synced { aggregate_id: session.clone(), seq: None }],
        "an empty log is a bare marker"
    );
    assert_eq!(
        empty[0].json(),
        format!(r#"{{"type":"log.synced","aggregateID":"{session}"}}"#),
        "the empty marker omits the seq key"
    );

    publish(&bus, &session, "zero");

    let drained = drain(bus.log(LogInput { aggregate_id: session.clone(), after: Some(0), follow: false }))
        .await
        .expect("the drained log completes");
    assert_eq!(
        drained,
        vec![LogItem::Synced { aggregate_id: session, seq: Some(0) }],
        "the cursor keeps the current log position"
    );
}

#[tokio::test]
async fn log_skips_undecodable_rows_and_marks_the_true_position() {
    let (_dir, bus) = setup(true);
    let session = session_id();
    publish(&bus, &session, "zero");
    bus.publish(
        &Definition::durable("test.session.log.gap", 1, "sessionID"),
        json!({ "sessionID": &session, "value": "filtered" }),
        PublishOptions::default(),
    )
    .expect("publish the undecodable event");
    publish(&bus, &session, "two");

    let items = drain(bus.log(LogInput { aggregate_id: session.clone(), after: Some(0), follow: false }))
        .await
        .expect("the log drains");

    assert_eq!(items.len(), 2, "only the decodable event and the marker remain");
    let event = into_event(items[0].clone());
    assert_eq!(seq(&event), 2, "the log resumes after the skipped row");
    assert_eq!(event.r#type(), "session.renamed", "the skipped row never surfaces");
    assert_eq!(event.data()["title"], json!("two"), "the decodable event keeps its data");
    assert_eq!(
        items[1],
        LogItem::Synced { aggregate_id: session, seq: Some(2) },
        "the marker reports the true log position"
    );
}

#[tokio::test]
async fn log_without_persistence_is_only_the_marker() {
    let (_dir, bus) = setup(false);
    let session = session_id();
    publish(&bus, &session, "zero");
    publish(&bus, &session, "one");

    let items = drain(bus.log(LogInput { aggregate_id: session.clone(), after: None, follow: false }))
        .await
        .expect("the log drains");

    assert_eq!(
        items,
        vec![LogItem::Synced { aggregate_id: session, seq: Some(1) }],
        "the sequence advances without stored event rows"
    );
}
