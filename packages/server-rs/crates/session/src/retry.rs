use std::time::Duration;

use opencode_ai::{AiError, AiErrorReason};
use opencode_contract::generated::{SessionStructuredError, SessionStructuredErrorResponse};

use crate::context::ContextError;
use crate::execution::ExecutionError;

const BASE_DELAY_MS: f64 = 2_000.0;
const GAP_MAX_MS: f64 = 10_000.0;
const RETRY_AFTER_MAX_MS: u64 = 900_000;
const MAX_RETRIES: u64 = 10;
const MAX_TIMEOUT_RETRIES: u64 = 3;
const UNREACHABLE_GAP_MAX_MS: f64 = 30_000.0;
const UNREACHABLE_WINDOW_MS: u128 = 3_600_000;
const UNREACHABLE_CODES: [&str; 5] = ["ENOTFOUND", "EAI_AGAIN", "ENETUNREACH", "EHOSTUNREACH", "ENETDOWN"];

const TOKEN_SHARING_MESSAGES: [(&str, &str); 10] = [
    (
        "subscription_sharing_user_not_eligible",
        "ChatGPT token sharing isn't available for this account. Connect with an API key or choose another provider.",
    ),
    (
        "subscription_sharing_usage_limit_exceeded",
        "ChatGPT usage limit reached. Check ChatGPT Settings → Usage for details.",
    ),
    (
        "subscription_sharing_workplace_individual_usage_limit_exceeded",
        "You've reached the usage limit your ChatGPT workspace admin set for you. Ask them to raise it in the OpenAI Admin Console.",
    ),
    ("subscription_sharing_usage_unavailable", "ChatGPT usage can't be checked right now. Try again later."),
    (
        "subscription_sharing_unsupported_capability",
        "This request uses a feature ChatGPT token sharing doesn't support. Remove the unsupported feature and try again.",
    ),
    (
        "subscription_sharing_route_not_supported",
        "ChatGPT token sharing doesn't support this API route. Check the configured endpoint and HTTP method.",
    ),
    ("subscription_sharing_invalid_user", "This ChatGPT connection is no longer valid. Reconnect to ChatGPT."),
    ("subscription_sharing_user_unavailable", "Your ChatGPT account is temporarily unavailable. Try again later."),
    (
        "chatpass_v2_scope_not_authorized",
        "This ChatGPT connection isn't authorized for this request. Reconnect to ChatGPT or choose another connection.",
    ),
    (
        "chatpass_v2_invalid_authorization_context",
        "This ChatGPT connection isn't authorized for this request. Reconnect to ChatGPT or choose another connection.",
    ),
];

const FREE_USAGE_LIMIT_MESSAGE: &str =
    "Free usage limit reached. Connect a provider or subscribe to OpenCode Go to keep going, or switch models.";

pub struct RetryDecision {
    pub attempt: u64,
    pub delay: Duration,
}

pub struct RetryPolicy {
    attempt: u64,
    timeouts: u64,
    unreachable: Option<(std::time::Instant, u32)>,
}

impl RetryPolicy {
    pub fn new() -> Self {
        Self { attempt: 1, timeouts: 0, unreachable: None }
    }

    pub fn decide(&mut self, error: &AiError, retryable: bool) -> Option<RetryDecision> {
        if !retryable {
            return None;
        }
        if is_unreachable(error) {
            let now = std::time::Instant::now();
            let (start, attempts) = self.unreachable.unwrap_or((now, 0));
            if now.duration_since(start).as_millis() >= UNREACHABLE_WINDOW_MS {
                return None;
            }
            let gap = (BASE_DELAY_MS * 2f64.powi(attempts as i32)).min(UNREACHABLE_GAP_MAX_MS);
            let jittered = (gap * (0.8 + rand::random::<f64>() * 0.4)).min(UNREACHABLE_GAP_MAX_MS);
            self.unreachable = Some((start, attempts + 1));
            self.attempt += 1;
            return Some(RetryDecision { attempt: self.attempt, delay: Duration::from_millis(jittered.ceil() as u64) });
        }
        if self.attempt > MAX_RETRIES {
            return None;
        }
        let gap = (BASE_DELAY_MS * 2f64.powi(self.attempt as i32 - 1)).min(GAP_MAX_MS);
        let jittered = gap * (0.8 + rand::random::<f64>() * 0.4);
        let delay =
            requested_delay(error).map_or(jittered, |requested| jittered.max(requested.min(RETRY_AFTER_MAX_MS) as f64));
        self.attempt += 1;
        if is_timeout(error) {
            self.timeouts += 1;
            if self.timeouts > MAX_TIMEOUT_RETRIES {
                return None;
            }
        }
        Some(RetryDecision { attempt: self.attempt, delay: Duration::from_millis(delay.ceil() as u64) })
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

pub fn structured(error: &ExecutionError) -> SessionStructuredError {
    match error {
        ExecutionError::Ai(error) => structured_ai(error),
        ExecutionError::OutputLimit => plain("output-limit", error.to_string()),
        ExecutionError::Interrupted => plain("aborted", error.to_string()),
        ExecutionError::Context(ContextError::ModelNotSelected { .. } | ContextError::ModelUnavailable { .. }) => {
            plain("provider.no-route", error.to_string())
        }
        ExecutionError::Context(_)
        | ExecutionError::Placement(_)
        | ExecutionError::Message(_)
        | ExecutionError::Bus(_) => plain("unknown", error.to_string()),
    }
}

fn requested_delay(error: &AiError) -> Option<u64> {
    match &error.reason {
        AiErrorReason::RateLimit { .. } | AiErrorReason::ProviderInternal { .. } => error.retry_after_ms(),
        _ => None,
    }
}

fn is_timeout(error: &AiError) -> bool {
    match &error.reason {
        AiErrorReason::Transport { code: Some(code), .. } => node_code(code) == "Timeout",
        _ => false,
    }
}

fn is_unreachable(error: &AiError) -> bool {
    let AiErrorReason::Transport { code: Some(code), .. } = &error.reason else {
        return false;
    };
    UNREACHABLE_CODES.contains(&node_code(code))
}

fn node_code(code: &str) -> &str {
    match code {
        "TimedOut" => "Timeout",
        "ConnectionRefused" => "ECONNREFUSED",
        "NetworkUnreachable" => "ENETUNREACH",
        "HostUnreachable" => "EHOSTUNREACH",
        "NetworkDown" => "ENETDOWN",
        other => other,
    }
}

fn structured_ai(error: &AiError) -> SessionStructuredError {
    SessionStructuredError {
        r#type: ai_type(error).to_owned(),
        message: ai_message(error),
        status: error.status().map(f64::from),
        response: error.body().map(|body| SessionStructuredErrorResponse { body: body.to_owned() }),
        retry_after_ms: error.retry_after_ms().map(|value| value as f64),
    }
}

fn ai_type(error: &AiError) -> &'static str {
    match &error.reason {
        AiErrorReason::RateLimit { .. } => "provider.rate-limit",
        AiErrorReason::Authentication { .. } => "provider.auth",
        AiErrorReason::QuotaExceeded { .. } if is_free_usage_limit(error) => "provider.free-limit",
        AiErrorReason::QuotaExceeded { .. } => "provider.quota",
        AiErrorReason::ContentPolicy { .. } => "provider.content-filter",
        AiErrorReason::Transport { .. } => "provider.transport",
        AiErrorReason::ProviderInternal { .. } => "provider.internal",
        AiErrorReason::InvalidProviderOutput { .. } => "provider.invalid-output",
        AiErrorReason::InvalidRequest { .. } => "provider.invalid-request",
        AiErrorReason::UnsupportedOperation { .. } => "provider.unsupported-operation",
        AiErrorReason::UnknownProvider { .. } => "provider.unknown",
        AiErrorReason::Timeout { .. } => "provider.timeout",
    }
}

fn ai_message(error: &AiError) -> String {
    if is_free_usage_limit(error) {
        return FREE_USAGE_LIMIT_MESSAGE.to_owned();
    }
    sharing_message(error).map_or_else(|| error.message().to_owned(), str::to_owned)
}

fn is_free_usage_limit(error: &AiError) -> bool {
    matches!(&error.reason, AiErrorReason::QuotaExceeded { .. })
        && error.body().is_some_and(|body| body.contains("FreeUsageLimitError"))
}

fn sharing_message(error: &AiError) -> Option<&'static str> {
    let body = error.body();
    TOKEN_SHARING_MESSAGES.iter().find_map(|(code, message)| {
        let shared = body.is_some_and(|body| body.contains(*code)) || error.message().contains(*code);
        shared.then_some(*message)
    })
}

fn plain(r#type: &str, message: String) -> SessionStructuredError {
    SessionStructuredError { r#type: r#type.to_owned(), message, status: None, response: None, retry_after_ms: None }
}
