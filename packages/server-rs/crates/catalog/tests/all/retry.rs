use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use opencode_ai::{AiError, AiErrorReason, Delivery, HttpContext, TransportKind, TransportOperation};
use opencode_catalog::retry::{
    MAX_TIMEOUT_RETRIES, ONE_SHOT_RETRIES, Policy, RETRY_AFTER_MAX, Random, SeededRandom, UNREACHABLE_GAP_MAX,
    UNREACHABLE_WINDOW, is_unreachable, retry, retry_one_shot,
};

struct FixedRandom(f64);

impl Random for FixedRandom {
    fn next_f64(&self) -> f64 {
        self.0
    }
}

fn http_context(status: u16, headers: BTreeMap<String, String>) -> HttpContext {
    HttpContext { url: "https://opencode.ai/x".into(), status, headers }
}

fn provider_internal() -> AiError {
    AiError {
        reason: AiErrorReason::ProviderInternal {
            message: "provider internal".into(),
            body: None,
            http: None,
            retry_after_ms: None,
        },
    }
}

fn provider_internal_status(status: u16) -> AiError {
    AiError {
        reason: AiErrorReason::ProviderInternal {
            message: "provider internal".into(),
            body: None,
            http: Some(http_context(status, BTreeMap::new())),
            retry_after_ms: None,
        },
    }
}

fn rate_limit(retry_after_ms: Option<u64>, headers: BTreeMap<String, String>) -> AiError {
    AiError {
        reason: AiErrorReason::RateLimit {
            message: "slow down".into(),
            body: None,
            http: Some(http_context(429, headers)),
            retry_after_ms,
            rate_limit: None,
        },
    }
}

fn transport(code: &str, operation: TransportOperation, delivery: Option<Delivery>) -> AiError {
    AiError {
        reason: AiErrorReason::Transport {
            message: "transport failed".into(),
            body: None,
            http: None,
            transport: TransportKind::Http,
            operation,
            code: Some(code.into()),
            url: None,
            phase: None,
            delivery,
            recovery: None,
        },
    }
}

fn transport_without_code(operation: TransportOperation) -> AiError {
    AiError {
        reason: AiErrorReason::Transport {
            message: "transport failed".into(),
            body: None,
            http: None,
            transport: TransportKind::Http,
            operation,
            code: None,
            url: None,
            phase: None,
            delivery: None,
            recovery: None,
        },
    }
}

fn invalid_request() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "invalid request".into(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

fn header(name: &str, value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(name.to_owned(), value.to_owned())])
}

async fn one_shot_calls(error: AiError) -> (Result<(), AiError>, u32) {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let calls = Arc::new(AtomicU32::new(0));
    let outcome: Result<(), AiError> = retry_one_shot(&mut policy, || {
        let calls = Arc::clone(&calls);
        let error = error.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(error)
        }
    })
    .await;
    (outcome, calls.load(Ordering::SeqCst))
}

#[test]
fn constants_match_the_typescript() {
    assert_eq!(RETRY_AFTER_MAX, Duration::from_secs(15 * 60));
    assert_eq!(UNREACHABLE_WINDOW, Duration::from_secs(60 * 60));
    assert_eq!(UNREACHABLE_GAP_MAX, Duration::from_secs(30));
    assert_eq!(MAX_TIMEOUT_RETRIES, 3);
    assert_eq!(ONE_SHOT_RETRIES, 3);
}

#[test]
fn general_allowance_gives_ten_retries() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let error = provider_internal();
    let expected = [2_000u64, 4_000, 8_000, 10_000, 10_000, 10_000, 10_000, 10_000, 10_000, 10_000];
    for (index, delay) in expected.iter().enumerate() {
        let decision = policy.decide(&error, true, 0).expect("a decision");
        assert!(decision.retry);
        assert_eq!(decision.delay, Duration::from_millis(*delay));
        assert_eq!(decision.attempt, index as u32 + 2);
    }
    assert!(policy.decide(&error, true, 0).is_none());
}

#[test]
fn timeouts_stop_after_three_retries() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let error = transport("Timeout", TransportOperation::Request, None);
    let expected = [true, true, true, false];
    for retry in expected {
        let decision = policy.decide(&error, true, 0).expect("a decision");
        assert_eq!(decision.retry, retry);
    }
}

#[test]
fn refused_connection_uses_the_general_allowance() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let error = transport("ECONNREFUSED", TransportOperation::Request, None);
    assert!(!is_unreachable(&error));
    for _ in 0..10 {
        assert!(policy.decide(&error, true, 0).expect("a decision").retry);
    }
    assert!(policy.decide(&error, true, 0).is_none());
}

#[test]
fn unreachable_host_retries_for_an_hour_with_thirty_second_gaps() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let error = transport("ENOTFOUND", TransportOperation::Request, None);
    assert!(is_unreachable(&error));
    let mut now_ms = 0u64;
    let mut delays = Vec::new();
    while let Some(decision) = policy.decide(&error, true, now_ms) {
        assert!(decision.retry);
        let delay = decision.delay.as_millis() as u64;
        delays.push(delay);
        now_ms += delay;
    }
    assert!(delays.len() > 100);
    assert!(delays.iter().all(|delay| *delay <= 30_000));
    let total: u64 = delays.iter().sum();
    assert!(total >= 3_600_000);
    assert!(total <= 3_630_000);
}

#[test]
fn unreachable_does_not_spend_the_general_allowance() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let unreachable = transport("ENOTFOUND", TransportOperation::Request, None);
    for _ in 0..20 {
        assert!(policy.decide(&unreachable, true, 0).expect("an unreachable decision").retry);
    }
    let general = provider_internal();
    for _ in 0..10 {
        assert!(policy.decide(&general, true, 0).expect("a general decision").retry);
    }
    assert!(policy.decide(&general, true, 0).is_none());
}

#[test]
fn retry_after_is_a_floor_capped_at_fifteen_minutes() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let limited = rate_limit(Some(3_600_000), BTreeMap::new());
    let decision = policy.decide(&limited, true, 0).expect("a decision");
    assert_eq!(decision.delay, Duration::from_millis(900_000));

    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let internal = AiError {
        reason: AiErrorReason::ProviderInternal {
            message: "provider internal".into(),
            body: None,
            http: Some(http_context(500, BTreeMap::new())),
            retry_after_ms: Some(60_000),
        },
    };
    let decision = policy.decide(&internal, true, 0).expect("a decision");
    assert!(decision.delay >= Duration::from_millis(60_000));
}

#[test]
fn jitter_stays_between_eighty_and_one_hundred_twenty_percent() {
    let error = provider_internal();
    let mut policy = Policy::new(Arc::new(FixedRandom(0.0)));
    assert_eq!(policy.decide(&error, true, 0).expect("a decision").delay, Duration::from_millis(1_600));

    let mut policy = Policy::new(Arc::new(FixedRandom(0.99)));
    let delay = policy.decide(&error, true, 0).expect("a decision").delay.as_millis() as u64;
    assert!((2_390..=2_400).contains(&delay));

    let mut policy = Policy::new(Arc::new(SeededRandom::new(7)));
    let delay = policy.decide(&error, true, 0).expect("a decision").delay.as_millis() as u64;
    assert!((1_600..=2_400).contains(&delay));
}

#[tokio::test(start_paused = true)]
async fn retry_returns_a_success() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let calls = Arc::new(AtomicU32::new(0));
    let outcome = retry(&mut policy, || {
        let calls = Arc::clone(&calls);
        async move {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Err(rate_limit(None, BTreeMap::new()));
            }
            Ok(7)
        }
    })
    .await;
    assert_eq!(outcome.expect("a success"), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn retry_gives_up_after_the_general_allowance() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let calls = Arc::new(AtomicU32::new(0));
    let outcome: Result<(), AiError> = retry(&mut policy, || {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(provider_internal())
        }
    })
    .await;
    assert!(outcome.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 11);
}

#[tokio::test(start_paused = true)]
async fn retry_one_shot_stops_after_three_retries() {
    let (outcome, calls) = one_shot_calls(provider_internal_status(503)).await;
    assert!(outcome.is_err());
    assert_eq!(calls, 4);
}

#[tokio::test(start_paused = true)]
async fn retry_one_shot_retries_rate_limits_and_overload() {
    let (limited, calls) = one_shot_calls(rate_limit(None, BTreeMap::new())).await;
    assert!(limited.is_err());
    assert_eq!(calls, 4);

    let (overloaded, calls) = one_shot_calls(provider_internal_status(529)).await;
    assert!(overloaded.is_err());
    assert_eq!(calls, 4);

    let (broken, calls) = one_shot_calls(provider_internal_status(500)).await;
    assert!(broken.is_err());
    assert_eq!(calls, 1);
}

#[tokio::test(start_paused = true)]
async fn retry_one_shot_honours_the_should_retry_header() {
    let (blocked, calls) = one_shot_calls(rate_limit(None, header("x-should-retry", "false"))).await;
    assert!(blocked.is_err());
    assert_eq!(calls, 1);

    let invalid = AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "invalid".into(),
            body: None,
            http: Some(http_context(400, header("x-should-retry", "true"))),
            parameter: None,
            classification: None,
        },
    };
    let (retried, calls) = one_shot_calls(invalid).await;
    assert!(retried.is_err());
    assert_eq!(calls, 4);
}

#[tokio::test(start_paused = true)]
async fn retry_one_shot_does_not_replay_a_cut_off_response() {
    let (outcome, calls) = one_shot_calls(transport("ENETDOWN", TransportOperation::Read, None)).await;
    assert!(outcome.is_err());
    assert_eq!(calls, 1);
}

#[tokio::test(start_paused = true)]
async fn retry_one_shot_replays_unsent_and_unreachable_transports() {
    let retried = [
        transport("ECONNRESET", TransportOperation::Request, Some(Delivery::NotSent)),
        transport("ECONNREFUSED", TransportOperation::Request, None),
        transport("ENOTFOUND", TransportOperation::Request, None),
        transport("EAI_AGAIN", TransportOperation::Request, None),
    ];
    for error in retried {
        let (outcome, calls) = one_shot_calls(error).await;
        assert!(outcome.is_err());
        assert_eq!(calls, 4);
    }

    let (rejected, calls) = one_shot_calls(invalid_request()).await;
    assert!(rejected.is_err());
    assert_eq!(calls, 1);
}

#[test]
fn unreachable_codes_are_recognised() {
    for code in ["ENOTFOUND", "EAI_AGAIN", "ENETUNREACH", "EHOSTUNREACH", "ENETDOWN"] {
        assert!(is_unreachable(&transport(code, TransportOperation::Request, None)));
    }
    assert!(!is_unreachable(&transport("ECONNREFUSED", TransportOperation::Request, None)));
    assert!(!is_unreachable(&transport("Timeout", TransportOperation::Request, None)));
    assert!(!is_unreachable(&transport_without_code(TransportOperation::Request)));
}

#[test]
fn a_disallowed_retry_uses_the_general_allowance_for_an_unreachable_code() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let error = transport("ENOTFOUND", TransportOperation::Request, None);
    for _ in 0..10 {
        let decision = policy.decide(&error, false, 0).expect("a decision");
        assert!(!decision.retry);
    }
    assert!(policy.decide(&error, false, 0).is_none());
}

#[tokio::test(start_paused = true)]
async fn retry_returns_a_non_retryable_error_without_replaying() {
    let mut policy = Policy::new(Arc::new(FixedRandom(0.5)));
    let calls = Arc::new(AtomicU32::new(0));
    let outcome: Result<(), AiError> = retry(&mut policy, || {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(invalid_request())
        }
    })
    .await;
    assert!(outcome.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
