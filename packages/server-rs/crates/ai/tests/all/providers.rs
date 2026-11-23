use opencode_ai::MapEnv;
use opencode_ai::providers::{ProviderPackages, Settings};
use opencode_ai::schema::AiErrorReason;
use serde_json::json;

fn from_value(value: serde_json::Value) -> Settings {
    Settings::new(value.as_object().expect("settings object").clone())
}

#[test]
fn request_options_split_connection_keys_from_provider_options() {
    let settings = from_value(json!({
        "apiKey": "fixture",
        "baseURL": "https://example.test",
        "headers": { "x-test": "one" },
        "body": { "reasoning": { "enabled": true } },
        "queryParams": { "api-version": "v1" },
        "effort": "high",
        "reasoningEffort": "low"
    }));
    let options = settings.request_options(&["apiKey", "baseURL"]).expect("request options");
    assert_eq!(options.headers.as_ref().and_then(|headers| headers.get("x-test")), Some("one"));
    let http = options.http.as_ref().expect("http options");
    assert_eq!(http.query.as_ref().and_then(|query| query.get("api-version")), Some("v1"));
    assert_eq!(http.body.as_ref().and_then(|body| body.get("reasoning")), Some(&json!({ "enabled": true })));
    let provider = options.provider_options.as_ref().expect("provider options");
    assert_eq!(provider.get("effort"), Some(&json!("high")));
    assert_eq!(provider.get("reasoningEffort"), Some(&json!("low")));
    assert!(provider.get("apiKey").is_none());
    assert!(provider.get("baseURL").is_none());
    assert!(provider.get("headers").is_none());
    assert!(provider.get("body").is_none());
    assert!(provider.get("queryParams").is_none());
}

#[test]
fn request_options_are_absent_without_extra_keys() {
    let settings = from_value(json!({ "apiKey": "fixture", "baseURL": "https://example.test" }));
    assert!(settings.request_options(&["apiKey", "baseURL"]).is_none());
}

#[test]
fn api_key_reads_strings_and_false() {
    let settings = from_value(json!({ "apiKey": "fixture" }));
    assert!(matches!(settings.api_key(), Some(opencode_ai::route::auth::ApiKey::Value(value)) if value == "fixture"));
    let disabled = from_value(json!({ "apiKey": false }));
    assert!(matches!(disabled.api_key(), Some(opencode_ai::route::auth::ApiKey::Disabled)));
    assert!(Settings::default().api_key().is_none());
}

#[test]
fn unknown_specifier_reports_provider_package() {
    let result = ProviderPackages::builtin().model(
        "@opencode/ai/providers/does-not-exist",
        "model",
        &Settings::default(),
        &MapEnv::default(),
    );
    let error = match result {
        Ok(_) => panic!("unknown specifier resolved"),
        Err(error) => error,
    };
    match error.reason {
        AiErrorReason::UnsupportedOperation { operation, message, .. } => {
            assert_eq!(operation, "provider-package");
            assert!(message.contains("does-not-exist"), "{message}");
        }
        other => panic!("expected unsupported operation, got {other:?}"),
    }
}
