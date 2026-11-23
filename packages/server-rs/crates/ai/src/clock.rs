use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    now_ms: u64,
}

impl FixedClock {
    pub fn new(now_ms: u64) -> Self {
        Self { now_ms }
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.now_ms
    }
}
