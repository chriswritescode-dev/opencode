use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};

pub const INTERVAL: Duration = Duration::from_secs(10 * 60);

const UNREAD: Duration = Duration::from_secs(24 * 60 * 60);
const RETRIES: usize = 3;

pub trait Transient {
    fn is_transient(&self) -> bool;
}

impl Transient for crate::provider_plugin::http::HttpError {
    fn is_transient(&self) -> bool {
        self.transient()
    }
}

pub fn fingerprint(secret: &str) -> String {
    opencode_util::hash::sha256_hex(secret.as_bytes()).chars().take(16).collect()
}

type Erased = Arc<dyn Any + Send + Sync>;
type LoadFuture = Shared<BoxFuture<'static, Arc<Loaded>>>;

enum Loaded {
    Value(Erased),
    Panic(String),
}

struct Cached {
    value: Erased,
    failed: bool,
    time: tokio::time::Instant,
}

struct Entry {
    last: Option<Cached>,
    loading: Option<LoadFuture>,
    read: tokio::time::Instant,
    unread: Duration,
}

impl Entry {
    fn new(now: tokio::time::Instant) -> Self {
        Self { last: None, loading: None, read: now, unread: UNREAD }
    }
}

pub struct SharedCache {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl SharedCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { entries: Arc::new(Mutex::new(HashMap::new())) })
    }

    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<SharedCache>> = OnceLock::new();
        GLOBAL.get_or_init(SharedCache::new).clone()
    }

    pub async fn get<A, E, F, Fut>(&self, key: &str, interval: Option<Duration>, force: bool, load: F) -> Result<A, E>
    where
        A: Clone + Send + Sync + 'static,
        E: Transient + std::fmt::Debug + Clone + Send + Sync + 'static,
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
    {
        let now = tokio::time::Instant::now();
        let age = interval.unwrap_or(INTERVAL);
        let outcome = {
            let mut entries = self.entries.lock().expect("the shared cache lock is never poisoned");
            entries.retain(|_, entry| {
                entry.loading.is_some() || now.saturating_duration_since(entry.read) <= entry.unread
            });
            let entry = entries.entry(key.to_owned()).or_insert_with(|| Entry::new(now));
            entry.read = now;
            entry.unread = entry.unread.max(age.saturating_mul(2));
            let fresh =
                !force && entry.last.as_ref().is_some_and(|last| now.saturating_duration_since(last.time) < age);
            if fresh {
                Ok(entry.last.as_ref().expect("a fresh entry has a value").value.clone())
            } else {
                if entry.loading.is_none() {
                    entry.loading = Some(start(self.entries.clone(), key.to_owned(), load, now));
                }
                Err(entry.loading.clone().expect("a running load is stored"))
            }
        };
        match outcome {
            Ok(value) => read_value(value),
            Err(loading) => read_loaded(loading.await),
        }
    }
}

fn start<A, E, F, Fut>(
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    key: String,
    load: F,
    time: tokio::time::Instant,
) -> LoadFuture
where
    A: Send + Sync + 'static,
    E: Transient + std::fmt::Debug + Send + Sync + 'static,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<A, E>> + Send + 'static,
{
    let future: BoxFuture<'static, Arc<Loaded>> =
        Box::pin(async move { settle(&entries, &key, time, AssertUnwindSafe(attempt(load)).catch_unwind().await) });
    let shared = future.shared();
    tokio::spawn({
        let shared = shared.clone();
        async move {
            shared.await;
        }
    });
    shared
}

async fn attempt<A, E, F, Fut>(load: F) -> Result<A, E>
where
    E: Transient,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<A, E>>,
{
    let mut delay = Duration::from_secs(1);
    for retry in 0..=RETRIES {
        match load().await {
            Ok(value) => return Ok(value),
            Err(error) if !error.is_transient() || retry == RETRIES => return Err(error),
            Err(_) => {}
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2);
    }
    unreachable!("the final attempt returns")
}

fn settle<A, E>(
    entries: &Mutex<HashMap<String, Entry>>,
    key: &str,
    time: tokio::time::Instant,
    outcome: Result<Result<A, E>, Box<dyn Any + Send>>,
) -> Arc<Loaded>
where
    A: Send + Sync + 'static,
    E: std::fmt::Debug + Send + Sync + 'static,
{
    let mut entries = entries.lock().expect("the shared cache lock is never poisoned");
    let entry = entries.get_mut(key).expect("a loading entry is never dropped");
    entry.loading = None;
    entry.read = tokio::time::Instant::now();
    match outcome {
        Err(payload) => Arc::new(Loaded::Panic(panic_message(payload))),
        Ok(Ok(value)) => {
            let value: Erased = Arc::new(Ok::<A, E>(value));
            entry.last = Some(Cached { value: value.clone(), failed: false, time });
            Arc::new(Loaded::Value(value))
        }
        Ok(Err(error)) => {
            log::warn!("failed to load shared value for {key}: {error:?}");
            if entry.last.as_ref().is_some_and(|last| !last.failed) {
                let last = entry.last.as_mut().expect("the kept value exists");
                last.time = time;
                last.failed = false;
                return Arc::new(Loaded::Value(last.value.clone()));
            }
            let value: Erased = Arc::new(Err::<A, E>(error));
            entry.last = Some(Cached { value: value.clone(), failed: true, time });
            Arc::new(Loaded::Value(value))
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "the shared cache load panicked".to_owned()
}

fn read_loaded<A, E>(loaded: Arc<Loaded>) -> Result<A, E>
where
    A: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    match loaded.as_ref() {
        Loaded::Value(value) => read_value(value.clone()),
        Loaded::Panic(message) => panic!("{message}"),
    }
}

fn read_value<A, E>(value: Erased) -> Result<A, E>
where
    A: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    let result = value
        .downcast::<Result<A, E>>()
        .unwrap_or_else(|_| panic!("a shared cache key was reused with a different value type"));
    match result.as_ref() {
        Ok(value) => Ok(value.clone()),
        Err(error) => Err(error.clone()),
    }
}
