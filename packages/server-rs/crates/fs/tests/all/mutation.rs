use std::sync::Arc;
use std::time::Duration;

use opencode_fs::FsError;
use opencode_fs::access::{Access, ResolveInput, Target};
use opencode_fs::mutation::Mutation;
use tokio::sync::Notify;

use crate::fixture::TempDir;

async fn target_for(access: &Access, path: &str) -> Target {
    access.resolve(ResolveInput { path: path.to_owned(), kind: None, base: None }).await.expect("resolves the target")
}

#[tokio::test]
async fn writes_existing_internal_file_and_returns_stable_result() {
    let temp = TempDir::new("mutation-write");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let hello = directory.join("hello.txt");
    std::fs::write(&hello, "before").unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, "hello.txt").await;
    let mutation = Mutation::new();

    let result = mutation.write(&target, "after").await.expect("writes");
    assert_eq!(result.operation, "write");
    assert_eq!(result.target, target.absolute);
    assert_eq!(result.resource, "hello.txt");
    assert!(result.existed);
    assert_eq!(std::fs::read_to_string(&hello).unwrap(), "after");
}

#[tokio::test]
async fn writes_prospective_internal_file_and_creates_parent_directories() {
    let temp = TempDir::new("mutation-create");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, "src/nested/hello.txt").await;
    let mutation = Mutation::new();

    let result = mutation.write(&target, "hello").await.expect("writes");
    assert_eq!(result.resource, "src/nested/hello.txt");
    assert!(!result.existed);
    assert_eq!(std::fs::read_to_string(&target.absolute).unwrap(), "hello");
}

#[tokio::test]
async fn appends_bytes_and_creates_the_file_when_missing() {
    let temp = TempDir::new("mutation-append");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, "log.txt").await;
    let mutation = Mutation::new();

    assert!(!mutation.append(&target, "one\n").await.expect("appends").existed);
    assert!(mutation.append(&target, [0xff_u8, 0x0a]).await.expect("appends").existed);
    assert_eq!(std::fs::read(&target.absolute).unwrap(), b"one\n\xff\n");
}

#[tokio::test]
async fn preserves_exactly_one_bom_and_normalizes_created_text() {
    let temp = TempDir::new("mutation-bom");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let preserved = directory.join("preserved.txt");
    std::fs::write(&preserved, "\u{feff}before").unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let preserved_target = target_for(&access, "preserved.txt").await;
    let created_target = target_for(&access, "created.txt").await;
    let mutation = Mutation::new();

    let preserved_result =
        mutation.write_text_preserving_bom(&preserved_target, "\u{feff}after").await.expect("writes");
    assert!(preserved_result.bom);
    assert!(preserved_result.existed);
    let created_result =
        mutation.write_text_preserving_bom(&created_target, "\u{feff}\u{feff}\u{feff}created").await.expect("writes");
    assert!(created_result.bom);
    assert!(!created_result.existed);
    assert_eq!(std::fs::read_to_string(&preserved).unwrap(), "\u{feff}after");
    assert_eq!(std::fs::read_to_string(&created_target.absolute).unwrap(), "\u{feff}created");
}

#[tokio::test]
async fn writes_an_explicitly_resolved_external_target() {
    let temp = TempDir::new("mutation-external");
    let directory = temp.path().join("case");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, &outside.join("external.txt").to_string_lossy()).await;
    let mutation = Mutation::new();

    let result = mutation.write(&target, "external").await.expect("writes");
    assert!(!result.existed);
    assert_eq!(result.resource, target.resource);
    assert_eq!(std::fs::read_to_string(outside.join("external.txt")).unwrap(), "external");
}

#[tokio::test]
async fn refuses_to_rewrite_invalid_utf8_as_text() {
    let temp = TempDir::new("mutation-utf8");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, "binary.bin").await;
    let mutation = Mutation::new();
    mutation.write(&target, [0xff_u8, 0xfe]).await.expect("writes bytes");

    let error = mutation.read_editable_text(&target).await.expect_err("rejects invalid UTF-8");
    assert!(matches!(error, FsError::InvalidUtf8 { .. }), "{error}");
    assert!(mutation.read_text(&target).await.expect("reads lossily").text.contains('\u{fffd}'));
}

#[tokio::test]
async fn serializes_concurrent_writes_to_the_same_absolute_target() {
    let temp = TempDir::new("mutation-serialize");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let target = target_for(&access, "shared.txt").await;
    let mutation = Arc::new(Mutation::new());

    let held = mutation.lock_target(&target).await;
    let writer = tokio::spawn({
        let mutation = Arc::clone(&mutation);
        let target = target.clone();
        async move { mutation.write(&target, "second").await }
    });
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(!writer.is_finished());
    drop(held);
    writer.await.unwrap().expect("writes after the lock is released");
    assert_eq!(std::fs::read_to_string(&target.absolute).unwrap(), "second");
}

#[tokio::test]
async fn transaction_locks_serialize_the_same_path_across_instances() {
    let temp = TempDir::new("mutation-transaction");
    let path = temp.path().join("shared.txt").to_string_lossy().into_owned();
    let first_mutation = Arc::new(Mutation::new());
    let second_mutation = Arc::new(Mutation::new());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let first = tokio::spawn({
        let mutation = Arc::clone(&first_mutation);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        let path = path.clone();
        async move {
            mutation
                .with_lock(&[path], async {
                    started.notify_one();
                    release.notified().await;
                })
                .await
        }
    });
    started.notified().await;
    let second = tokio::spawn({
        let mutation = Arc::clone(&second_mutation);
        let path = path.clone();
        async move { mutation.with_lock(&[path], async {}).await }
    });
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(!second.is_finished());
    release.notify_one();
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test]
async fn transaction_locks_for_distinct_paths_proceed_independently() {
    let temp = TempDir::new("mutation-distinct");
    let first_path = temp.path().join("first.txt").to_string_lossy().into_owned();
    let second_path = temp.path().join("second.txt").to_string_lossy().into_owned();
    let mutation = Arc::new(Mutation::new());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let first = tokio::spawn({
        let mutation = Arc::clone(&mutation);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        async move {
            mutation
                .with_lock(&[first_path], async {
                    started.notify_one();
                    release.notified().await;
                })
                .await
        }
    });
    started.notified().await;
    let finished = Arc::new(Notify::new());
    let second = tokio::spawn({
        let mutation = Arc::clone(&mutation);
        let finished = Arc::clone(&finished);
        async move {
            mutation
                .with_lock(&[second_path], async {
                    finished.notify_one();
                })
                .await
        }
    });
    finished.notified().await;
    assert!(second.is_finished());
    release.notify_one();
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test]
async fn target_locks_for_distinct_paths_proceed_independently() {
    let temp = TempDir::new("mutation-targets");
    let directory = temp.path().join("case");
    std::fs::create_dir_all(&directory).unwrap();
    let access =
        Access::new(directory.to_string_lossy().into_owned(), directory.to_string_lossy().into_owned(), "/home/agent");
    let first = target_for(&access, "first.txt").await;
    let second = target_for(&access, "second.txt").await;
    let mutation = Mutation::new();

    let _held = mutation.lock_target(&first).await;
    let _other = tokio::time::timeout(Duration::from_secs(2), mutation.lock_target(&second))
        .await
        .expect("a different target locks independently");
}
