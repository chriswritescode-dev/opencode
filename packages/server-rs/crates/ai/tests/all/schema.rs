use std::collections::BTreeSet;

use opencode_ai::schema::{AiError, HttpOptions, HttpTimeout, LlmEvent, LlmRequest, RequestTimeout};
use serde_json::json;

use crate::corpus::Corpus;

#[test]
fn corpus_schema_round_trips() {
    let corpus = Corpus::load();
    assert_eq!(corpus.cassettes.len(), 264, "unexpected cassette count");
    let mut steps = 0usize;
    for cassette in &corpus.cassettes {
        assert!(!cassette.protocol.is_empty(), "{}: empty protocol", cassette.name);
        assert!(!cassette.group.is_empty(), "{}: empty group", cassette.name);
        assert!(!cassette.route_id.is_empty(), "{}: empty routeId", cassette.name);
        assert!(cassette.clock_epoch_ms > 0, "{}: zero clockEpochMs", cassette.name);
        for (index, step) in cassette.steps.iter().enumerate() {
            steps += 1;
            if step.operation.is_none() {
                let label = format!("{} step {index} request", cassette.name);
                round_trip::<LlmRequest>(&step.request, &label);
            }
            if let Some(events) = &step.events {
                for (event_index, event) in events.iter().enumerate() {
                    let label = format!("{} step {index} event {event_index}", cassette.name);
                    round_trip::<LlmEvent>(event, &label);
                }
            }
            if let Some(step_error) = &step.error {
                let label = format!("{} step {index} error", cassette.name);
                let value = json!({ "reason": step_error.reason.clone() });
                let error: AiError = round_trip(&value, &label);
                assert_eq!(error.is_retryable(), step_error.retryable, "{label}: retryable mismatch");
            }
        }
    }
    assert!(steps > 0, "corpus has no steps");
}

#[test]
fn provider_error_rows_round_trip() {
    let corpus = Corpus::load();
    assert!(!corpus.provider_errors.is_empty(), "provider error corpus is empty");
    let mut tags = BTreeSet::new();
    for row in &corpus.provider_errors {
        let label = format!("provider error {}", row.reason);
        let value = json!({ "reason": row.reason.clone() });
        let error: AiError = round_trip(&value, &label);
        assert_eq!(error.is_retryable(), row.retryable, "{label}: retryable mismatch");
        let Some(tag) = row.reason.get("_tag").and_then(serde_json::Value::as_str) else {
            panic!("{label}: missing _tag");
        };
        tags.insert(tag);
    }
    for required in [
        "Authentication",
        "ContentPolicy",
        "InvalidRequest",
        "ProviderInternal",
        "QuotaExceeded",
        "RateLimit",
        "UnknownProvider",
    ] {
        assert!(tags.contains(required), "missing provider error tag {required}");
    }
}

#[test]
fn unknown_event_type_is_rejected() {
    let value = json!({ "type": "not-a-real-event", "id": "x" });
    assert!(serde_json::from_value::<LlmEvent>(value).is_err());
}

#[test]
fn unknown_provider_option_keys_are_kept() {
    let value = json!({
        "system": [],
        "messages": [],
        "tools": [],
        "providerOptions": { "custom": { "futureKnob": 7 } }
    });
    let request: LlmRequest = match serde_json::from_value(value.clone()) {
        Ok(request) => request,
        Err(error) => panic!("failed to decode request: {error}"),
    };
    let encoded = match serde_json::to_value(&request) {
        Ok(encoded) => encoded,
        Err(error) => panic!("failed to encode request: {error}"),
    };
    assert_eq!(encoded, value, "unknown provider option key was dropped");
}

#[test]
fn json_helpers() {
    assert_eq!(opencode_ai::json::canonical(&json!({"b": 1, "a": {"d": 2, "c": 3}})), r#"{"a":{"c":3,"d":2},"b":1}"#);
    assert_eq!(opencode_ai::json::canonical(&json!({"10": 1, "2": 2, "a": 3})), r#"{"2":2,"10":1,"a":3}"#);
    assert_eq!(opencode_ai::json::canonical(&json!({"z": [3, {"b": 1, "a": 2}]})), r#"{"z":[3,{"a":2,"b":1}]}"#);
    assert_eq!(
        opencode_ai::json::deep_merge(&json!({"a": {"b": 1, "c": 2}, "x": 1}), &json!({"a": {"c": 3}, "y": 2})),
        Some(json!({"a": {"b": 1, "c": 3}, "x": 1, "y": 2}))
    );
    assert_eq!(opencode_ai::json::deep_merge(&json!({"a": 1}), &json!({"a": {"b": 2}})), Some(json!({"a": {"b": 2}})));
    assert_eq!(opencode_ai::json::encode_body(&json!({"n": 1.5, "i": 2})), r#"{"n":1.5,"i":2}"#);
}

#[test]
fn request_timeout_and_http_body_reject_out_of_contract_values() {
    let decode = |value: serde_json::Value| serde_json::from_value::<RequestTimeout>(value);
    assert!(matches!(decode(json!(250)), Ok(RequestTimeout::Timeout(HttpTimeout::Millis(_)))));
    assert!(matches!(decode(json!(false)), Ok(RequestTimeout::Timeout(HttpTimeout::Disabled(false)))));
    assert!(decode(json!(true)).is_err());
    assert!(decode(json!(0)).is_err());
    assert!(decode(json!(-5)).is_err());

    assert_eq!(serde_json::to_value(HttpTimeout::Millis(serde_json::Number::from(250))).unwrap(), json!(250));
    assert_eq!(serde_json::to_value(HttpTimeout::Disabled(false)).unwrap(), json!(false));

    assert!(serde_json::from_value::<HttpOptions>(json!({ "body": [] })).is_err());
    assert!(serde_json::from_value::<HttpOptions>(json!({ "body": "text" })).is_err());
    assert!(serde_json::from_value::<HttpOptions>(json!({ "body": { "a": 1 } })).is_ok());
}

fn round_trip<T>(value: &serde_json::Value, label: &str) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let decoded = match serde_json::from_value(value.clone()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("{label}: failed to decode: {error}"),
    };
    let encoded = match serde_json::to_value(&decoded) {
        Ok(encoded) => encoded,
        Err(error) => panic!("{label}: failed to encode: {error}"),
    };
    assert_eq!(encoded, *value, "{label}: round trip changed the value");
    decoded
}
