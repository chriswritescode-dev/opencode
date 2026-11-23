use std::sync::Arc;

pub mod app;
pub mod auth;
mod expiry;
pub mod pairing;
pub mod response;
pub mod router;
pub mod serve;
pub mod status;
pub mod ticket;

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn clock() -> Clock {
    Arc::new(now_ms)
}
