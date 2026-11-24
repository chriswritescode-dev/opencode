use std::sync::Arc;
use std::time::Duration;

use opencode_bus::feed::{EventFeed, SUBSCRIBER_CAPACITY};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_session::{ProjectionSettings, SessionProjector};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::Value;

use crate::TempDir;

const CORPUS: &str = include_str!("../corpus/events.json");

#[tokio::test]
async fn usage_updated_follows_the_event_that_triggered_it_on_the_feed() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 1000i64)), ..Default::default() });
    let _usage = SessionProjector::start(&bus, &database, Arc::new(ProjectionSettings::default));
    let feed = EventFeed::new(&bus, SUBSCRIBER_CAPACITY);
    let mut subscription = feed.subscribe(None);

    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let fixture = corpus["fixtures"]
        .as_array()
        .expect("the corpus carries fixtures")
        .iter()
        .find(|fixture| fixture["name"] == "step-supersedes")
        .expect("the step-supersedes fixture");

    for statement in fixture["start"].as_array().expect("the fixture carries start statements") {
        database
            .read(|connection| {
                connection.execute(statement.as_str().expect("a start entry is a statement"), [])?;
                Ok(())
            })
            .expect("seed the fixture start statement");
    }

    for step in fixture["steps"].as_array().expect("the fixture carries steps") {
        let definition = Definition::latest_durable(step["type"].as_str().expect("a step carries type"))
            .expect("a durable step type");
        let options = PublishOptions {
            id: Some(step["id"].as_str().expect("a step carries id").to_owned()),
            metadata: step["metadata"].as_object().cloned(),
            ..Default::default()
        };
        bus.publish(&definition, step["data"].clone(), options).expect("publish the fixture step");
    }

    let mut frames = Vec::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
            .await
            .expect("a usage frame before the timeout")
            .expect("the subscription stays open")
            .expect("the feed encodes the frame");
        let usage = frame.contains("\"type\":\"session.usage.updated\"");
        frames.push(frame);
        if usage {
            break;
        }
    }

    let ended = frames
        .iter()
        .position(|frame| frame.contains("\"type\":\"session.step.ended\""))
        .expect("the feed carries session.step.ended");
    let usage = frames
        .iter()
        .position(|frame| frame.contains("\"type\":\"session.usage.updated\""))
        .expect("the feed carries session.usage.updated");
    assert!(ended < usage, "session.usage.updated reached the feed before session.step.ended");
}
