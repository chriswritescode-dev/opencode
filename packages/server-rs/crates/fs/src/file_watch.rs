//! Port of `packages/core/src/filesystem/file-watch.ts` as a process-global service.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::generated::LocationRef;
use serde_json::json;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::watcher::{Subscription, WatchInput, WatchSource};

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const MAX_PER_LOCATION: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("File watching is disabled")]
pub struct DisabledError;

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub location: LocationRef,
    /// Absolute path of the file to watch.
    pub file: String,
}

pub struct FileWatch {
    inner: Option<Arc<Inner>>,
}

impl FileWatch {
    pub fn new(source: Arc<dyn WatchSource>, bus: Arc<Bus>, enabled: bool) -> FileWatch {
        if !enabled {
            return FileWatch { inner: None };
        }
        let (shutdown, _) = broadcast::channel(1);
        let inner = Arc::new(Inner {
            source,
            bus,
            state: Mutex::new(State::default()),
            shutdown,
            next_id: AtomicU64::new(0),
            sweep: StdMutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let mut shutdown_receiver = inner.shutdown.subscribe();
        let sweep = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
            loop {
                tokio::select! {
                    _ = ticks.tick() => {}
                    _ = shutdown_receiver.recv() => return,
                }
                let Some(inner) = weak.upgrade() else { return };
                inner.sweep().await;
            }
        });
        *inner.sweep.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sweep);
        FileWatch { inner: Some(inner) }
    }

    /// Starts watching a file, or resets its expiry when it is already watched.
    pub async fn watch(&self, input: Input) -> Result<(), DisabledError> {
        let Some(inner) = &self.inner else { return Err(DisabledError) };
        let key = (input.location.directory.0.clone(), input.location.workspace_id.as_ref().map(|id| id.0.clone()));
        let mut state = inner.state.lock().await;
        let expires = tokio::time::Instant::now() + TTL;
        state.remove_expired(&key);
        if let Some(entry) = state.locations.get_mut(&key).and_then(|files| files.get_mut(&input.file)) {
            entry.expires = expires;
            return Ok(());
        }
        state.evict_if_full(&key);
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (cancel, cancel_receiver) = oneshot::channel();
        state
            .locations
            .entry(key.clone())
            .or_default()
            .insert(input.file.clone(), Entry { expires, id, _cancel: cancel });
        drop(state);
        spawn_watch(inner, key, input, id, cancel_receiver);
        Ok(())
    }

    pub async fn unwatch(&self, input: Input) {
        let Some(inner) = &self.inner else { return };
        let key = (input.location.directory.0.clone(), input.location.workspace_id.as_ref().map(|id| id.0.clone()));
        let mut state = inner.state.lock().await;
        state.remove_file(&key, &input.file);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(sweep) = self.sweep.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            sweep.abort();
        }
    }
}

struct Inner {
    source: Arc<dyn WatchSource>,
    bus: Arc<Bus>,
    state: Mutex<State>,
    shutdown: broadcast::Sender<()>,
    next_id: AtomicU64,
    sweep: StdMutex<Option<JoinHandle<()>>>,
}

type LocationKey = (String, Option<String>);

#[derive(Default)]
struct State {
    locations: HashMap<LocationKey, HashMap<String, Entry>>,
}

struct Entry {
    expires: tokio::time::Instant,
    id: u64,
    _cancel: oneshot::Sender<()>,
}

impl State {
    fn remove_expired(&mut self, key: &LocationKey) {
        let now = tokio::time::Instant::now();
        let expired: Vec<String> = self
            .locations
            .get(key)
            .into_iter()
            .flatten()
            .filter(|(_, entry)| entry.expires <= now)
            .map(|(file, _)| file.clone())
            .collect();
        for file in expired {
            self.remove_file(key, &file);
        }
    }

    fn evict_if_full(&mut self, key: &LocationKey) {
        let Some(files) = self.locations.get_mut(key) else { return };
        if files.len() < MAX_PER_LOCATION {
            return;
        }
        let soonest = files.iter().min_by_key(|(_, entry)| entry.expires).map(|(file, _)| file.clone());
        if let Some(file) = soonest {
            files.remove(&file);
        }
    }

    fn remove_file(&mut self, key: &LocationKey, file: &str) {
        let empty = {
            let Some(files) = self.locations.get_mut(key) else { return };
            files.remove(file);
            files.is_empty()
        };
        if empty {
            self.locations.remove(key);
        }
    }

    fn remove_watch(&mut self, key: &LocationKey, file: &str, id: u64) {
        let empty = {
            let Some(files) = self.locations.get_mut(key) else { return };
            if !files.get(file).is_some_and(|entry| entry.id == id) {
                return;
            }
            files.remove(file);
            files.is_empty()
        };
        if empty {
            self.locations.remove(key);
        }
    }
}

impl Inner {
    async fn sweep(&self) {
        let mut state = self.state.lock().await;
        let keys: Vec<LocationKey> = state.locations.keys().cloned().collect();
        for key in keys {
            state.remove_expired(&key);
        }
    }
}

fn spawn_watch(inner: &Arc<Inner>, key: LocationKey, input: Input, id: u64, mut cancel: oneshot::Receiver<()>) {
    let source = inner.source.clone();
    let bus = inner.bus.clone();
    let state = Arc::downgrade(inner);
    let mut shutdown = inner.shutdown.subscribe();
    tokio::spawn(async move {
        let mut subscription: Subscription = source.subscribe(WatchInput::File { path: input.file.clone().into() });
        loop {
            tokio::select! {
                _ = &mut cancel => break,
                _ = shutdown.recv() => break,
                update = subscription.recv() => match update {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
            let exists = tokio::fs::try_exists(&input.file).await.unwrap_or(false);
            let data = json!({ "file": input.file, "event": if exists { "change" } else { "unlink" } });
            let options = PublishOptions { location: Some(input.location.clone()), ..Default::default() };
            if let Err(error) = bus.publish(&Definition::ephemeral("filesystem.changed"), data, options) {
                log::error!("failed to publish a file watch event: {error}");
                break;
            }
        }
        forget(state, key, input.file, id).await;
    });
}

async fn forget(state: Weak<Inner>, key: LocationKey, file: String, id: u64) {
    let Some(inner) = state.upgrade() else { return };
    let mut state = inner.state.lock().await;
    state.remove_watch(&key, &file, id);
}
