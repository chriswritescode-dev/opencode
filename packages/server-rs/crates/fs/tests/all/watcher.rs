use std::time::Duration;

use opencode_fs::watcher::{Subscription, Update, UpdateKind, WatchInput, Watcher};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;

use crate::fixture::TempDir;

async fn receive(subscription: &mut Subscription) -> Update {
    timeout(Duration::from_secs(3), subscription.recv())
        .await
        .expect("an update arrives")
        .expect("the subscription stays open")
}

async fn expect_quiet(subscription: &mut Subscription) {
    assert!(timeout(Duration::from_millis(300), subscription.recv()).await.is_err(), "no update was expected");
}

#[tokio::test]
async fn a_missing_directory_ends_the_stream() {
    let directory = TempDir::new("watcher-missing");
    let target = directory.path().join("missing").join("a.ts");
    let watcher = Watcher::new(true);
    let mut subscription = watcher.subscribe(WatchInput::File { path: target });
    assert!(matches!(subscription.recv().await, Err(RecvError::Closed)));
}

#[tokio::test]
async fn watches_only_the_exact_file() {
    let directory = TempDir::new("watcher-file");
    let target = directory.path().join("opencode.json");
    std::fs::write(&target, "one").expect("writes the target");
    let watcher = Watcher::new(true);
    let mut subscription = watcher.subscribe(WatchInput::File { path: target.clone() });

    std::fs::write(directory.path().join("other.json"), "sibling").expect("writes a sibling");
    expect_quiet(&mut subscription).await;

    std::fs::write(&target, "two").expect("modifies the target");
    let update = receive(&mut subscription).await;
    assert_eq!(update.path, target);
    assert_eq!(update.kind, UpdateKind::Update);
}

#[tokio::test]
async fn detects_creation_of_a_missing_directory_target() {
    let directory = TempDir::new("watcher-created");
    let target = directory.path().join("generated");
    let watcher = Watcher::new(true);
    let mut subscription = watcher.subscribe(WatchInput::File { path: target.clone() });

    std::fs::create_dir(&target).expect("creates the missing directory");
    let update = receive(&mut subscription).await;
    assert_eq!(update.path, target);
}

#[tokio::test]
async fn watches_only_named_immediate_entries() {
    let directory = TempDir::new("watcher-entries");
    let watcher = Watcher::new(true);
    let mut subscription = watcher.subscribe(WatchInput::Entries {
        path: directory.path().to_path_buf(),
        names: vec!["opencode.json".to_owned(), ".opencode".to_owned()],
    });

    std::fs::create_dir(directory.path().join("nested")).expect("creates a nested directory");
    std::fs::write(directory.path().join("nested").join("opencode.json"), "ignored").expect("writes a nested entry");
    std::fs::write(directory.path().join("opencode.jsonc"), "ignored").expect("writes a sibling");
    expect_quiet(&mut subscription).await;

    let target = directory.path().join("opencode.json");
    std::fs::write(&target, "first").expect("writes the watched entry");
    loop {
        let update = receive(&mut subscription).await;
        if update.path == target {
            assert_eq!(update.kind, UpdateKind::Update);
            break;
        }
    }

    let hidden = directory.path().join(".opencode");
    std::fs::create_dir(&hidden).expect("creates the watched entry");
    loop {
        let update = receive(&mut subscription).await;
        if update.path == hidden {
            break;
        }
    }
}

#[tokio::test]
async fn watches_directories_recursively_and_honors_a_literal_ignore() {
    let directory = TempDir::new("watcher-directory");
    let ignored = directory.path().join("ignored");
    let kept = directory.path().join("kept");
    std::fs::create_dir(&ignored).expect("creates the ignored directory");
    std::fs::create_dir(&kept).expect("creates the kept directory");
    let watcher = Watcher::new(true);
    let mut subscription = watcher
        .subscribe(WatchInput::Directory { path: directory.path().to_path_buf(), ignore: vec!["ignored".to_owned()] });

    std::fs::write(ignored.join("a.ts"), "ignored").expect("writes an ignored file");
    expect_quiet(&mut subscription).await;

    let nested = kept.join("b.ts");
    std::fs::write(&nested, "kept").expect("writes a nested file");
    loop {
        let update = receive(&mut subscription).await;
        if update.path == nested {
            break;
        }
    }
}

#[tokio::test]
async fn watches_directories_recursively_and_honors_a_glob_ignore() {
    let directory = TempDir::new("watcher-glob");
    let watcher = Watcher::new(true);
    let mut subscription = watcher
        .subscribe(WatchInput::Directory { path: directory.path().to_path_buf(), ignore: vec!["**/*.tmp".to_owned()] });

    let temporary = directory.path().join("a.tmp");
    std::fs::write(&temporary, "temporary").expect("writes an ignored file");
    expect_quiet(&mut subscription).await;

    let kept = directory.path().join("kept.ts");
    std::fs::write(&kept, "kept").expect("writes a kept file");
    loop {
        let update = receive(&mut subscription).await;
        if update.path == kept {
            break;
        }
    }
}

#[tokio::test]
async fn shares_equivalent_inputs_and_releases_with_the_last_subscriber() {
    let directory = TempDir::new("watcher-shared");
    let target = directory.path().join("shared.ts");
    std::fs::write(&target, "one").expect("writes the shared file");
    let watcher = Watcher::new(true);

    let mut first = watcher.subscribe(WatchInput::File { path: target.clone() });
    let mut second = watcher.subscribe(WatchInput::File { path: target.clone() });
    assert_eq!(watcher.active(), 1);

    std::fs::write(&target, "two").expect("modifies the shared file");
    assert_eq!(receive(&mut first).await.path, target);
    assert_eq!(receive(&mut second).await.path, target);

    drop(first);
    assert_eq!(watcher.active(), 1);
    drop(second);
    assert_eq!(watcher.active(), 0);
}

#[tokio::test]
async fn a_disabled_watcher_ends_every_subscription() {
    let directory = TempDir::new("watcher-disabled");
    let watcher = Watcher::new(false);
    let mut subscription = watcher.subscribe(WatchInput::File { path: directory.path().join("a.ts") });
    assert!(matches!(subscription.recv().await, Err(RecvError::Closed)));
    assert_eq!(watcher.active(), 0);
}
