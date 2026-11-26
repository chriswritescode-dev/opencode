use std::time::Duration;

use opencode_fs::mutex::KeyedMutex;
use tokio::sync::oneshot;
use tokio::task::yield_now;

#[tokio::test]
async fn serializes_effects_with_the_same_key() {
    let mutex = KeyedMutex::new();
    let (started, started_rx) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();

    let first = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guard = mutex.lock("shared".to_string()).await;
            started.send(()).unwrap();
            release_rx.await.unwrap();
        }
    });
    started_rx.await.unwrap();

    let (second_started, mut second_started_rx) = oneshot::channel();
    let second = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guard = mutex.lock("shared".to_string()).await;
            second_started.send(()).unwrap();
        }
    });
    yield_now().await;
    assert!(second_started_rx.try_recv().is_err(), "the second caller ran before the first released");

    release.send(()).unwrap();
    first.await.unwrap();
    second.await.unwrap();
    assert_eq!(mutex.size(), 0);
}

#[tokio::test]
async fn allows_different_keys_to_proceed_independently() {
    let mutex = KeyedMutex::new();
    let first = mutex.lock("first".to_string()).await;

    let second = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guard = mutex.lock("second".to_string()).await;
        }
    });
    tokio::time::timeout(Duration::from_secs(1), second).await.expect("a different key waited for the first").unwrap();

    drop(first);
    assert_eq!(mutex.size(), 0);
}

#[tokio::test]
async fn an_interrupted_waiter_keeps_the_holder_lock() {
    let mutex = KeyedMutex::new();
    let (started, started_rx) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();

    let first = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guard = mutex.lock("shared".to_string()).await;
            started.send(()).unwrap();
            release_rx.await.unwrap();
        }
    });
    started_rx.await.unwrap();

    let waiter = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guard = mutex.lock("shared".to_string()).await;
        }
    });
    yield_now().await;
    waiter.abort();
    assert!(waiter.await.is_err());
    assert_eq!(mutex.size(), 1);

    release.send(()).unwrap();
    first.await.unwrap();
    assert_eq!(mutex.size(), 0);
}

#[tokio::test]
async fn lock_all_sorts_and_deduplicates_keys() {
    let mutex = KeyedMutex::new();
    let guards = mutex.lock_all(["b".to_string(), "a".to_string(), "a".to_string()]).await;
    assert_eq!(guards.len(), 2);
    assert_eq!(mutex.size(), 2);
    drop(guards);
    assert_eq!(mutex.size(), 0);
}

#[tokio::test]
async fn lock_all_in_opposite_orders_does_not_deadlock() {
    let mutex = KeyedMutex::new();
    let one = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guards = mutex.lock_all(["a".to_string(), "b".to_string()]).await;
        }
    });
    let two = tokio::spawn({
        let mutex = mutex.clone();
        async move {
            let _guards = mutex.lock_all(["b".to_string(), "a".to_string()]).await;
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        one.await.unwrap();
        two.await.unwrap();
    })
    .await
    .expect("opposite lock_all orders deadlocked");
    assert_eq!(mutex.size(), 0);
}
