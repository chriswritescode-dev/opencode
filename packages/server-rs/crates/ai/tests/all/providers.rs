use std::sync::{Arc, Mutex};

use opencode_ai::MapEnv;
use opencode_ai::modalities::embedding::{EmbeddingClient, EmbeddingRequestInput, EmbeddingValue};
use opencode_ai::providers::google_vertex_shared::{self, ApiKeyInput};
use opencode_ai::providers::meta::{UserLocation, WebSearchOptions};
use opencode_ai::providers::openai::ImageGenerationOptions;
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::auth::CredentialSource;
use opencode_ai::route::compile::{Compiled, StreamOptions, compile};
use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::model::Model;
use opencode_ai::schema::{AiError, AiErrorReason, ToolDefinition};
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
    assert!(http.query.is_none());
    assert_eq!(http.body.as_ref().and_then(|body| body.get("reasoning")), Some(&json!({ "enabled": true })));
    let provider = options.provider_options.as_ref().expect("provider options");
    assert_eq!(provider.get("effort"), Some(&json!("high")));
    assert_eq!(provider.get("reasoningEffort"), Some(&json!("low")));
    assert_eq!(provider.get("queryParams"), Some(&json!({ "api-version": "v1" })));
    assert!(provider.get("apiKey").is_none());
    assert!(provider.get("baseURL").is_none());
    assert!(provider.get("headers").is_none());
    assert!(provider.get("body").is_none());
}

#[test]
fn declared_query_params_leave_provider_options() {
    let settings = from_value(json!({
        "apiKey": "fixture",
        "baseURL": "https://example.test",
        "queryParams": { "foo": "bar" }
    }));
    assert!(settings.request_options(&["apiKey", "baseURL", "queryParams"]).is_none());
    assert_eq!(settings.query_params().and_then(|query| query.get("foo").map(str::to_string)), Some("bar".to_string()));
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
    let RuntimeModel::Language(model) = runtime else {
        panic!("{specifier}: expected a language model");
    };
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

fn zai_request(max_tokens: Option<u64>, with_tools: bool) -> serde_json::Value {
    let tools = if with_tools {
        json!([{
            "type": "tool",
            "name": "lookup",
            "description": "Look up a value",
            "inputSchema": { "type": "object", "properties": { "key": { "type": "string" } } }
        }])
    } else {
        json!([])
    };
    let mut request = json!({
        "system": [],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hi" }] }],
        "tools": tools
    });
    if let Some(max_tokens) = max_tokens {
        request["maxTokens"] = json!(max_tokens);
    }
    request
}

fn compile_zai(model: &Model, request: serde_json::Value) -> Compiled {
    let request = serde_json::from_value(request).expect("request");
    compile(model, request, &StreamOptions::default(), &crate::compile::context()).expect("compile")
}

#[test]
fn zai_chat_applies_native_body_fields() {
    let model = model(
        "@opencode/ai/providers/zai",
        "glm-5.3",
        json!({
            "apiKey": "fixture",
            "reasoningEffort": "high",
            "thinking": { "type": "enabled", "clear_thinking": false }
        }),
    );
    let compiled = compile_zai(&model, zai_request(Some(123), true));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://api.z.ai/api/paas/v4/chat/completions");
    assert_eq!(headers(prepared).get("authorization").map(String::as_str), Some("Bearer fixture"));
    let body = json_body(&compiled);
    assert_eq!(body.get("max_tokens"), Some(&json!(123)));
    assert_eq!(body.get("reasoning_effort"), Some(&json!("high")));
    assert_eq!(body.get("thinking"), Some(&json!({ "type": "enabled", "clear_thinking": false })));
    assert_eq!(body.get("tool_stream"), Some(&json!(true)));
}

#[test]
fn zai_chat_scopes_tool_streaming_to_glm_4_6_and_later() {
    for (model_id, supported) in [
        ("glm-5.3", true),
        ("glm-4.5", false),
        ("glm-4.6", true),
        ("GLM-4.6", true),
        ("glm-10.1-new-family", true),
        ("zai/glm-6.1", true),
        ("glm-4.60foo", false),
        ("glm-6.x", false),
        ("future-model", false),
    ] {
        let model = model("@opencode/ai/providers/zai", model_id, json!({ "apiKey": "fixture" }));
        let body = json_body(&compile_zai(&model, zai_request(None, true)));
        if supported {
            assert_eq!(body.get("tool_stream"), Some(&json!(true)), "{model_id}");
        } else {
            assert!(body.get("tool_stream").is_none(), "{model_id}");
        }
    }
    let model = model("@opencode/ai/providers/zai", "glm-5.3", json!({ "apiKey": "fixture" }));
    assert!(json_body(&compile_zai(&model, zai_request(None, false))).get("tool_stream").is_none());
}

#[test]
fn zai_coding_plan_routes_use_zai_endpoints() {
    for (specifier, url) in [
        ("@opencode/ai/providers/zai-coding-plan", "https://api.z.ai/api/coding/paas/v4/chat/completions"),
        ("@opencode/ai/providers/zai-coding-plan/chat", "https://api.z.ai/api/coding/paas/v4/chat/completions"),
        ("@opencode/ai/providers/zai-coding-plan/messages", "https://api.z.ai/api/anthropic/v1/messages"),
        ("@opencode/ai/providers/zai-coding-plan/responses", "https://api.z.ai/api/v1/responses"),
    ] {
        let compiled = compile_model(&model(specifier, "glm-5.3", json!({ "apiKey": "fixture" })));
        assert_eq!(compiled.prepared.request.url, url, "{specifier}");
        assert_eq!(
            headers(&compiled.prepared.request).get("authorization").map(String::as_str),
            Some("Bearer fixture"),
            "{specifier}"
        );
        assert_eq!(compiled.resolved.model.route.provider_metadata_key.as_deref(), Some("zai"), "{specifier}");
    }
}

#[test]
fn zai_coding_messages_keeps_native_thinking_and_effort() {
    let compiled = compile_model(&model(
        "@opencode/ai/providers/zai-coding-plan/messages",
        "glm-5.3",
        json!({ "apiKey": "fixture", "thinking": { "type": "enabled" }, "effort": "high" }),
    ));
    let prepared = &compiled.prepared.request;
    assert_eq!(prepared.url, "https://api.z.ai/api/anthropic/v1/messages");
    let headers = headers(prepared);
    assert_eq!(headers.get("anthropic-version").map(String::as_str), Some("2023-06-01"));
    assert!(!headers.contains_key("anthropic-beta"));
    let body = json_body(&compiled);
    assert_eq!(body.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(body.get("output_config"), Some(&json!({ "effort": "high" })));
}

#[test]
fn zai_rejects_invalid_thinking_options() {
    let model = model(
        "@opencode/ai/providers/zai",
        "glm-5.3",
        json!({ "apiKey": "fixture", "thinking": { "clear_thinking": "false" } }),
    );
    let request = serde_json::from_value(zai_request(None, false)).expect("request");
    let error = compile(&model, request, &StreamOptions::default(), &crate::compile::context())
        .err()
        .expect("invalid thinking option");
    assert!(error.message().contains("clear_thinking"), "{}", error.message());
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }), "{error:?}");
}

#[test]
fn moonshot_routes_use_native_endpoints_and_provider_identity() {
    for (specifier, url) in [
        ("@opencode/ai/providers/moonshot", "https://api.moonshot.ai/v1/chat/completions"),
        ("@opencode/ai/providers/moonshot/chat", "https://api.moonshot.ai/v1/chat/completions"),
        ("@opencode/ai/providers/moonshot/messages", "https://api.moonshot.ai/anthropic/v1/messages"),
        ("@opencode/ai/providers/moonshot/responses", "https://api.moonshot.ai/v1/responses"),
    ] {
        let compiled = compile_model(&model(specifier, "kimi-k3", json!({ "apiKey": "fixture" })));
        assert_eq!(compiled.prepared.request.url, url, "{specifier}");
        assert_eq!(compiled.resolved.model.provider, "moonshotai", "{specifier}");
        assert_eq!(compiled.resolved.model.route.provider_metadata_key.as_deref(), Some("moonshot"), "{specifier}");
        assert_eq!(
            headers(&compiled.prepared.request).get("authorization").map(String::as_str),
            Some("Bearer fixture"),
            "{specifier}"
        );
    }
}

#[test]
fn moonshot_reads_api_key_environment_fallbacks_in_order() {
    let settings = Settings::default();
    let runtime = ProviderPackages::builtin()
        .model(
            "@opencode/ai/providers/moonshot/chat",
            "kimi-k3",
            &settings,
            &MapEnv::new(vec![
                ("MOONSHOT_API_KEY".to_string(), "primary".to_string()),
                ("MOONSHOTAI_API_KEY".to_string(), "fallback".to_string()),
            ]),
        )
        .expect("moonshot model");
    let RuntimeModel::Language(model) = runtime else {
        panic!("moonshot: expected a language model");
    };
    let request = serde_json::from_value(zai_request(Some(64), false)).expect("request");
    let context = opencode_ai::ExecutionContext::new(
        std::sync::Arc::new(opencode_ai::clock::FixedClock::new(1_767_323_045_000)),
        std::sync::Arc::new(opencode_ai::id::SequenceIds::new()),
        std::sync::Arc::new(MapEnv::new(vec![
            ("MOONSHOT_API_KEY".to_string(), "primary".to_string()),
            ("MOONSHOTAI_API_KEY".to_string(), "fallback".to_string()),
        ])),
    );
    let compiled = compile(&model, request, &StreamOptions::default(), &context).expect("compile");
    assert_eq!(headers(&compiled.prepared.request).get("authorization").map(String::as_str), Some("Bearer primary"));

    let fallback_context = opencode_ai::ExecutionContext::new(
        std::sync::Arc::new(opencode_ai::clock::FixedClock::new(1_767_323_045_000)),
        std::sync::Arc::new(opencode_ai::id::SequenceIds::new()),
        std::sync::Arc::new(MapEnv::new(vec![("MOONSHOTAI_API_KEY".to_string(), "fallback".to_string())])),
    );
    let request = serde_json::from_value(zai_request(Some(64), false)).expect("request");
    let compiled = compile(&model, request, &StreamOptions::default(), &fallback_context).expect("compile");
    assert_eq!(headers(&compiled.prepared.request).get("authorization").map(String::as_str), Some("Bearer fallback"));
}

#[test]
fn moonshot_messages_omits_anthropic_version_and_beta() {
    let compiled =
        compile_model(&model("@opencode/ai/providers/moonshot/messages", "kimi-k3", json!({ "apiKey": "fixture" })));
    assert_eq!(compiled.prepared.request.url, "https://api.moonshot.ai/anthropic/v1/messages");
    let headers = headers(&compiled.prepared.request);
    assert!(!headers.contains_key("anthropic-version"));
    assert!(!headers.contains_key("anthropic-beta"));
}

#[test]
fn moonshot_chat_uses_max_tokens_and_passes_thinking_through() {
    for (thinking, expected) in [
        (json!({ "type": "disabled" }), json!({ "type": "disabled" })),
        (json!({ "type": "enabled", "keep": "all" }), json!({ "type": "enabled", "keep": "all" })),
        (json!({ "type": "enabled", "keep": null }), json!({ "type": "enabled", "keep": null })),
    ] {
        let model = model(
            "@opencode/ai/providers/moonshot/chat",
            "kimi-k2.6",
            json!({ "apiKey": "fixture", "thinking": thinking, "reasoningEffort": "high" }),
        );
        let compiled = compile_zai(&model, zai_request(Some(123), false));
        let body = json_body(&compiled);
        assert_eq!(body.get("max_tokens"), Some(&json!(123)), "{thinking}");
        assert!(body.get("max_completion_tokens").is_none(), "{thinking}");
        assert_eq!(body.get("reasoning_effort"), Some(&json!("high")), "{thinking}");
        assert_eq!(body.get("thinking"), Some(&expected), "{thinking}");
    }
    let model = model("@opencode/ai/providers/moonshot/chat", "kimi-k3", json!({ "apiKey": "fixture" }));
    let body = json_body(&compile_zai(&model, zai_request(Some(123), false)));
    assert!(body.get("thinking").is_none());
}

#[test]
fn moonshot_rejects_invalid_chat_thinking_options() {
    let model = model(
        "@opencode/ai/providers/moonshot/chat",
        "kimi-k2.6",
        json!({ "apiKey": "fixture", "thinking": { "type": "enabled", "keep": 42 } }),
    );
    let request = serde_json::from_value(zai_request(None, false)).expect("request");
    let error = compile(&model, request, &StreamOptions::default(), &crate::compile::context())
        .err()
        .expect("invalid thinking option");
    assert!(error.message().contains("keep"), "{}", error.message());
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }), "{error:?}");
}

#[test]
fn minimax_routes_use_native_endpoints_and_provider_identity() {
    for (specifier, url) in [
        ("@opencode/ai/providers/minimax", "https://api.minimax.io/anthropic/v1/messages"),
        ("@opencode/ai/providers/minimax/messages", "https://api.minimax.io/anthropic/v1/messages"),
        ("@opencode/ai/providers/minimax/chat", "https://api.minimax.io/v1/chat/completions"),
        ("@opencode/ai/providers/minimax/responses", "https://api.minimax.io/v1/responses"),
    ] {
        let compiled = compile_model(&model(specifier, "MiniMax-M3", json!({ "apiKey": "fixture" })));
        assert_eq!(compiled.prepared.request.url, url, "{specifier}");
        assert_eq!(compiled.resolved.model.provider, "minimax", "{specifier}");
        assert_eq!(compiled.resolved.model.route.provider_metadata_key.as_deref(), Some("minimax"), "{specifier}");
        assert_eq!(
            headers(&compiled.prepared.request).get("authorization").map(String::as_str),
            Some("Bearer fixture"),
            "{specifier}"
        );
    }
}

#[test]
fn minimax_messages_sends_anthropic_version_without_beta() {
    let compiled =
        compile_model(&model("@opencode/ai/providers/minimax/messages", "MiniMax-M3", json!({ "apiKey": "fixture" })));
    let headers = headers(&compiled.prepared.request);
    assert_eq!(headers.get("anthropic-version").map(String::as_str), Some("2023-06-01"));
    assert!(!headers.contains_key("anthropic-beta"));
}

#[test]
fn minimax_chat_uses_max_completion_tokens_and_reasoning_split() {
    let chat = model(
        "@opencode/ai/providers/minimax/chat",
        "MiniMax-M3",
        json!({ "apiKey": "fixture", "thinking": { "type": "disabled" } }),
    );
    let body = json_body(&compile_zai(&chat, zai_request(Some(128), false)));
    assert_eq!(body.get("max_completion_tokens"), Some(&json!(128)));
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert_eq!(body.get("reasoning_split"), Some(&json!(true)));

    let split_off = model(
        "@opencode/ai/providers/minimax/chat",
        "MiniMax-M3",
        json!({ "apiKey": "fixture", "reasoningSplit": false }),
    );
    let body = json_body(&compile_zai(&split_off, zai_request(None, false)));
    assert_eq!(body.get("reasoning_split"), Some(&json!(false)));
}

fn vertex_env(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())))
}

#[test]
fn google_vertex_environment_fallbacks_follow_precedence() {
    let all = vertex_env(&[
        ("GOOGLE_VERTEX_PROJECT", "vertex-project"),
        ("GOOGLE_CLOUD_PROJECT", "cloud-project"),
        ("GCP_PROJECT", "gcp-project"),
        ("GCLOUD_PROJECT", "gcloud-project"),
        ("GOOGLE_VERTEX_LOCATION", "europe-west4"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ("VERTEX_LOCATION", "us-east1"),
    ]);
    assert_eq!(google_vertex_shared::project(Some("explicit"), &all).as_deref(), Some("explicit"));
    assert_eq!(google_vertex_shared::project(None, &all).as_deref(), Some("vertex-project"));
    assert_eq!(google_vertex_shared::location(Some("asia-east1"), &all), "asia-east1");
    assert_eq!(google_vertex_shared::location(None, &all), "europe-west4");

    let cloud = vertex_env(&[
        ("GOOGLE_CLOUD_PROJECT", "cloud-project"),
        ("GCP_PROJECT", "gcp-project"),
        ("GCLOUD_PROJECT", "gcloud-project"),
        ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ("VERTEX_LOCATION", "us-east1"),
    ]);
    assert_eq!(google_vertex_shared::project(None, &cloud).as_deref(), Some("cloud-project"));
    assert_eq!(google_vertex_shared::location(None, &cloud), "us-central1");

    let gcp = vertex_env(&[("GCP_PROJECT", "gcp-project"), ("GCLOUD_PROJECT", "gcloud-project")]);
    assert_eq!(google_vertex_shared::project(None, &gcp).as_deref(), Some("gcp-project"));

    let gcloud = vertex_env(&[("GCLOUD_PROJECT", "gcloud-project"), ("VERTEX_LOCATION", "us-east1")]);
    assert_eq!(google_vertex_shared::project(None, &gcloud).as_deref(), Some("gcloud-project"));
    assert_eq!(google_vertex_shared::location(None, &gcloud), "us-east1");

    let empty = MapEnv::default();
    assert_eq!(google_vertex_shared::project(None, &empty), None);
    assert_eq!(google_vertex_shared::location(None, &empty), "global");
}

#[test]
fn google_vertex_hosts_use_jurisdictional_endpoints() {
    assert_eq!(google_vertex_shared::host("global"), "aiplatform.googleapis.com");
    assert_eq!(google_vertex_shared::host("eu"), "aiplatform.eu.rep.googleapis.com");
    assert_eq!(google_vertex_shared::host("us"), "aiplatform.us.rep.googleapis.com");
    assert_eq!(google_vertex_shared::host("europe-west4"), "europe-west4-aiplatform.googleapis.com");
}

#[test]
fn google_vertex_api_key_rules() {
    let empty = MapEnv::default();
    let explicit = ApiKeyInput { api_key: Some("explicit".to_string()), ..ApiKeyInput::default() };
    assert_eq!(google_vertex_shared::api_key(&explicit, &empty).expect("explicit key").as_deref(), Some("explicit"));

    let ambient = vertex_env(&[("GOOGLE_VERTEX_API_KEY", "ambient")]);
    assert_eq!(
        google_vertex_shared::api_key(&ApiKeyInput::default(), &ambient).expect("ambient key").as_deref(),
        Some("ambient")
    );

    let project = ApiKeyInput { project: Some("project".to_string()), ..ApiKeyInput::default() };
    assert_eq!(google_vertex_shared::api_key(&project, &ambient).expect("project suppresses key"), None);

    let location = ApiKeyInput { location: Some("global".to_string()), ..ApiKeyInput::default() };
    assert_eq!(google_vertex_shared::api_key(&location, &ambient).expect("location suppresses key"), None);

    let access_token = ApiKeyInput { access_token: Some("token".to_string()), ..ApiKeyInput::default() };
    assert_eq!(google_vertex_shared::api_key(&access_token, &ambient).expect("access token suppresses key"), None);

    let conflict = ApiKeyInput {
        api_key: Some("explicit".to_string()),
        access_token: Some("token".to_string()),
        ..ApiKeyInput::default()
    };
    let error = google_vertex_shared::api_key(&conflict, &empty).expect_err("conflicting credentials");
    assert_eq!(error.message(), "Google Vertex apiKey cannot be combined with accessToken or auth");

    let auth_conflict = ApiKeyInput {
        api_key: Some("explicit".to_string()),
        auth: Some(opencode_ai::route::auth::Auth::None),
        ..ApiKeyInput::default()
    };
    let error = google_vertex_shared::api_key(&auth_conflict, &empty).expect_err("conflicting auth");
    assert_eq!(error.message(), "Google Vertex apiKey cannot be combined with accessToken or auth");
}

#[test]
fn google_vertex_gemini_entry_matches_the_root_provider() {
    let settings = from_value(json!({ "apiKey": "fixture" }));
    let env = MapEnv::default();
    let packages = ProviderPackages::builtin();
    let RuntimeModel::Language(root) = packages
        .model("@opencode/ai/providers/google-vertex", "gemini-2.5-flash", &settings, &env)
        .expect("root model")
    else {
        panic!("google-vertex: expected a language model");
    };
    let RuntimeModel::Language(gemini) = packages
        .model("@opencode/ai/providers/google-vertex/gemini", "gemini-2.5-flash", &settings, &env)
        .expect("gemini model")
    else {
        panic!("google-vertex/gemini: expected a language model");
    };
    assert_eq!(gemini.route.id, "google-vertex-gemini");
    assert_eq!(
        serde_json::to_value(gemini.descriptor()).expect("gemini descriptor"),
        serde_json::to_value(root.descriptor()).expect("root descriptor")
    );
}

#[test]
fn google_adc_reports_a_missing_credential() {
    let error =
        opencode_ai::providers::google_adc::GoogleAdc::new(None).load(&MapEnv::default()).expect_err("missing ADC");
    match error.reason {
        AiErrorReason::Authentication { message, .. } => {
            assert_eq!(message, "Missing auth credential: Google Application Default Credentials");
        }
        other => panic!("expected authentication error, got {other:?}"),
    }
}

#[test]
fn openai_query_params_reach_both_language_endpoints() {
    for specifier in ["@opencode/ai/providers/openai", "@opencode/ai/providers/openai/chat"] {
        let model = model(
            specifier,
            "gpt-5-mini",
            json!({ "apiKey": "fixture", "queryParams": { "api-version": "2025-01-01" } }),
        );
        assert!(
            model
                .options
                .as_ref()
                .and_then(|options| options.provider_options.as_ref())
                .is_none_or(|options| !options.contains_key("queryParams")),
            "{specifier}: queryParams stayed in provider options"
        );
        let compiled = compile_model(&model);
        assert!(
            compiled.prepared.request.url.ends_with("?api-version=2025-01-01"),
            "{specifier}: {}",
            compiled.prepared.request.url
        );
    }
}

#[test]
fn openai_media_headers_keep_caller_values_over_organization_and_project() {
    let settings = from_value(json!({
        "apiKey": "fixture",
        "organization": "org",
        "project": "project",
        "headers": {
            "OpenAI-Organization": "caller-org",
            "OpenAI-Project": "caller-project",
            "x-extra": "one"
        }
    }));
    let RuntimeModel::Transcription(model) =
        opencode_ai::providers::openai::transcription_model("whisper-1", &settings, &MapEnv::default())
            .expect("transcription model")
    else {
        panic!("expected a transcription model");
    };
    let headers = model.transport.and_then(|transport| transport.headers).expect("transport headers");
    assert_eq!(headers.get("OpenAI-Organization"), Some("caller-org"));
    assert_eq!(headers.get("OpenAI-Project"), Some("caller-project"));
    assert_eq!(headers.get("x-extra"), Some("one"));
}

#[test]
fn google_vertex_tuned_embeddings_reject_express_mode_api_keys() {
    for (settings, env) in [
        (from_value(json!({ "apiKey": "key" })), MapEnv::default()),
        (Settings::default(), MapEnv::new(vec![("GOOGLE_VERTEX_API_KEY".to_string(), "key".to_string())])),
    ] {
        let error = match ProviderPackages::builtin().model(
            "@opencode/ai/providers/google-vertex/embeddings",
            "endpoints/123",
            &settings,
            &env,
        ) {
            Ok(_) => panic!("tuned embedding model accepted an Express Mode key"),
            Err(error) => error,
        };
        assert_eq!(error.message(), "Google Vertex tuned models do not support Express Mode API keys");
    }
}

struct CaptureSender {
    request: Mutex<Option<PreparedHttp>>,
}

impl HttpSender for CaptureSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        *self.request.lock().expect("capture lock") = Some(request);
        Box::pin(async {
            Err(AiError {
                reason: AiErrorReason::ProviderInternal {
                    message: "capture".to_string(),
                    body: None,
                    http: None,
                    retry_after_ms: None,
                },
            })
        })
    }
}

#[tokio::test]
async fn google_vertex_tuned_embeddings_leave_the_publishers_google_path() {
    let settings = from_value(json!({ "project": "project", "location": "us-central1", "accessToken": "token" }));
    let runtime = ProviderPackages::builtin()
        .model("@opencode/ai/providers/google-vertex/embeddings", "endpoints/123", &settings, &MapEnv::default())
        .expect("tuned embedding model");
    let RuntimeModel::Embedding(model) = runtime else { panic!("expected an embedding model") };
    let sender = Arc::new(CaptureSender { request: Mutex::new(None) });
    let executor = RequestExecutor::new(sender.clone(), None, crate::compile::context().clock);
    let input = EmbeddingRequestInput {
        values: vec![EmbeddingValue::Text("fixture".to_string())],
        dimensions: None,
        input_type: None,
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    };
    let _ = EmbeddingClient::generate(input.with_model(model), &executor, &crate::compile::context()).await;
    let url = sender
        .request
        .lock()
        .expect("capture lock")
        .as_ref()
        .map(|request| request.url.clone())
        .expect("prepared request");
    assert_eq!(
        url,
        "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/project/locations/us-central1/endpoints/123:predict"
    );
}

#[test]
fn root_decision_specifiers_match_their_decisions_subpath() {
    let settings = from_value(json!({
        "apiKey": "fixture",
        "accountId": "account",
        "baseURL": "https://example.test/ai/v1"
    }));
    let env = MapEnv::default();
    for (root, subpath) in [
        ("@opencode/ai/providers/cloudflare-ai-gateway", "@opencode/ai/providers/cloudflare-ai-gateway/decisions"),
        ("@opencode/ai/providers/cloudflare-workers-ai", "@opencode/ai/providers/cloudflare-workers-ai/decisions"),
        ("@opencode/ai/providers/digitalocean", "@opencode/ai/providers/digitalocean/decisions"),
        ("@opencode/ai/providers/google", "@opencode/ai/providers/google/decisions"),
        ("@opencode/ai/providers/openai-compatible", "@opencode/ai/providers/openai-compatible/decisions"),
        ("@opencode/ai/providers/vercel-ai-gateway", "@opencode/ai/providers/vercel-ai-gateway/decisions"),
    ] {
        assert_eq!(decision_identity(root, &settings, &env), decision_identity(subpath, &settings, &env), "{root}");
    }
}

fn decision_identity(specifier: &str, settings: &Settings, env: &MapEnv) -> Result<(String, String), String> {
    match ProviderPackages::builtin().decision(specifier, "fixture", settings, env) {
        Ok(RuntimeModel::Decision(model)) => Ok((model.provider, model.route.id.clone())),
        Ok(_) => panic!("{specifier}: expected a decision model"),
        Err(error) => Err(error.message().to_string()),
    }
}

fn tool_json(tool: &ToolDefinition) -> serde_json::Value {
    let mut value = serde_json::to_value(tool).expect("tool json");
    value.as_object_mut().expect("tool object").insert("type".to_string(), json!("tool"));
    value
}

#[test]
fn openai_image_generation_matches_the_hosted_tool_shape() {
    assert_eq!(
        tool_json(&opencode_ai::providers::openai::image_generation(ImageGenerationOptions::default())),
        json!({
            "type": "tool",
            "name": "image_generation",
            "description": "Generate or edit an image using OpenAI's hosted image generation tool.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "native": { "openai": { "type": "image_generation" } }
        })
    );

    let recorded = opencode_ai::providers::openai::image_generation(ImageGenerationOptions {
        action: Some("auto".to_string()),
        output_compression: Some(10),
        output_format: Some("jpeg".to_string()),
        partial_images: Some(0),
        quality: Some("low".to_string()),
        size: Some("1024x1024".to_string()),
        ..ImageGenerationOptions::default()
    });
    assert_eq!(
        tool_json(&recorded),
        json!({
            "type": "tool",
            "name": "image_generation",
            "description": "Generate or edit an image using OpenAI's hosted image generation tool.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "native": { "openai": {
                "type": "image_generation",
                "action": "auto",
                "output_compression": 10,
                "output_format": "jpeg",
                "partial_images": 0,
                "quality": "low",
                "size": "1024x1024"
            } }
        })
    );

    let configured = opencode_ai::providers::openai::image_generation(ImageGenerationOptions {
        background: Some("transparent".to_string()),
        input_fidelity: Some("high".to_string()),
        ..ImageGenerationOptions::default()
    });
    assert_eq!(
        tool_json(&configured)["native"],
        json!({ "openai": { "type": "image_generation", "background": "transparent", "input_fidelity": "high" } })
    );
}

#[test]
fn meta_web_search_matches_the_hosted_tool_shape() {
    assert_eq!(
        tool_json(&opencode_ai::providers::meta::web_search(WebSearchOptions::default())),
        json!({
            "type": "tool",
            "name": "web_search",
            "description": "Search the web with Meta's hosted search tool.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "native": { "meta": { "type": "web_search" } }
        })
    );

    let configured = opencode_ai::providers::meta::web_search(WebSearchOptions {
        search_context_size: Some("high".to_string()),
        user_location: Some(UserLocation {
            city: Some("Paris".to_string()),
            country: Some("FR".to_string()),
            ..UserLocation::default()
        }),
    });
    assert_eq!(
        tool_json(&configured)["native"],
        json!({ "meta": {
            "type": "web_search",
            "search_context_size": "high",
            "user_location": { "type": "approximate", "city": "Paris", "country": "FR" }
        } })
    );
}

#[test]
fn meta_image_generation_matches_the_hosted_tool_shape() {
    assert_eq!(
        tool_json(&opencode_ai::providers::meta::image_generation(
            opencode_ai::providers::meta::ImageGenerationOptions::default()
        )),
        json!({
            "type": "tool",
            "name": "image_generation",
            "description": "Generate or edit an image with Muse Image.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "native": { "meta": { "type": "image_generation" } }
        })
    );

    let configured =
        opencode_ai::providers::meta::image_generation(opencode_ai::providers::meta::ImageGenerationOptions {
            size: Some("512x512".to_string()),
            output_format: Some("png".to_string()),
            reasoning_strength: Some("high".to_string()),
            enable_image_search: Some(false),
            enable_web_search: Some(true),
            enable_shell: Some(false),
        });
    assert_eq!(
        tool_json(&configured)["native"],
        json!({ "meta": {
            "type": "image_generation",
            "size": "512x512",
            "output_format": "png",
            "reasoning_strength": "high",
            "enable_image_search": false,
            "enable_web_search": true,
            "enable_shell": false
        } })
    );
}

#[test]
fn alibaba_hosted_tools_match_the_recorded_shapes() {
    assert_eq!(
        tool_json(&opencode_ai::providers::alibaba::web_search()),
        json!({
            "type": "tool",
            "name": "web_search",
            "description": "Search the web with Alibaba's hosted search tool.",
            "inputSchema": { "type": "object", "properties": {} },
            "native": { "alibaba": { "type": "web_search" } }
        })
    );
    assert_eq!(
        tool_json(&opencode_ai::providers::alibaba::web_extractor()),
        json!({
            "type": "tool",
            "name": "web_extractor",
            "description": "Extract web page content with Alibaba's hosted tool.",
            "inputSchema": { "type": "object", "properties": {} },
            "native": { "alibaba": { "type": "web_extractor" } }
        })
    );
    assert_eq!(
        tool_json(&opencode_ai::providers::alibaba::code_interpreter()),
        json!({
            "type": "tool",
            "name": "code_interpreter",
            "description": "Execute code with Alibaba's hosted interpreter.",
            "inputSchema": { "type": "object", "properties": {} },
            "native": { "alibaba": { "type": "code_interpreter" } }
        })
    );
}
