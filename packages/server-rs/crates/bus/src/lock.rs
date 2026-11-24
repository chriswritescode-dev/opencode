use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

pub struct KeyedMutex<K> {
    entries: Mutex<HashMap<K, Arc<Mutex<()>>>>,
}

impl<K> Default for KeyedMutex<K> {
    fn default() -> Self {
        KeyedMutex { entries: Mutex::new(HashMap::new()) }
    }
}

impl<K: Clone + Eq + Hash> KeyedMutex<K> {
    pub fn with<T>(&self, key: K, f: impl FnOnce() -> T) -> T {
        let entry = {
            let mut entries = self.entries.lock().unwrap_or_else(|error| error.into_inner());
            entries.entry(key.clone()).or_default().clone()
        };
        let value = {
            let _guard = entry.lock().unwrap_or_else(|error| error.into_inner());
            f()
        };
        let mut entries = self.entries.lock().unwrap_or_else(|error| error.into_inner());
        if Arc::strong_count(&entry) == 2 {
            entries.remove(&key);
        }
        drop(entry);
        value
    }
}
