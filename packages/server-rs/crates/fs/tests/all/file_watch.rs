use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use opencode_bus::{Bus, BusOptions};
use opencode_contract::generated::{AbsolutePath, LocationRef};
use opencode_fs::file_watch::{DisabledError, FileWatch, Input, MAX_PER_LOCATION};
use opencode_fs::watcher::{Subscription, Update, UpdateKind, WatchInput, WatchSource};
use opencode_storage::{Database, DatabaseLocation};
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::fixture::TempDir;

struct FakeSource {
    calls: StdMutex<Vec<WatchInput>>,
    senders: StdMutex<Vec<broadcast::Sender<Update>>>,
}

impl FakeSource {
    fn new() -> Arc<FakeSource> {
        Arc::new(FakeSource { calls: StdMutex::new(Vec::new()), senders: StdMutex::new(Vec::new()) })
    }

    fn count(&self) -> usize {
        self.calls.lock().expect("the fake source lock is never poisoned").len()
    }

    fn emit(&self, update: Update) {
        for sender in self.senders.lock().expect("the fake source lock is never poisoned").iter() {
            let _ = sender.send(update.clone());
        }
    }
}

impl WatchSource for FakeSource {
    fn subscribe(&self, input: WatchInput) -> Subscription {
        self.calls.lock().expect("the fake source lock is never poisoned").push(input);
        let (sender, receiver) = broadcast::channel(16);
        self.senders.lock().expect("the fake source lock is never poisoned").push(sender);
        Subscription::from_receiver(receiver)
    }
}

fn setup() -> (TempDir, Arc<Bus>) {
    let directory = TempDir::new("file-watch");
    let (database, _) = Database::open(DatabaseLocation::Memory, directory.path()).expect("opens the database");
    let bus = Bus::new(Arc::new(database), BusOptions::default());
    (directory, bus)
}

fn location(directory: &Path) -> LocationRef {
    LocationRef { directory: AbsolutePath(directory.to_string_lossy().into_owned()), workspace_id: None }
}

fn input(directory: &Path, name: &str) -> Input {
    Input { location: location(directory), file: directory.join(name).to_string_lossy().into_owned() }
}

async fn wait_started(source: &FakeSource, count: usize) {
    for _ in 0..10_000 {
        if source.count() >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the watch did not start");
}

async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn shares_one_watch_per_file_and_resubscribes_after_unwatch() {
    let (directory, bus) = setup();
    let source = FakeSource::new();
    let watch = FileWatch::new(source.clone(), bus, true);
    let file = input(directory.path(), "a.ts");

    watch.watch(file.clone()).await.expect("starts the watch");
    wait_started(&source, 1).await;
    watch.watch(file.clone()).await.expect("resets the watch");
    settle().await;
    assert_eq!(source.count(), 1);

    watch.unwatch(file.clone()).await;
    watch.watch(file.clone()).await.expect("restarts the watch");
    wait_started(&source, 2).await;
    assert_eq!(source.count(), 2);
}

#[tokio::test]
async fn expires_a_watch_after_24_hours_unless_watched_again() {
    tokio::time::pause();
    let (directory, bus) = setup();
    let source = FakeSource::new();
    let watch = FileWatch::new(source.clone(), bus, true);
    let first = input(directory.path(), "a.ts");
    let second = input(directory.path(), "b.ts");

    watch.watch(first.clone()).await.expect("starts the first watch");
    watch.watch(second.clone()).await.expect("starts the second watch");
    wait_started(&source, 2).await;

    tokio::time::advance(Duration::from_secs(23 * 60 * 60)).await;
    watch.watch(first.clone()).await.expect("resets the first watch");
    tokio::time::advance(Duration::from_secs(2 * 60 * 60)).await;
    settle().await;
    assert_eq!(source.count(), 2);

    watch.watch(first.clone()).await.expect("resets the live watch");
    settle().await;
    assert_eq!(source.count(), 2);

    watch.watch(second.clone()).await.expect("restarts the expired watch");
    wait_started(&source, 3).await;
    assert_eq!(source.count(), 3);
}

#[tokio::test]
async fn evicts_the_watch_closest_to_expiring_when_a_location_is_full() {
    tokio::time::pause();
    let (directory, bus) = setup();
    let source = FakeSource::new();
    let watch = FileWatch::new(source.clone(), bus, true);
    let other = TempDir::new("file-watch-other");

    watch.watch(input(directory.path(), "oldest.ts")).await.expect("starts the oldest watch");
    tokio::time::advance(Duration::from_secs(60)).await;
    for index in 0..MAX_PER_LOCATION - 1 {
        watch.watch(input(directory.path(), &format!("{index}.ts"))).await.expect("starts a watch");
    }
    watch
        .watch(Input {
            location: location(other.path()),
            file: other.path().join("a.ts").to_string_lossy().into_owned(),
        })
        .await
        .expect("starts a watch in another location");
    wait_started(&source, MAX_PER_LOCATION + 1).await;
    assert_eq!(source.count(), MAX_PER_LOCATION + 1);

    watch.watch(input(directory.path(), "newest.ts")).await.expect("evicts and starts a watch");
    wait_started(&source, MAX_PER_LOCATION + 2).await;
    assert_eq!(source.count(), MAX_PER_LOCATION + 2);

    watch.watch(input(directory.path(), "0.ts")).await.expect("resets an existing watch");
    settle().await;
    assert_eq!(source.count(), MAX_PER_LOCATION + 2);

    watch.watch(input(directory.path(), "oldest.ts")).await.expect("restarts the evicted watch");
    wait_started(&source, MAX_PER_LOCATION + 3).await;
    assert_eq!(source.count(), MAX_PER_LOCATION + 3);
}

#[tokio::test]
async fn publishes_filesystem_changed_on_the_watch_location() {
    let (directory, bus) = setup();
    let source = FakeSource::new();
    let watch = FileWatch::new(source.clone(), bus.clone(), true);
    let file = input(directory.path(), "a.ts");
    let target = Path::new(&file.file);
    std::fs::write(target, "a").expect("writes the watched file");
    let mut events = bus.subscribe(&["filesystem.changed"], Some(file.location.clone()));

    watch.watch(file.clone()).await.expect("starts the watch");
    wait_started(&source, 1).await;
    source.emit(Update { path: target.to_path_buf(), kind: UpdateKind::Update });
    let event = timeout(Duration::from_secs(2), events.recv()).await.expect("an event arrives").expect("an event");
    assert_eq!(event.r#type(), "filesystem.changed");
    assert_eq!(event.data()["file"], target.to_string_lossy().as_ref());
    assert_eq!(event.data()["event"], "change");
    assert_eq!(event.location(), Some(file.location.clone()));

    std::fs::remove_file(target).expect("removes the watched file");
    source.emit(Update { path: target.to_path_buf(), kind: UpdateKind::Update });
    let event = timeout(Duration::from_secs(2), events.recv()).await.expect("an event arrives").expect("an event");
    assert_eq!(event.data()["event"], "unlink");
}

#[tokio::test]
async fn a_disabled_file_watch_rejects_requests() {
    let (directory, bus) = setup();
    let source = FakeSource::new();
    let watch = FileWatch::new(source, bus, false);
    let error = watch.watch(input(directory.path(), "a.ts")).await.expect_err("the watch fails");
    assert_eq!(error, DisabledError);
}
