//! One async lock per key, with entries removed once no holder or waiter remains.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

struct Slot {
    lock: Arc<AsyncMutex<()>>,
    users: usize,
}

struct Inner<Key> {
    entries: Mutex<HashMap<Key, Slot>>,
}

/// A table of async locks keyed by user-chosen values.
pub struct KeyedMutex<Key> {
    inner: Arc<Inner<Key>>,
}

impl<Key> Clone for KeyedMutex<Key> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<Key> Default for KeyedMutex<Key>
where
    Key: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Key> KeyedMutex<Key>
where
    Key: Clone + Eq + Hash,
{
    pub fn new() -> Self {
        Self { inner: Arc::new(Inner { entries: Mutex::new(HashMap::new()) }) }
    }

    /// The number of keys with a holder or waiter.
    pub fn size(&self) -> usize {
        self.entries().len()
    }

    /// Waits for the lock on `key`, then holds it until the guard is dropped.
    pub async fn lock(&self, key: Key) -> KeyedMutexGuard<Key> {
        let lock = {
            let mut entries = self.entries();
            let slot =
                entries.entry(key.clone()).or_insert_with(|| Slot { lock: Arc::new(AsyncMutex::new(())), users: 0 });
            slot.users += 1;
            slot.lock.clone()
        };
        // The placeholder guard counts the waiter, so cancelling the wait removes it again.
        let mut guard = KeyedMutexGuard { inner: self.inner.clone(), key, held: None };
        guard.held = Some(lock.lock_owned().await);
        guard
    }

    /// Locks every key in sorted order, so callers taking overlapping sets cannot deadlock.
    pub async fn lock_all(&self, keys: impl IntoIterator<Item = Key>) -> Vec<KeyedMutexGuard<Key>>
    where
        Key: Ord,
    {
        let mut keys: Vec<Key> = keys.into_iter().collect();
        keys.sort();
        keys.dedup();
        let mut guards = Vec::with_capacity(keys.len());
        for key in keys {
            guards.push(self.lock(key).await);
        }
        guards
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
        self.inner.entries.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Holds one key's lock. Dropping it lets the next waiter in and removes an unused entry.
pub struct KeyedMutexGuard<Key: Eq + Hash> {
    inner: Arc<Inner<Key>>,
    key: Key,
    held: Option<OwnedMutexGuard<()>>,
}

impl<Key: Eq + Hash> Drop for KeyedMutexGuard<Key> {
    fn drop(&mut self) {
        drop(self.held.take());
        let mut entries = self.inner.entries.lock().unwrap_or_else(|error| error.into_inner());
        let Some(slot) = entries.get_mut(&self.key) else { return };
        slot.users -= 1;
        if slot.users == 0 {
            entries.remove(&self.key);
        }
    }
}
