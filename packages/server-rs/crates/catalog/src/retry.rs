use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::clock::{Clock, SystemClock};
use opencode_ai::{AiError, AiErrorReason, Delivery};

pub trait Random: Send + Sync {
    fn next_f64(&self) -> f64;
}

pub struct SeededRandom {
    state: Mutex<u64>,
}

impl SeededRandom {
    pub fn new(seed: u64) -> Self {
        Self { state: Mutex::new(seed.max(1)) }
    }
}

impl Random for SeededRandom {
    fn next_f64(&self) -> f64 {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut value = *state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        *state = value;
        (value.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct ThreadRandom;

impl Random for ThreadRandom {
    fn next_f64(&self) -> f64 {
        rand::random::<f64>()
    }
}

pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(15 * 60);
pub const UNREACHABLE_WINDOW: Duration = Duration::from_secs(60 * 60);
pub const UNREACHABLE_GAP_MAX: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT_RETRIES: u32 = 3;
pub const ONE_SHOT_RETRIES: u32 = 3;

pub struct Decision {
    pub attempt: u32,
    pub delay: Duration,
    pub retry: bool,
}

pub fn is_unreachable(error: &AiError) -> bool {
    matches!(
        &error.reason,
        AiErrorReason::Transport { code: Some(code), .. }
            if matches!(code.as_str(), "ENOTFOUND" | "EAI_AGAIN" | "ENETUNREACH" | "EHOSTUNREACH" | "ENETDOWN")
    )
}

pub struct Policy {
    random: Arc<dyn Random>,
    decisions: u32,
    unreachable_decisions: u32,
    unreachable_start: Option<u64>,
    attempt: u32,
    timeouts: u32,
}

impl Policy {
    pub fn new(random: Arc<dyn Random>) -> Self {
        Self { random, decisions: 0, unreachable_decisions: 0, unreachable_start: None, attempt: 1, timeouts: 0 }
    }

    pub fn decide(&mut self, error: &AiError, retry: bool, now_ms: u64) -> Option<Decision> {
        let delay = if retry && is_unreachable(error) {
            self.unreachable_decisions += 1;
            let start = *self.unreachable_start.get_or_insert(now_ms);
            if now_ms.saturating_sub(start) > UNREACHABLE_WINDOW.as_millis() as u64 {
                return None;
            }
            let base = 2_000u64
                .saturating_mul(1u64 << (self.unreachable_decisions - 1).min(63))
                .min(UNREACHABLE_GAP_MAX.as_millis() as u64);
            jittered(base, self.random.next_f64()).min(UNREACHABLE_GAP_MAX.as_millis() as u64)
        } else {
            self.decisions += 1;
            if self.decisions > 10 {
                return None;
            }
            let base = 2_000u64.saturating_mul(1u64 << (self.decisions - 1)).min(10_000);
            let delay = jittered(base, self.random.next_f64());
            match &error.reason {
                AiErrorReason::RateLimit { .. } | AiErrorReason::ProviderInternal { .. } => error
                    .retry_after_ms()
                    .map_or(delay, |retry_after| delay.max(retry_after.min(RETRY_AFTER_MAX.as_millis() as u64))),
                _ => delay,
            }
        };
        self.attempt += 1;
        if matches!(&error.reason, AiErrorReason::Transport { code: Some(code), .. } if code == "Timeout") {
            self.timeouts += 1;
        }
        Some(Decision {
            attempt: self.attempt,
            delay: Duration::from_millis(delay),
            retry: retry && self.timeouts <= MAX_TIMEOUT_RETRIES,
        })
    }
}

fn jittered(base_ms: u64, random: f64) -> u64 {
    (base_ms as f64 * (0.8 + 0.4 * random)).ceil() as u64
}

#[allow(clippy::result_large_err)]
pub async fn retry<T, F, Fut>(policy: &mut Policy, mut operation: F) -> Result<T, AiError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AiError>>,
{
    loop {
        let error = match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(decision) = policy.decide(&error, error.is_retryable(), SystemClock.now_ms()) else {
            return Err(error);
        };
        if !decision.retry {
            return Err(error);
        }
        tokio::time::sleep(decision.delay).await;
    }
}

fn not_run(error: &AiError) -> bool {
    let override_value = error.http_context().and_then(|http| {
        http.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-should-retry"))
            .map(|(_, value)| value.as_str())
    });
    match override_value {
        Some("true") => true,
        Some("false") => false,
        _ => match &error.reason {
            AiErrorReason::RateLimit { .. } => true,
            AiErrorReason::ProviderInternal { http, .. } => {
                matches!(http.as_ref().map(|http| http.status), Some(503 | 529))
            }
            AiErrorReason::Transport { delivery, code, .. } => {
                *delivery == Some(Delivery::NotSent)
                    || code.as_deref().is_some_and(|code| matches!(code, "ECONNREFUSED" | "ENOTFOUND" | "EAI_AGAIN"))
            }
            _ => false,
        },
    }
}

#[allow(clippy::result_large_err)]
pub async fn retry_one_shot<T, F, Fut>(policy: &mut Policy, mut operation: F) -> Result<T, AiError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AiError>>,
{
    let mut retries = 0u32;
    loop {
        let error = match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(decision) = policy.decide(&error, not_run(&error), SystemClock.now_ms()) else {
            return Err(error);
        };
        if !decision.retry || retries >= ONE_SHOT_RETRIES {
            return Err(error);
        }
        retries += 1;
        tokio::time::sleep(decision.delay).await;
    }
}
