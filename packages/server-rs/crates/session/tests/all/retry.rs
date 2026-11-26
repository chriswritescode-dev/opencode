use std::collections::BTreeMap;
use std::time::Duration;

use opencode_ai::{AiError, AiErrorReason, HttpContext, TransportKind, TransportOperation};
use opencode_catalog::resolver::Error;
use opencode_contract::generated::{ProviderID, SessionStructuredErrorResponse};
use opencode_session::context::ContextError;
use opencode_session::execution::ExecutionError;
use opencode_session::retry::{RetryPolicy, structured};

fn rate_limit(retry_after_ms: Option<u64>) -> AiError {
    AiError {
        reason: AiErrorReason::RateLimit {
            message: "slow down".to_owned(),
            body: None,
            http: None,
            retry_after_ms,
            rate_limit: None,
        },
    }
}

#[test]
fn provider_requested_delay_raises_the_first_gap() {
    let mut policy = RetryPolicy::new();
    let decision = policy.decide(&rate_limit(Some(30_000)), true).expect("a retry decision");

    assert_eq!(decision.attempt, 2);
    assert!(decision.delay >= Duration::from_secs(30));
    assert!(decision.delay <= Duration::from_secs(15 * 60));
}

#[test]
fn the_general_schedule_allows_ten_retries() {
    let mut policy = RetryPolicy::new();

    for _ in 0..10 {
        assert!(policy.decide(&rate_limit(None), true).is_some());
    }

    assert!(policy.decide(&rate_limit(None), true).is_none());
}

#[test]
fn a_non_retryable_error_is_not_retried() {
    let mut policy = RetryPolicy::new();
    let error =
        AiError { reason: AiErrorReason::Authentication { message: "bad key".to_owned(), body: None, http: None } };

    assert!(policy.decide(&error, false).is_none());
}

#[test]
fn structured_error_carries_the_provider_details() {
    let body = "{\"error\":\"rate\"}".to_owned();
    let error = ExecutionError::Ai(AiError {
        reason: AiErrorReason::RateLimit {
            message: "slow down".to_owned(),
            body: Some(body.clone()),
            http: Some(HttpContext {
                url: "https://example.test/v1".to_owned(),
                status: 429,
                headers: BTreeMap::new(),
            }),
            retry_after_ms: Some(1_000),
            rate_limit: None,
        },
    });

    let structured = structured(&error);

    assert_eq!(structured.r#type, "provider.rate-limit");
    assert_eq!(structured.message, "slow down");
    assert_eq!(structured.status, Some(429.0));
    assert_eq!(structured.response, Some(SessionStructuredErrorResponse { body }));
    assert_eq!(structured.retry_after_ms, Some(1_000.0));
}

#[test]
fn unreachable_hosts_use_a_separate_retry_budget() {
    let mut policy = RetryPolicy::new();
    let error = AiError {
        reason: AiErrorReason::Transport {
            message: "no route to host".to_owned(),
            body: None,
            http: None,
            transport: TransportKind::Http,
            operation: TransportOperation::Request,
            code: Some("HostUnreachable".to_owned()),
            url: None,
            phase: None,
            delivery: None,
            recovery: None,
        },
    };

    for _ in 0..20 {
        let decision = policy.decide(&error, true).expect("the unreachable budget outlasts the general one");
        assert!(decision.delay <= Duration::from_secs(30));
    }
}

#[test]
fn structured_error_maps_the_output_limit() {
    let structured = structured(&ExecutionError::OutputLimit);

    assert_eq!(structured.r#type, "output-limit");
    assert_eq!(structured.message, "Response still truncated after two output token limit continuations");
    assert_eq!(structured.status, None);
    assert_eq!(structured.response, None);
    assert_eq!(structured.retry_after_ms, None);
}

#[test]
fn structured_error_maps_model_resolution_failures() {
    let provider_id = ProviderID("test-provider".to_owned());
    let cases = [
        (
            Error::UnresolvedProviderVariables {
                provider_id: provider_id.clone(),
                model_id: "test-model".to_owned(),
                variables: vec!["MISSING_HOST".to_owned()],
            },
            "provider.no-route",
            "Cannot initialize test-provider/test-model: MISSING_HOST is required to resolve the provider endpoint",
        ),
        (
            Error::UnsupportedCompaction {
                provider_id: provider_id.clone(),
                model_id: "test-model".to_owned(),
                route: "anthropic-messages".to_owned(),
            },
            "provider.unsupported-operation",
            "Provider compaction is not supported by test-provider/test-model (anthropic-messages)",
        ),
        (
            Error::VariantUnavailable { provider_id, model_id: "test-model".to_owned(), variant: "max".to_owned() },
            "provider.no-route",
            "Variant unavailable for test-provider/test-model: max",
        ),
    ];
    for (error, kind, message) in cases {
        let structured = structured(&ExecutionError::Context(ContextError::ModelResolution(error)));
        assert_eq!(structured.r#type, kind);
        assert_eq!(structured.message, message);
    }
}
