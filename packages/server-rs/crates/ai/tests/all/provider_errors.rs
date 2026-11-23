use serde_json::Value;

use opencode_ai::clock::FixedClock;
use opencode_ai::provider_error::{HttpFailureInput, http_failure};

use crate::corpus::Corpus;

const FIXED_EPOCH_MS: u64 = 1_767_323_045_000;

#[test]
fn provider_error_rows_match_typescript() {
    let corpus = Corpus::load();
    assert!(!corpus.provider_errors.is_empty(), "provider error corpus is empty");
    let clock = FixedClock::new(FIXED_EPOCH_MS);
    for row in &corpus.provider_errors {
        let input = parse_input(&row.input);
        let label = input.message.clone();
        let error = http_failure(input, &clock);
        let encoded = match serde_json::to_value(&error.reason) {
            Ok(encoded) => encoded,
            Err(failure) => panic!("{label}: failed to encode reason: {failure}"),
        };
        assert_eq!(encoded, row.reason, "{label}: reason mismatch");
        assert_eq!(error.is_retryable(), row.retryable, "{label}: retryable mismatch");
    }
}

fn parse_input(value: &Value) -> HttpFailureInput {
    HttpFailureInput {
        message: value.get("message").and_then(Value::as_str).expect("provider error input has a message").to_string(),
        url: value.get("url").and_then(Value::as_str).map(str::to_string),
        status: value.get("status").and_then(Value::as_u64).map(|status| status as u16),
        data: value.get("data").cloned(),
        response_headers: value
            .get("responseHeaders")
            .map(|headers| serde_json::from_value(headers.clone()).expect("responseHeaders decodes"))
            .unwrap_or_default(),
        response_body: value.get("responseBody").and_then(Value::as_str).map(str::to_string),
    }
}
