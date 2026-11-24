use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::future::{BoxFuture, join_all};
use indexmap::IndexMap;
use tokio::sync::{Mutex, Notify, watch};

use crate::lifecycle::Lifecycle;
use crate::location::{BootError, LocationRef};

const HELD_WARNING_AFTER: Duration = Duration::from_secs(30);

type BootFn<T> = Box<dyn Fn(LocationRef) -> BoxFuture<'static, Result<Arc<T>, BootError>> + Send + Sync>;

pub trait LocationServices: Send + Sync + 'static {
    fn lifecycle(&self) -> &Lifecycle;
    fn close(&self) -> BoxFuture<'_, ()>;
}

pub struct LocationMap<T: LocationServices> {
    inner: Arc<Inner<T>>,
}

impl<T: LocationServices> LocationMap<T> {
    pub fn new(
        boot: impl Fn(LocationRef) -> BoxFuture<'static, Result<Arc<T>, BootError>> + Send + Sync + 'static,
    ) -> Self {
        LocationMap {
            inner: Arc::new(Inner {
                boot: Box::new(boot),
                state: Mutex::new(MapState { slots: IndexMap::new(), closed: false }),
            }),
        }
    }

    pub async fn get(&self, reference: &LocationRef) -> Result<Lease<T>, BootError> {
        let key = reference.canonical();
        loop {
            let slot = {
                let mut state = self.inner.state.lock().await;
                if state.closed {
                    return Err(BootError::Closed);
                }
                match state.slots.get(&key) {
                    Some(Slot::Ready(entry)) => return Ok(entry.lease()),
                    Some(Slot::Booting(slot)) => slot.clone(),
                    None => {
                        let slot = Arc::new(BootSlot::new());
                        state.slots.insert(key.clone(), Slot::Booting(slot.clone()));
                        spawn_boot(self.inner.clone(), key.clone(), slot.clone());
                        slot
                    }
                }
            };
            match slot.wait().await {
                BootOutcome::Failed(error) => return Err(error),
                BootOutcome::Ready(entry) => {
                    let state = self.inner.state.lock().await;
                    if !matches!(state.slots.get(&key), Some(Slot::Ready(current)) if Arc::ptr_eq(current, &entry)) {
                        drop(state);
                        continue;
                    }
                    return Ok(entry.lease());
                }
            }
        }
    }

    pub async fn invalidate(&self, reference: &LocationRef) {
        let key = reference.canonical();
        let entry = match self.inner.state.lock().await.slots.shift_remove(&key) {
            Some(Slot::Ready(entry)) => entry,
            _ => return,
        };
        entry.invalidated.store(true, Ordering::SeqCst);
        let _ = tokio::spawn(release_entry(entry, key)).await;
    }

    pub async fn reload(&self) -> Result<(), BootError> {
        let keys = self.keys().await;
        join_all(keys.iter().map(|key| self.invalidate(key))).await;
        let results = join_all(keys.iter().map(|key| self.get(key))).await;
        if let Some(error) = results.into_iter().find_map(Result::err) {
            return Err(error);
        }
        log::info!("location services reloaded");
        Ok(())
    }

    pub async fn keys(&self) -> Vec<LocationRef> {
        self.inner.state.lock().await.slots.keys().cloned().collect()
    }

    pub async fn has(&self, reference: &LocationRef) -> bool {
        let key = reference.canonical();
        self.inner.state.lock().await.slots.contains_key(&key)
    }

    pub async fn shutdown_all(&self) {
        let ready = {
            let mut state = self.inner.state.lock().await;
            state.closed = true;
            state
                .slots
                .drain(..)
                .filter_map(|(_, slot)| match slot {
                    Slot::Ready(entry) => Some(entry),
                    Slot::Booting(_) => None,
                })
                .collect::<Vec<_>>()
        };
        let closings = ready.into_iter().filter_map(|entry| {
            entry.invalidated.store(true, Ordering::SeqCst);
            if !entry.claim_close() {
                return None;
            }
            Some(async move {
                entry.services.lifecycle().shutdown().await;
                entry.services.close().await;
            })
        });
        join_all(closings).await;
    }
}

pub struct Lease<T: LocationServices> {
    entry: Arc<Entry<T>>,
}

impl<T: LocationServices> Lease<T> {
    pub fn services(&self) -> &Arc<T> {
        &self.entry.services
    }
}

impl<T: LocationServices> Deref for Lease<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.entry.services
    }
}

impl<T: LocationServices> Drop for Lease<T> {
    fn drop(&mut self) {
        if self.entry.leases.fetch_sub(1, Ordering::SeqCst) > 1 {
            return;
        }
        self.entry.released.notify_one();
        if !self.entry.invalidated.load(Ordering::SeqCst) {
            return;
        }
        self.entry.spawn_close();
    }
}

struct Inner<T: LocationServices> {
    boot: BootFn<T>,
    state: Mutex<MapState<T>>,
}

struct MapState<T: LocationServices> {
    slots: IndexMap<LocationRef, Slot<T>>,
    closed: bool,
}

enum Slot<T: LocationServices> {
    Booting(Arc<BootSlot<T>>),
    Ready(Arc<Entry<T>>),
}

struct Entry<T: LocationServices> {
    services: Arc<T>,
    leases: AtomicUsize,
    invalidated: AtomicBool,
    closed: AtomicBool,
    released: Notify,
}

impl<T: LocationServices> Entry<T> {
    fn new(services: Arc<T>) -> Self {
        Entry {
            services,
            leases: AtomicUsize::new(0),
            invalidated: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            released: Notify::new(),
        }
    }

    fn lease(self: &Arc<Self>) -> Lease<T> {
        self.leases.fetch_add(1, Ordering::SeqCst);
        Lease { entry: self.clone() }
    }

    fn claim_close(&self) -> bool {
        !self.closed.swap(true, Ordering::SeqCst)
    }

    fn spawn_close(self: &Arc<Self>) {
        if !self.claim_close() {
            return;
        }
        let entry = self.clone();
        tokio::spawn(async move {
            entry.services.close().await;
        });
    }
}

struct BootSlot<T: LocationServices> {
    outcome: watch::Sender<Option<BootOutcome<T>>>,
}

impl<T: LocationServices> BootSlot<T> {
    fn new() -> Self {
        let (outcome, _) = watch::channel(None);
        BootSlot { outcome }
    }

    fn finish(&self, outcome: BootOutcome<T>) {
        self.outcome.send_replace(Some(outcome));
    }

    async fn wait(&self) -> BootOutcome<T> {
        let mut receiver = self.outcome.subscribe();
        let Ok(value) = receiver.wait_for(|value| value.is_some()).await else {
            return BootOutcome::Failed(BootError::Invalidated);
        };
        match value.clone() {
            Some(outcome) => outcome,
            None => BootOutcome::Failed(BootError::Invalidated),
        }
    }
}

enum BootOutcome<T: LocationServices> {
    Ready(Arc<Entry<T>>),
    Failed(BootError),
}

impl<T: LocationServices> Clone for BootOutcome<T> {
    fn clone(&self) -> Self {
        match self {
            BootOutcome::Ready(entry) => BootOutcome::Ready(entry.clone()),
            BootOutcome::Failed(error) => BootOutcome::Failed(error.clone()),
        }
    }
}

fn spawn_boot<T: LocationServices>(inner: Arc<Inner<T>>, key: LocationRef, slot: Arc<BootSlot<T>>) {
    tokio::spawn(async move {
        let result = (inner.boot)(key.clone()).await;
        finish_boot(inner, key, slot, result).await;
    });
}

async fn finish_boot<T: LocationServices>(
    inner: Arc<Inner<T>>,
    key: LocationRef,
    slot: Arc<BootSlot<T>>,
    result: Result<Arc<T>, BootError>,
) {
    let mut state = inner.state.lock().await;
    let installed =
        !state.closed && matches!(state.slots.get(&key), Some(Slot::Booting(current)) if Arc::ptr_eq(current, &slot));
    if installed {
        match result {
            Ok(services) => {
                let entry = Arc::new(Entry::new(services));
                state.slots.insert(key, Slot::Ready(entry.clone()));
                drop(state);
                slot.finish(BootOutcome::Ready(entry));
                return;
            }
            Err(error) => {
                state.slots.shift_remove(&key);
                drop(state);
                slot.finish(BootOutcome::Failed(error));
                return;
            }
        }
    }
    drop(state);
    slot.finish(BootOutcome::Failed(BootError::Invalidated));
    if let Ok(services) = result {
        services.lifecycle().shutdown().await;
        services.close().await;
    }
}

async fn release_entry<T: LocationServices>(entry: Arc<Entry<T>>, key: LocationRef) {
    entry.services.lifecycle().shutdown().await;
    if entry.leases.load(Ordering::SeqCst) > 0 {
        spawn_held_warning(entry, key);
        return;
    }
    if !entry.claim_close() {
        return;
    }
    entry.services.close().await;
}

fn spawn_held_warning<T: LocationServices>(entry: Arc<Entry<T>>, key: LocationRef) {
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(HELD_WARNING_AFTER) => {}
            _ = entry.released.notified() => return,
        }
        let holders = entry.leases.load(Ordering::SeqCst);
        if holders == 0 {
            return;
        }
        log::warn!(
            "location still held after shutdown directory={} workspace_id={:?} holders={}",
            key.directory,
            key.workspace_id,
            holders
        );
    });
}
