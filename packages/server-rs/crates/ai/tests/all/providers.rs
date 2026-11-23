use opencode_ai::MapEnv;
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::compile::Compiled;
use opencode_ai::route::model::Model;
use opencode_ai::schema::AiErrorReason;
use serde_json::json;

use crate::compile::{compile_model, compile_model_with_channel, headers};

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

fn model(specifier: &str, model_id: &str, settings: serde_json::Value) -> Model {
    let settings = Settings::new(settings.as_object().expect("settings object").clone());
    let runtime = ProviderPackages::builtin()
        .model(specifier, model_id, &settings, &MapEnv::default())
        .unwrap_or_else(|error| panic!("{specifier}: {}", error.message()));
    let RuntimeModel::Language(model) = runtime;
    model
}

fn json_body(compiled: &Compiled) -> serde_json::Value {
    serde_json::from_slice(&compiled.prepared.request.body).expect("prepared body")
}

fn message(specifier: &str, model_id: &str, settings: serde_json::Value) -> String {
    let settings = Settings::new(settings.as_object().expect("settings object").clone());
    match ProviderPackages::builtin().model(specifier, model_id, &settings, &MapEnv::default()) {
        Ok(_) => panic!("{specifier} resolved"),
        Err(error) => error.message().to_string(),
    }
}

#[test]
fn azure_messages_sends_x_api_key_to_foundry() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/azure/messages",
        "claude-haiku-4-5",
        json!({ "resourceName": "fixture", "apiKey": "key" }),
    ));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://fixture.services.ai.azure.com/anthropic/v1/messages");
    let headers = headers(prepared);
    assert_eq!(headers.get("x-api-key").map(String::as_str), Some("key"));
    assert!(!headers.contains_key("api-key"));
    assert!(!headers.contains_key("authorization"));
    assert_eq!(headers.get("anthropic-version").map(String::as_str), Some("2023-06-01"));
}

#[test]
fn azure_chat_keeps_a_foundry_models_url_without_api_version() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/azure/chat",
        "Kimi-K2.5",
        json!({ "baseURL": "https://fixture.services.ai.azure.com/models", "apiKey": "key" }),
    ));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://fixture.services.ai.azure.com/models/chat/completions");
    let headers = headers(prepared);
    assert_eq!(headers.get("api-key").map(String::as_str), Some("key"));
    assert!(!headers.contains_key("authorization"));
}

#[test]
fn azure_deployment_urls_carry_the_configured_api_version() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/azure/chat",
        "gpt-5.6-luna",
        json!({
            "resourceName": "fixture",
            "apiKey": "key",
            "useDeploymentBasedUrls": true,
            "apiVersion": "2025-01-01"
        }),
    ));
    assert_eq!(
        compiled.prepared.request.url,
        "https://fixture.openai.azure.com/openai/deployments/gpt-5.6-luna/chat/completions?api-version=2025-01-01"
    );
}

#[test]
fn azure_requires_a_resource_name_or_base_url() {
    assert_eq!(
        message("@opencode/ai/providers/azure/chat", "gpt-5.6-luna", json!({ "apiKey": "key" })),
        "Azure requires resourceName or baseURL"
    );
}

#[test]
fn azure_responses_channel_is_limited_to_the_openai_v1_responses_url() {
    let compiled = compile_model_with_channel(&model(
        "@opencode/ai/providers/azure/responses",
        "gpt-5.6-luna",
        json!({ "resourceName": "fixture", "apiKey": "key" }),
    ));
    let channel = compiled.channel.expect("channel preparation");
    assert_eq!(channel.url, "wss://fixture.openai.azure.com/openai/v1/responses");

    let foundry = compile_model_with_channel(&model(
        "@opencode/ai/providers/azure/responses",
        "gpt-5.6-luna",
        json!({ "baseURL": "https://fixture.services.ai.azure.com/models", "apiKey": "key" }),
    ));
    assert!(foundry.channel.is_none());
}

#[test]
fn mantle_signs_with_the_mantle_service() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/amazon-bedrock/mantle/responses",
        "openai.gpt-oss-120b",
        json!({
            "credentials": {
                "region": "us-west-1",
                "accessKeyId": "AKIAIOSFODNN7EXAMPLE",
                "secretAccessKey": "secret"
            },
            "region": "us-west-1"
        }),
    ));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://bedrock-mantle.us-west-1.api.aws/v1/responses");
    let authorization = headers(prepared);
    let authorization = authorization.get("authorization").expect("authorization");
    assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"), "{authorization}");
    assert!(authorization.contains("/us-west-1/bedrock-mantle/aws4_request"), "{authorization}");
}

#[test]
fn mantle_bearer_auth_uses_a_custom_base_url() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/amazon-bedrock/mantle/chat",
        "openai.gpt-oss-safeguard-20b",
        json!({ "apiKey": "test-key", "baseURL": "https://mantle.test/v1" }),
    ));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://mantle.test/v1/chat/completions");
    assert_eq!(headers(prepared).get("authorization").map(String::as_str), Some("Bearer test-key"));
}

#[test]
fn mantle_applies_top_p_to_every_route() {
    for (specifier, model_id) in [
        ("@opencode/ai/providers/amazon-bedrock/mantle/responses", "openai.gpt-oss-120b"),
        ("@opencode/ai/providers/amazon-bedrock/mantle/chat", "openai.gpt-oss-120b"),
        ("@opencode/ai/providers/amazon-bedrock/mantle/messages", "anthropic.claude-opus-4-8"),
    ] {
        let compiled = compile_model(&model(specifier, model_id, json!({ "apiKey": "fixture", "topP": 0.8 })));
        assert_eq!(json_body(&compiled).get("top_p"), Some(&json!(0.8)), "{specifier}");
    }
}

#[test]
fn mantle_rejects_conflicting_auth_settings() {
    assert_eq!(
        message("@opencode/ai/providers/amazon-bedrock/mantle/chat", "model", json!({ "auth": "bearer" })),
        "Amazon Bedrock Mantle bearer auth requires apiKey"
    );
    assert_eq!(
        message(
            "@opencode/ai/providers/amazon-bedrock/mantle/chat",
            "model",
            json!({ "auth": "sigv4", "apiKey": "key" })
        ),
        "Amazon Bedrock Mantle SigV4 auth does not accept apiKey"
    );
}
