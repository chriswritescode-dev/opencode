use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use opencode_catalog::provider_plugin::http::{BufferedResponse, HttpError};
use opencode_catalog::shared_cache::{SharedCache, Transient};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TestError {
    code: u16,
    transient: bool,
}

impl Transient for TestError {
    fn is_transient(&self) -> bool {
        self.transient
    }
}

const OFFLINE: TestError = TestError { code: 0, transient: false };
const TIMEOUT: TestError = TestError { code: 0, transient: true };

fn status(code: u16) -> HttpError {
    HttpError::Status(BufferedResponse { status: code, headers: Vec::new(), body: bytes::Bytes::new() })
}

fn enqueue(
    queue: &Mutex<VecDeque<Result<String, TestError>>>,
    results: impl IntoIterator<Item = Result<String, TestError>>,
) {
    queue.lock().expect("the queue lock is never poisoned").extend(results);
}

fn once(counts: &Mutex<HashMap<&'static str, usize>>, key: &'static str) -> Result<String, TestError> {
    let mut counts = counts.lock().expect("the load counts lock is never poisoned");
    let count = counts.entry(key).or_default();
    *count += 1;
    if *count == 1 {
        return Ok(key.to_owned());
    }
    Err(OFFLINE)
}

fn counter(
    counts: Arc<Mutex<HashMap<&'static str, usize>>>,
    key: &'static str,
) -> impl Fn() -> std::future::Ready<Result<String, TestError>> + Send + 'static {
    move || std::future::ready(once(&counts, key))
}

#[test]
fn counts_transient_failures() {
    assert!(HttpError::Transport("boom".to_owned()).is_transient());
    assert!(HttpError::Timeout.is_transient());
    assert!(status(503).is_transient());
    assert!(status(429).is_transient());
    assert!(!status(401).is_transient());
    assert!(!status(404).is_transient());
    assert!(!TestError { code: 400, transient: false }.is_transient());
}

#[tokio::test(start_paused = true)]
async fn joins_a_running_load_and_refreshes_after_the_callers_interval() {
    let cache = SharedCache::new();
    let (release, gate) = watch::channel(false);
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));

    let get = {
        let cache = cache.clone();
        let gate = gate.clone();
        move |key: &'static str, loads: Arc<AtomicUsize>, interval: Option<Duration>, force: bool| {
            let cache = cache.clone();
            let gate = gate.clone();
            async move {
                cache
                    .get(key, interval, force, move || {
                        let mut gate = gate.clone();
                        let loads = loads.clone();
                        async move {
                            if !*gate.borrow() {
                                let _ = gate.changed().await;
                            }
                            Ok::<String, TestError>(format!("{key}{}", loads.fetch_add(1, Ordering::SeqCst) + 1))
                        }
                    })
                    .await
            }
        }
    };

    let first = tokio::spawn(get("a", a.clone(), None, false));
    let forced = tokio::spawn(get("a", a.clone(), None, true));
    let other = tokio::spawn(get("b", b.clone(), None, false));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    release.send(true).expect("release the gate");
    assert_eq!(first.await.expect("join the first caller").expect("the first load"), "a1");
    assert_eq!(forced.await.expect("join the forced caller").expect("the forced load"), "a1");
    assert_eq!(other.await.expect("join the other caller").expect("the other load"), "b1");

    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    assert_eq!(get("a", a.clone(), None, false).await.expect("the cached value"), "a1");
    assert_eq!(
        get("a", a.clone(), Some(Duration::from_secs(60)), false).await.expect("the shorter interval reloads"),
        "a2"
    );
    assert_eq!(get("a", a.clone(), None, true).await.expect("the forced load"), "a3");
    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    assert_eq!(get("b", b.clone(), None, false).await.expect("the stale value reloads"), "b2");
    assert_eq!(a.load(Ordering::SeqCst), 3);
    assert_eq!(b.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn retries_transient_failures_and_keeps_the_last_good_value() {
    let cache = SharedCache::new();
    let loads = Arc::new(AtomicUsize::new(0));
    let queue: Arc<Mutex<VecDeque<Result<String, TestError>>>> = Arc::new(Mutex::new(VecDeque::new()));

    let get = {
        let cache = cache.clone();
        let loads = loads.clone();
        let queue = queue.clone();
        move |force: bool| {
            let cache = cache.clone();
            let loads = loads.clone();
            let queue = queue.clone();
            async move {
                cache
                    .get("models", None, force, move || {
                        let loads = loads.clone();
                        let queue = queue.clone();
                        async move {
                            loads.fetch_add(1, Ordering::SeqCst);
                            queue
                                .lock()
                                .expect("the queue lock is never poisoned")
                                .pop_front()
                                .expect("a queued result")
                        }
                    })
                    .await
            }
        }
    };

    enqueue(&queue, [Err(TestError { code: 401, transient: false })]);
    assert_eq!(get(false).await.expect_err("the first load fails").code, 401);
    assert_eq!(loads.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_secs(9 * 60)).await;
    assert_eq!(get(false).await.expect_err("the cached failure is returned").code, 401);
    assert_eq!(loads.load(Ordering::SeqCst), 1);

    enqueue(&queue, [Err(TIMEOUT), Err(TIMEOUT), Ok("list".to_owned())]);
    assert_eq!(get(true).await.expect("the flaky load recovers"), "list");
    assert_eq!(loads.load(Ordering::SeqCst), 4);

    enqueue(
        &queue,
        [Err(TestError { code: 401, transient: false }), Err(TIMEOUT), Err(TIMEOUT), Err(TIMEOUT), Err(TIMEOUT)],
    );
    assert_eq!(get(true).await.expect("the last list survives a non-transient failure"), "list");
    assert_eq!(loads.load(Ordering::SeqCst), 5);
    assert_eq!(get(true).await.expect("the last list survives an outage"), "list");
    assert_eq!(loads.load(Ordering::SeqCst), 9);
    assert_eq!(get(false).await.expect("the kept list is cached"), "list");
    assert_eq!(loads.load(Ordering::SeqCst), 9);
}

#[tokio::test(start_paused = true)]
async fn keeps_the_load_when_callers_drop_and_never_caches_panics() {
    let cache = SharedCache::new();
    let (release, gate) = watch::channel(false);
    let loads = Arc::new(AtomicUsize::new(0));

    let first = tokio::spawn({
        let cache = cache.clone();
        let loads = loads.clone();
        let gate = gate.clone();
        async move {
            cache
                .get::<u32, TestError, _, _>("models", None, false, move || {
                    let loads = loads.clone();
                    let mut gate = gate.clone();
                    async move {
                        if !*gate.borrow() {
                            let _ = gate.changed().await;
                        }
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(1)
                    }
                })
                .await
        }
    });

    let second = tokio::spawn({
        let cache = cache.clone();
        let loads = loads.clone();
        async move {
            cache
                .get("models", None, false, move || {
                    let loads = loads.clone();
                    async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok::<u32, TestError>(2)
                    }
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    first.abort();
    release.send(true).expect("release the gate");
    assert_eq!(second.await.expect("join the second caller").expect("the joined load"), 1);
    assert_eq!(loads.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    let panicked = AssertUnwindSafe(cache.get::<u32, TestError, _, _>("models", None, false, {
        let loads = loads.clone();
        move || {
            let loads = loads.clone();
            async move {
                loads.fetch_add(1, Ordering::SeqCst);
                panic!("bug")
            }
        }
    }))
    .catch_unwind()
    .await;
    let message = panicked.expect_err("the load panic reaches the caller").downcast::<String>().ok();
    assert_eq!(message.as_deref().map(String::as_str), Some("bug"));
    assert_eq!(loads.load(Ordering::SeqCst), 2);

    let fresh = cache
        .get::<u32, TestError, _, _>("models", None, false, {
            let loads = loads.clone();
            move || {
                let loads = loads.clone();
                async move {
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(3)
                }
            }
        })
        .await;
    assert_eq!(fresh.expect("the fresh load succeeds"), 3);
    assert_eq!(loads.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn drops_entries_unread_for_24_hours() {
    let cache = SharedCache::new();
    let counts: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let (release, gate) = watch::channel(false);
    let daily = Duration::from_secs(48 * 60 * 60);

    let idle = cache.get("idle", None, false, counter(counts.clone(), "idle")).await.expect("the idle load");
    assert_eq!(idle, "idle");
    let first = cache.get("daily", Some(daily), false, counter(counts.clone(), "daily")).await.expect("the daily load");
    assert_eq!(first, "daily");

    let slow_load = {
        let counts = counts.clone();
        let gate = gate.clone();
        move || {
            let counts = counts.clone();
            let mut gate = gate.clone();
            async move {
                if !*gate.borrow() {
                    let _ = gate.changed().await;
                }
                once(&counts, "slow")
            }
        }
    };

    let slow = tokio::spawn({
        let cache = cache.clone();
        let load = slow_load.clone();
        async move { cache.get("slow", None, false, load).await }
    });
    tokio::task::yield_now().await;

    for _ in 0..=150 {
        let used =
            cache.get("used", None, false, counter(counts.clone(), "used")).await.expect("the used value survives");
        assert_eq!(used, "used");
        tokio::time::advance(Duration::from_secs(10 * 60)).await;
    }

    let expired =
        cache.get("idle", None, false, counter(counts.clone(), "idle")).await.expect_err("the idle entry expired");
    assert_eq!(expired, OFFLINE);
    assert_eq!(counts.lock().expect("the counts lock is never poisoned").get("idle").copied(), Some(2));

    tokio::time::advance(Duration::from_secs(23 * 60 * 60)).await;
    let kept = cache
        .get("daily", Some(daily), false, counter(counts.clone(), "daily"))
        .await
        .expect("the daily value survives a failed reload");
    assert_eq!(kept, "daily");

    let joined = tokio::spawn({
        let cache = cache.clone();
        let load = slow_load.clone();
        async move { cache.get("slow", None, false, load).await }
    });
    tokio::task::yield_now().await;
    release.send(true).expect("release the slow load");
    assert_eq!(slow.await.expect("join the first slow caller").expect("the slow load"), "slow");
    assert_eq!(joined.await.expect("join the second slow caller").expect("the joined slow load"), "slow");
    assert_eq!(counts.lock().expect("the counts lock is never poisoned").get("slow").copied(), Some(1));
}

#[tokio::test(start_paused = true)]
async fn drops_entries_read_only_with_an_infinite_interval_after_24_hours() {
    let cache = SharedCache::new();
    let counts: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let infinite = Some(Duration::MAX);

    let first =
        cache.get("switched", infinite, false, counter(counts.clone(), "switched")).await.expect("the first load");
    assert_eq!(first, "switched");

    tokio::time::advance(Duration::from_secs(24 * 60 * 60 + 1)).await;
    let expired = cache
        .get("switched", infinite, false, counter(counts.clone(), "switched"))
        .await
        .expect_err("the infinite-interval entry expired");
    assert_eq!(expired, OFFLINE);
    assert_eq!(counts.lock().expect("the counts lock is never poisoned").get("switched").copied(), Some(2));
}
