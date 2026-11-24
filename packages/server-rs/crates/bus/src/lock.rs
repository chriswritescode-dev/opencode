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
        let entry =
            self.entries.lock().unwrap_or_else(|error| error.into_inner()).entry(key.clone()).or_default().clone();
        let release = Release { mutex: self, key, entry };
        let _guard = release.entry.lock().unwrap_or_else(|error| error.into_inner());
        f()
    }
}

struct Release<'a, K: Eq + Hash> {
    mutex: &'a KeyedMutex<K>,
    key: K,
    entry: Arc<Mutex<()>>,
}

impl<K: Eq + Hash> Drop for Release<'_, K> {
    fn drop(&mut self) {
        let mut entries = self.mutex.entries.lock().unwrap_or_else(|error| error.into_inner());
        if Arc::strong_count(&self.entry) == 2 {
            entries.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::KeyedMutex;

    #[test]
    fn a_released_key_leaves_no_entry() {
        let mutex = KeyedMutex::default();
        assert_eq!(mutex.with("a", || 1), 1);
        assert!(mutex.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn a_panicking_holder_leaves_no_entry_and_the_key_stays_usable() {
        let mutex = KeyedMutex::default();
        let result = catch_unwind(AssertUnwindSafe(|| mutex.with("a", || panic!("holder failed"))));
        assert!(result.is_err());
        assert!(mutex.entries.lock().unwrap().is_empty());
        assert_eq!(mutex.with("a", || 2), 2);
    }
}
