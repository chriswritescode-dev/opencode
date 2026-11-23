use opencode_ai::MapEnv;
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::compile::{Compiled, StreamOptions, compile};
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
    let RuntimeModel::Language(model) = runtime;
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
