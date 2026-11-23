use std::collections::HashMap;
use std::sync::Mutex;

use crate::Clock;

struct Entry<V> {
    value: V,
    expires_at: u64,
}

struct State<V> {
    entries: HashMap<String, Entry<V>>,
    order: Vec<String>,
}

impl<V> State<V> {
    fn remove_expired(&mut self, now: u64) {
        let entries = &self.entries;
        self.order.retain(|key| entries.get(key).is_some_and(|entry| entry.expires_at > now));
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    fn remove(&mut self, key: &str) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.order.retain(|existing| existing != key);
        Some(entry.value)
    }
}

pub struct ExpiringMap<V> {
    capacity: usize,
    ttl_ms: u64,
    clock: Clock,
    state: Mutex<State<V>>,
}

impl<V> ExpiringMap<V> {
    pub fn new(capacity: usize, ttl_ms: u64, clock: Clock) -> Self {
        Self { capacity, ttl_ms, clock, state: Mutex::new(State { entries: HashMap::new(), order: Vec::new() }) }
    }

    pub fn insert(&self, key: String, value: V) {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.remove_expired(now);
        if !state.entries.contains_key(&key) {
            while state.entries.len() >= self.capacity {
                let Some(oldest) = state.order.first().cloned() else { break };
                state.remove(&oldest);
            }
            state.order.push(key.clone());
        }
        state.entries.insert(key, Entry { value, expires_at: now.saturating_add(self.ttl_ms) });
    }

    pub fn take(&self, key: &str, matches: impl FnOnce(&V) -> bool) -> bool {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.entries.get(key).is_some_and(|entry| entry.expires_at <= now) {
            state.remove(key);
            return false;
        }
        let Some(entry) = state.entries.get(key) else { return false };
        if !matches(&entry.value) {
            return false;
        }
        state.remove(key);
        true
    }
}
