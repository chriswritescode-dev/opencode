use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use opencode_ai::route::auth::AuthInput;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::schema::{
    ContentPart, GenerationOptions, HttpTimeout, LlmRequest, Message, RequestTimeout, SystemMessage, SystemPart,
    TextPart, TimeoutOptions, UserMessage,
};
use opencode_ai::{Auth, Env, ExecutionContext, MapEnv, Model, ProviderPackages, RuntimeModel};
use opencode_catalog::aisdk_native::{Context, Target, aisdk, rewrite};
use opencode_catalog::credential::CreateInput;
use opencode_catalog::model::ModelService;
use opencode_catalog::resolver::{self, Error, Phase, Resolver, RuntimeInfo, with_variant};
use opencode_catalog::variant::Support;
use opencode_contract::codec::{JsonMap, decode_json, encode_json};
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType,
    CredentialValue, IntegrationID, IntegrationMethodID, ModelInfo, ModelRef, ModelVariant, ProviderID, ProviderInfo,
    ProviderInfoActivation, ProviderSettings, ProviderTransport,
};
use opencode_location::state::Registration;

#[derive(Default)]
struct Options {
    provider: Option<&'static str>,
    canonical: Option<&'static str>,
    model_id: Option<&'static str>,
    compatibility: Option<Value>,
    settings: Option<Value>,
    headers: Option<Value>,
    body: Option<Value>,
    variants: Option<Value>,
    limit: Option<Value>,
}

fn catalog(package: Option<&str>, options: Options) -> RuntimeInfo {
    let provider = options.provider.unwrap_or("test-provider");
    let model_id = options.model_id.unwrap_or("api-test-model");
    let settings = options.settings.unwrap_or_else(|| json!({}));
    let headers = options.headers.unwrap_or_else(|| json!({ "x-test": "header" }));
    let body = options.body.unwrap_or_else(|| json!({ "custom_extension": { "enabled": true } }));
    let variants = options.variants.unwrap_or_else(|| json!([]));
    let limit = options.limit.unwrap_or_else(|| json!({ "context": 100, "output": 20 }));
    let mut target: Target = serde_json::from_value(json!({
        "settings": settings,
        "headers": headers,
        "body": body,
        "package": package,
        "variants": variants,
    }))
    .expect("a catalog target");
    let context = Context {
        provider_id: provider.to_owned(),
        canonical: options.canonical.map(str::to_owned),
        model_id: Some(model_id.to_owned()),
        shape: None,
        settings: None,
        specifier: package.map(str::to_owned),
    };
    rewrite(&mut target, &context, &ProviderPackages::builtin().specifiers());
    let runtime_settings = target.settings.clone().unwrap_or_default();
    let mut info = Map::new();
    info.insert("id".to_owned(), json!("test-model"));
    info.insert("modelID".to_owned(), json!(model_id));
    info.insert("providerID".to_owned(), json!(provider));
    info.insert("name".to_owned(), json!("Test model"));
    info.insert("capabilities".to_owned(), json!({ "tools": true, "input": ["text"], "output": ["text"] }));
    info.insert("time".to_owned(), json!({ "released": 0 }));
    info.insert("cost".to_owned(), json!([]));
    info.insert("status".to_owned(), json!("active"));
    info.insert("enabled".to_owned(), json!(true));
    info.insert("limit".to_owned(), limit);
    if let Some(canonical) = options.canonical {
        info.insert("canonical".to_owned(), json!(canonical));
    }
    if let Some(compatibility) = options.compatibility {
        info.insert("compatibility".to_owned(), compatibility);
    }
    if let Some(package) = target.package {
        info.insert("package".to_owned(), json!(package));
    }
    if let Some(settings) = target.settings {
        info.insert("settings".to_owned(), Value::Object(settings));
    }
    if let Some(headers) = target.headers {
        info.insert("headers".to_owned(), json!(headers));
    }
    if let Some(body) = target.body {
        info.insert("body".to_owned(), Value::Object(body));
    }
    if let Some(variants) = target.variants {
        info.insert("variants".to_owned(), json!(variants));
    }
    let info = decode_json::<ModelInfo>(&Value::Object(info).to_string()).expect("a catalog model");
    let settings =
        decode_json::<ProviderSettings>(&Value::Object(runtime_settings).to_string()).expect("runtime settings");
    RuntimeInfo::new(info, Some(settings))
}

fn variant_runtime(runtime: &RuntimeInfo, variant: &str) -> RuntimeInfo {
    let info = with_variant(&runtime.info, Some(variant)).expect("the variant resolves");
    let settings = info
        .settings
        .as_ref()
        .map(|settings| decode_json::<ProviderSettings>(&encode_json(settings)).expect("variant settings"));
    RuntimeInfo::new(info, settings)
}

fn resolve(runtime: &RuntimeInfo, credential: Option<&CredentialValue>) -> Result<Model, Error> {
    resolver::from_catalog_model(runtime, credential, &MapEnv::default(), &resolver::NoBridge)
}

fn resolve_in(env: MapEnv, runtime: &RuntimeInfo, credential: Option<&CredentialValue>) -> Result<Model, Error> {
    resolver::from_catalog_model(runtime, credential, &env, &resolver::NoBridge)
}

fn resolve_variant(runtime: &RuntimeInfo, variant: &str, credential: Option<&CredentialValue>) -> Result<Model, Error> {
    resolve(&variant_runtime(runtime, variant), credential)
}

fn key(secret: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: secret.to_owned(), metadata: None })
}

fn oauth(token: &str, metadata: Option<Value>) -> CredentialValue {
    oauth_method(token, "device", metadata)
}

fn oauth_method(token: &str, method: &str, metadata: Option<Value>) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(method.to_owned()),
        refresh: "refresh".to_owned(),
        access: token.to_owned(),
        expires: 0.0,
        metadata: metadata.map(json_map),
    })
}

fn external(method: &str, metadata: Value) -> CredentialValue {
    CredentialValue::External(CredentialExternal {
        r#type: CredentialExternalType::External,
        method_id: IntegrationMethodID(method.to_owned()),
        metadata: Some(json_map(metadata)),
    })
}

fn json_map(value: Value) -> JsonMap<Value> {
    decode_json::<JsonMap<Value>>(&value.to_string()).expect("a JSON map")
}

fn auth_headers(model: &Model, env: &dyn Env, url: &str) -> Vec<(String, String)> {
    model
        .route
        .auth
        .apply(
            AuthInput { method: "POST".into(), url: url.into(), body: "{}".into(), headers: Vec::new(), now_ms: 0 },
            env,
        )
        .expect("auth applies")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(existing, _)| existing == name).map(|(_, value)| value.as_str())
}

fn compile_body(model: &Model, request: LlmRequest) -> Value {
    compile(model, request, &StreamOptions::default(), &ExecutionContext::system()).expect("compiles").body
}

fn prompt() -> LlmRequest {
    LlmRequest { messages: vec![user("Hello")], ..LlmRequest::default() }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage { id: None, content: vec![text_part(text)], provider_metadata: None })
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextPart { text: text.to_string(), cache: None, provider_metadata: None })
}

fn millis(value: f64) -> HttpTimeout {
    HttpTimeout::Millis(serde_json::Number::from_f64(value).expect("a finite timeout"))
}

fn options_json(model: &Model) -> Value {
    serde_json::to_value(&model.options).expect("request options serialize")
}

fn failure<T>(result: Result<T, Error>) -> Error {
    match result {
        Ok(_) => panic!("the resolution succeeds unexpectedly"),
        Err(error) => error,
    }
}

fn env(values: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(values.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())))
}

fn models(harness: &crate::provider::Harness) -> Arc<ModelService> {
    ModelService::new(
        harness.providers.clone(),
        harness.bus.clone(),
        harness.location.clone(),
        crate::provider::config_env(&HashMap::new()),
    )
}

fn service_resolver(harness: &crate::provider::Harness, models: &Arc<ModelService>) -> Arc<Resolver> {
    Resolver::new(
        harness.providers.clone(),
        models.clone(),
        harness.integrations.clone(),
        Arc::new(MapEnv::default()),
        Arc::new(resolver::NoBridge),
    )
}

fn provider(id: &str, package: &str, settings: Option<Value>) -> ProviderInfo {
    ProviderInfo {
        id: ProviderID(id.to_owned()),
        canonical: None,
        integration_id: None,
        name: id.to_owned(),
        activation: ProviderInfoActivation::Enabled,
        package: package.to_owned(),
        compatibility: None,
        settings: settings
            .map(|settings| decode_json::<ProviderSettings>(&settings.to_string()).expect("provider settings")),
        headers: None,
        body: None,
    }
}

fn catalog_model(provider: &str, id: &str, model_id: &str, extra: Value) -> ModelInfo {
    let mut info = Map::new();
    info.insert("id".to_owned(), json!(id));
    info.insert("modelID".to_owned(), json!(model_id));
    info.insert("providerID".to_owned(), json!(provider));
    info.insert("name".to_owned(), json!(id));
    info.insert("capabilities".to_owned(), json!({ "tools": true, "input": ["text"], "output": ["text"] }));
    info.insert("time".to_owned(), json!({ "released": 0 }));
    info.insert("cost".to_owned(), json!([]));
    info.insert("status".to_owned(), json!("active"));
    info.insert("enabled".to_owned(), json!(true));
    info.insert("limit".to_owned(), json!({ "context": 100, "output": 20 }));
    info.insert("variants".to_owned(), json!([]));
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            info.insert(key, value);
        }
    }
    decode_json::<ModelInfo>(&Value::Object(info).to_string()).expect("a catalog model")
}

async fn register(harness: &crate::provider::Harness, provider: ProviderInfo, models: &[ModelInfo]) -> Registration {
    let id = provider.id.clone();
    let models = models.to_vec();
    harness
        .providers
        .transform(move |editor| {
            editor.add(provider.clone(), &models, None);
            editor.update(&id, |_| {});
            Ok(())
        })
        .await
}

#[test]
fn constructs_native_azure_requests_with_deployment_ids_and_projected_resource_urls() {
    let responses = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                model_id: Some("responses-deployment"),
                settings: Some(json!({"resourceName": "modern-resource", "apiVersion": "2025-01-01-preview"})),
                ..Default::default()
            },
        ),
        Some(&key("secret")),
    )
    .expect("responses resolve");
    let chat = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                model_id: Some("chat-deployment"),
                settings: Some(json!({"resourceName": "modern-resource", "useCompletionUrls": true})),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("chat resolves");
    let deployment = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                model_id: Some("legacy-url-deployment"),
                settings: Some(json!({
                    "resourceName": "modern-resource",
                    "apiVersion": "2025-01-01-preview",
                    "useDeploymentBasedUrls": true,
                })),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("deployment resolves");
    let compatible = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai-compatible")),
            Options {
                provider: Some("azure"),
                model_id: Some("legacy-deployment"),
                settings: Some(json!({
                    "resourceName": "legacy-resource",
                    "baseURL": "https://legacy-resource.cognitiveservices.azure.com/openai",
                })),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("compatible resolves");

    assert_eq!(responses.id, "responses-deployment");
    assert_eq!(responses.provider, "azure");
    assert_eq!(responses.route.id, "azure-openai-responses");
    assert_eq!(
        responses.route.endpoint.base_url.as_deref(),
        Some("https://modern-resource.openai.azure.com/openai/v1")
    );
    assert_eq!(responses.route.endpoint.query.get("api-version"), Some("2025-01-01-preview"));
    assert_eq!(chat.id, "chat-deployment");
    assert_eq!(chat.provider, "azure");
    assert_eq!(chat.route.id, "azure-openai-chat");
    assert_eq!(deployment.id, "legacy-url-deployment");
    assert_eq!(deployment.provider, "azure");
    assert_eq!(
        deployment.route.endpoint.base_url.as_deref(),
        Some("https://modern-resource.openai.azure.com/openai/deployments/legacy-url-deployment")
    );
    assert_eq!(deployment.route.endpoint.query.get("api-version"), Some("2025-01-01-preview"));
    assert_eq!(compatible.id, "legacy-deployment");
    assert_eq!(compatible.provider, "azure");
    assert_eq!(
        compatible.route.endpoint.base_url.as_deref(),
        Some("https://legacy-resource.cognitiveservices.azure.com/openai")
    );
}

#[test]
fn routes_compatible_packages_to_the_host_package() {
    let routes = [
        ("alibaba-coding-plan-cn", "alibaba-chat"),
        ("baseten", "baseten-chat"),
        ("deepseek", "deepseek-chat"),
        ("fireworks-ai", "fireworks-chat"),
        ("moonshotai-cn", "moonshot-chat"),
        ("zhipuai", "zai-chat"),
        ("zhipuai-coding-plan", "zai-coding-chat"),
        ("cerebras", "openai-compatible-chat"),
        ("deepinfra", "openai-compatible-chat"),
        ("groq", "openai-compatible-chat"),
        ("togetherai", "openai-compatible-chat"),
    ];
    for (provider, route) in routes {
        let model = resolve(
            &catalog(
                Some(&aisdk("@ai-sdk/openai-compatible")),
                Options {
                    provider: Some(provider),
                    settings: Some(json!({"baseURL": "https://provider.example/v1/openai", "apiKey": "fixture"})),
                    ..Default::default()
                },
            ),
            None,
        )
        .expect("the compatible model resolves");
        assert_eq!(model.provider, provider);
        assert_eq!(model.route.id, route);
        assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://provider.example/v1/openai"));
        let body = compile_body(&model, prompt());
        assert_eq!(body["messages"], json!([{ "role": "user", "content": "Hello" }]));
        assert!(body.get("apiKey").is_none());
    }
}

#[test]
fn resolves_environment_templates_before_native_providers_inspect_endpoints() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/azure")),
        Options {
            provider: Some("azure"),
            settings: Some(json!({"baseURL": "https://${AZURE_HOST}/openai"})),
            ..Default::default()
        },
    );
    let model = resolve_in(env(&[("AZURE_HOST", "resource.openai.azure.com")]), &runtime, None).expect("resolves");

    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://resource.openai.azure.com/openai/v1"));
    assert_eq!(model.route.endpoint.query.get("api-version"), Some("v1"));
}

#[test]
fn maps_bedrock_mantle_models_to_chat_and_responses() {
    let credential = key("secret");
    let settings = json!({"region": "us-east-2"});
    let responses = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
            Options { model_id: Some("openai.gpt-5.5"), settings: Some(settings.clone()), ..Default::default() },
        ),
        Some(&credential),
    )
    .expect("responses resolve");
    let chat = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
            Options { model_id: Some("openai.gpt-oss-20b"), settings: Some(settings), ..Default::default() },
        ),
        Some(&credential),
    )
    .expect("chat resolves");

    assert_eq!(responses.route.id, "bedrock-mantle-responses");
    assert_eq!(responses.route.endpoint.base_url.as_deref(), Some("https://bedrock-mantle.us-east-2.api.aws/v1"));
    assert_eq!(chat.route.id, "bedrock-mantle-chat");
    assert_eq!(chat.route.endpoint.base_url.as_deref(), Some("https://bedrock-mantle.us-east-2.api.aws/v1"));
}

#[test]
fn prefers_the_configured_mantle_region_over_the_environment() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
        Options {
            provider: Some("amazon-bedrock"),
            model_id: Some("openai.gpt-5.5"),
            settings: Some(json!({
                "region": "us-west-2",
                "baseURL": "https://bedrock-mantle.${AWS_REGION}.api.aws/openai/v1",
            })),
            ..Default::default()
        },
    );
    let model = resolve_in(env(&[("AWS_REGION", "us-east-1")]), &runtime, Some(&key("secret"))).expect("resolves");

    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://bedrock-mantle.us-west-2.api.aws/openai/v1"));
}

#[test]
fn resolves_bedrock_mantle_catalog_endpoints_from_the_configured_region() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
        Options {
            provider: Some("amazon-bedrock"),
            model_id: Some("openai.gpt-5.5"),
            settings: Some(json!({
                "region": "us-west-2",
                "baseURL": "https://bedrock-mantle.${AWS_REGION}.api.aws/openai/v1",
            })),
            ..Default::default()
        },
    );
    let model = resolve(&runtime, Some(&key("secret"))).expect("resolves");
    let settings = runtime.info.settings.as_ref().expect("catalog settings");
    let base_url = settings.rest.0.iter().find(|(name, _)| name == "baseURL").map(|(_, value)| value.clone());

    assert_eq!(model.route.id, "bedrock-mantle-responses");
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://bedrock-mantle.us-west-2.api.aws/openai/v1"));
    assert_eq!(base_url, Some(json!("https://bedrock-mantle.us-west-2.api.aws/openai/v1")));
}

#[test]
fn fills_mantle_catalog_urls_from_the_provider_region() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
        Options {
            provider: Some("amazon-bedrock"),
            model_id: Some("openai.gpt-5.5"),
            settings: Some(json!({
                "region": "us-west-2",
                "baseURL": "https://bedrock-mantle.${AWS_REGION}.api.aws/openai/v1",
            })),
            ..Default::default()
        },
    );
    let model = resolve(&runtime, Some(&key("secret"))).expect("resolves");

    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://bedrock-mantle.us-west-2.api.aws/openai/v1"));
}

#[test]
fn uses_the_api_model_id_and_keeps_catalog_api_keys_out_of_the_body() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai")),
        Options {
            settings: Some(json!({"baseURL": "https://openai.example/v1"})),
            limit: Some(json!({"context": 100, "input": 80, "output": 20})),
            ..Default::default()
        },
    );
    assert_eq!(runtime.info.id, "test-model");
    let model = resolve(&runtime, Some(&key("secret"))).expect("resolves");

    assert_eq!(model.id, "api-test-model");
    assert_eq!(model.provider, "test-provider");
    assert_eq!(model.route.id, "openai-responses");
    assert_eq!(model.route.provider_metadata_key.as_deref(), Some("openai"));
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://openai.example/v1"));
    let options = options_json(&model);
    assert_eq!(options["headers"]["x-test"], json!("header"));
    assert_eq!(options["http"]["body"], json!({"custom_extension": {"enabled": true}}));
    let body = compile_body(&model, prompt());
    assert!(body.get("max_output_tokens").is_none());
    assert!(!body.to_string().contains("max_output_tokens"));

    let configured = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"apiKey": "secret", "baseURL": "https://openai.example/v1"})),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("the configured model resolves");
    let body = compile_body(&configured, prompt()).to_string();
    assert!(!body.contains("apiKey"));
    assert!(!body.contains("secret"));
}

#[test]
fn treats_an_empty_configured_api_key_as_omitted() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai")),
        Options { settings: Some(json!({"apiKey": "", "baseURL": "https://openai.example/v1"})), ..Default::default() },
    );
    let environment = env(&[("OPENAI_API_KEY", "environment-key")]);
    let model = resolve_in(environment.clone(), &runtime, None).expect("resolves");
    let headers = auth_headers(&model, &environment, "https://openai.example/v1/responses");

    assert_eq!(header(&headers, "authorization"), Some("Bearer environment-key"));
}

#[test]
fn keeps_native_environment_auth_strict_without_an_api_key() {
    let model = resolve_in(
        MapEnv::default(),
        &catalog(
            Some(&aisdk("@ai-sdk/google")),
            Options { settings: Some(json!({"baseURL": "https://google.example.com/v1"})), ..Default::default() },
        ),
        None,
    )
    .expect("the google model resolves");
    let applied = model.route.auth.apply(
        AuthInput {
            method: "POST".into(),
            url: "https://google.example.com/v1".into(),
            body: "{}".into(),
            headers: Vec::new(),
            now_ms: 0,
        },
        &MapEnv::default(),
    );

    assert!(applied.is_err());
}

#[test]
fn uses_merged_api_settings_for_openai_compatible_auth_and_request_defaults() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai-compatible")),
            Options {
                canonical: Some("deepseek"),
                compatibility: Some(json!({
                    "reasoningField": "vendor_reasoning",
                    "requireReasoning": true,
                    "maxTokensField": "max_completion_tokens",
                    "requireFinishReason": false,
                    "requireAssistantAfterTool": true,
                })),
                settings: Some(json!({
                    "apiKey": "settings-secret",
                    "baseURL": "https://compatible.example/v1",
                    "compatibility": "strict",
                })),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("resolves");
    let headers = auth_headers(&model, &MapEnv::default(), "https://compatible.example/v1/chat/completions");
    let request = LlmRequest {
        generation: GenerationOptions { max_tokens: Some(serde_json::Number::from(10)), ..Default::default() },
        ..prompt()
    };
    let body = compile_body(&model, request);
    let compatibility = model.compatibility.as_ref().expect("compatibility");

    assert_eq!(header(&headers, "authorization"), Some("Bearer settings-secret"));
    assert_eq!(model.route.id, "openai-compatible-chat");
    assert_eq!(model.provider, "deepseek");
    assert_eq!(model.route.provider_metadata_key.as_deref(), Some("deepseek"));
    assert_eq!(compatibility["reasoningField"], json!("vendor_reasoning"));
    assert_eq!(compatibility["requireReasoning"], json!(true));
    assert_eq!(compatibility["maxTokensField"], json!("max_completion_tokens"));
    assert_eq!(compatibility["requireFinishReason"], json!(false));
    assert_eq!(compatibility["requireAssistantAfterTool"], json!(true));
    assert_eq!(body["max_completion_tokens"], json!(10));
    assert!(body.get("max_tokens").is_none());
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://compatible.example/v1"));
    assert!(options_json(&model).get("http").and_then(|http| http.get("body")).is_none());
}

#[test]
fn resolves_provider_urls_from_environment_without_mutating_the_catalog_model() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai-compatible")),
        Options { settings: Some(json!({"baseURL": "https://${ACME_HOST}/v1"})), ..Default::default() },
    );
    let settings = runtime.info.settings.as_ref().expect("catalog settings");
    let base_url = settings.rest.0.iter().find(|(name, _)| name == "baseURL").map(|(_, value)| value.clone());
    assert_eq!(base_url, Some(json!("https://${ACME_HOST}/v1")));

    let model = resolve_in(env(&[("ACME_HOST", "api.acme.test")]), &runtime, None).expect("resolves");

    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://api.acme.test/v1"));
}

#[test]
fn rejects_unresolved_variables() {
    let error = failure(resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai-compatible")),
            Options {
                settings: Some(json!({"baseURL": "https://${REQUIRED_HOST}/${REQUIRED_PATH}/v1"})),
                ..Default::default()
            },
        ),
        None,
    ));
    match &error {
        Error::UnresolvedProviderVariables { variables, .. } => {
            assert_eq!(variables, &vec!["REQUIRED_HOST".to_owned(), "REQUIRED_PATH".to_owned()])
        }
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(
        error.to_string(),
        "Cannot initialize test-provider/test-model: REQUIRED_HOST, REQUIRED_PATH are required to resolve the provider endpoint"
    );

    let nested = failure(resolve_in(
        env(&[("PROVIDER_HOST", "${MISSING_HOST}")]),
        &catalog(
            Some(&aisdk("@ai-sdk/openai-compatible")),
            Options { settings: Some(json!({"baseURL": "https://${PROVIDER_HOST}/v1"})), ..Default::default() },
        ),
        None,
    ));
    match &nested {
        Error::UnresolvedProviderVariables { variables, .. } => assert_eq!(variables, &vec!["MISSING_HOST".to_owned()]),
        other => panic!("unexpected error {other:?}"),
    }

    let native = failure(resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/google")),
            Options { settings: Some(json!({"baseURL": "https://${REQUIRED_HOST}/v1"})), ..Default::default() },
        ),
        None,
    ));
    match &native {
        Error::UnresolvedProviderVariables { variables, .. } => {
            assert_eq!(variables, &vec!["REQUIRED_HOST".to_owned()])
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn overlays_selected_openai_variant_settings_and_bodies() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai")),
        Options {
            settings: Some(json!({"baseURL": "https://openai.example/v1"})),
            variants: Some(json!([{
                "id": "xhigh",
                "settings": {
                    "reasoningEffort": "xhigh",
                    "reasoningSummary": "auto",
                    "include": ["reasoning.encrypted_content"],
                },
                "headers": {"x-variant": "high"},
                "body": {"store": false, "service_tier": "priority", "temperature": 0.2},
            }])),
            ..Default::default()
        },
    );
    let model = resolve_variant(&runtime, "xhigh", Some(&key("secret"))).expect("the variant resolves");
    let options = options_json(&model);
    let body = compile_body(&model, prompt());

    assert_eq!(options["headers"]["x-test"], json!("header"));
    assert_eq!(options["headers"]["x-variant"], json!("high"));
    assert_eq!(
        options["http"]["body"],
        json!({
            "custom_extension": {"enabled": true},
            "store": false,
            "service_tier": "priority",
            "temperature": 0.2,
        })
    );
    assert_eq!(
        options["providerOptions"],
        json!({
            "store": false,
            "reasoningEffort": "xhigh",
            "reasoningSummary": "auto",
            "include": ["reasoning.encrypted_content"],
        })
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["reasoning"], json!({"effort": "xhigh", "summary": "auto"}));
}

#[test]
fn sends_xai_responses_effort_variants_with_reasoning_summaries() {
    let variants = opencode_catalog::variant::resolve(
        &opencode_catalog::variant::Model {
            id: "test-model".to_owned(),
            model_id: "grok-4.6".to_owned(),
            provider_id: "xai".to_owned(),
            package: Some("@opencode/ai/providers/xai".to_owned()),
            output: 20,
        },
        &[Support::Effort { values: Some(vec!["low".to_owned(), "high".to_owned()]) }],
    );
    let runtime = catalog(
        Some("@opencode/ai/providers/xai"),
        Options {
            provider: Some("xai"),
            model_id: Some("grok-4.6"),
            variants: Some(serde_json::to_value(variants).expect("variants serialize")),
            ..Default::default()
        },
    );
    let model = resolve_variant(&runtime, "high", Some(&key("secret"))).expect("the variant resolves");
    let body = compile_body(&model, prompt());

    assert_eq!(model.route.id, "xai-responses");
    assert_eq!(body["reasoning"], json!({"effort": "high", "summary": "auto"}));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn overlays_selected_openai_compatible_variant_bodies() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai-compatible")),
        Options {
            settings: Some(json!({"baseURL": "https://compatible.example/v1"})),
            variants: Some(json!([{
                "id": "high",
                "settings": {},
                "headers": {},
                "body": {"store": false, "reasoning_effort": "high"},
            }])),
            ..Default::default()
        },
    );
    let model = resolve_variant(&runtime, "high", None).expect("the variant resolves");

    assert_eq!(
        options_json(&model)["http"]["body"],
        json!({"custom_extension": {"enabled": true}, "store": false, "reasoning_effort": "high"})
    );
}

#[test]
fn overlays_selected_anthropic_variant_settings() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/anthropic")),
        Options {
            settings: Some(json!({"baseURL": "https://anthropic.example/v1"})),
            variants: Some(json!([{
                "id": "high",
                "settings": {"thinking": {"type": "enabled", "budgetTokens": 12000}},
                "headers": {},
                "body": {},
            }])),
            ..Default::default()
        },
    );
    let model = resolve_variant(&runtime, "high", None).expect("the variant resolves");
    let options = options_json(&model);

    assert_eq!(options["http"]["body"], json!({"custom_extension": {"enabled": true}}));
    assert_eq!(options["providerOptions"], json!({"thinking": {"type": "enabled", "budgetTokens": 12000}}));
}

#[test]
fn rejects_an_explicit_unavailable_variant() {
    let runtime = catalog(
        Some(&aisdk("@ai-sdk/openai")),
        Options { settings: Some(json!({"baseURL": "https://openai.example/v1"})), ..Default::default() },
    );
    let error = with_variant(&runtime.info, Some("unknown")).expect_err("the unknown variant rejects");

    match &error {
        Error::VariantUnavailable { variant, .. } => assert_eq!(variant, "unknown"),
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.to_string(), "Variant unavailable for test-provider/test-model: unknown");
    assert_eq!(with_variant(&runtime.info, Some("default")).expect("the default variant"), runtime.info);
}

#[test]
fn maps_catalog_anthropic_models_into_native_routes() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/anthropic")),
            Options { settings: Some(json!({"baseURL": "https://anthropic.example/v1"})), ..Default::default() },
        ),
        None,
    )
    .expect("resolves");

    assert_eq!(model.route.id, "anthropic-messages");
    assert_eq!(model.route.provider_metadata_key.as_deref(), Some("anthropic"));
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://anthropic.example/v1"));
}

#[test]
fn prefers_stored_credentials_over_configured_auth() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"apiKey": "configured-secret", "baseURL": "https://openai.example/v1"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&key("stored-secret")),
    )
    .expect("resolves");
    let headers = auth_headers(&model, &MapEnv::default(), "https://openai.example/v1/responses");

    assert_eq!(header(&headers, "authorization"), Some("Bearer stored-secret"));
}

#[test]
fn uses_resolved_credentials_for_bearer_auth() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"baseURL": "https://openai.example/v1"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&key("secret")),
    )
    .expect("resolves");
    let headers = auth_headers(&model, &MapEnv::default(), "https://openai.example/v1/responses");

    assert_eq!(header(&headers, "authorization"), Some("Bearer secret"));
}

#[test]
fn lets_azure_cli_requests_past_package_auth() {
    let credential = external("azure-cli", json!({"resourceName": "cli-resource"}));
    let azure = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                settings: Some(json!({"resourceName": "cli-resource"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&credential),
    )
    .expect("the azure model resolves");
    let foundry = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/anthropic")),
            Options {
                provider: Some("azure"),
                settings: Some(json!({"baseURL": "https://cli-resource.services.ai.azure.com/anthropic/v1"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&credential),
    )
    .expect("the anthropic model resolves");
    let url = "https://cli-resource.openai.azure.com/openai/v1/responses";
    let azure_headers = auth_headers(&azure, &MapEnv::default(), url);
    let foundry_headers = auth_headers(&foundry, &MapEnv::default(), url);

    assert_eq!(azure.route.endpoint.base_url.as_deref(), Some("https://cli-resource.openai.azure.com/openai/v1"));
    assert_eq!(header(&azure_headers, "api-key"), Some("azure-cli"));
    assert_eq!(header(&foundry_headers, "authorization"), Some("Bearer azure-cli"));
}

#[test]
fn reports_provider_configuration_errors_from_supported_packages() {
    let credential = CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: "secret".to_owned(),
        metadata: Some(json_map(json!({"resourceName": "migrated-resource"}))),
    });
    let error = failure(resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                model_id: Some("responses-deployment"),
                settings: Some(json!({"apiVersion": "2025-01-01-preview"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&credential),
    ));
    match &error {
        Error::ModelConfiguration { detail, .. } => assert_eq!(detail, "Azure requires resourceName or baseURL"),
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.to_string(), "Cannot initialize azure/test-model: Azure requires resourceName or baseURL");

    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/azure")),
            Options {
                provider: Some("azure"),
                model_id: Some("responses-deployment"),
                settings: Some(json!({
                    "apiVersion": "2025-01-01-preview",
                    "resourceName": "configured-resource",
                })),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&credential),
    )
    .expect("the configured model resolves");

    assert_eq!(
        model.route.endpoint.base_url.as_deref(),
        Some("https://configured-resource.openai.azure.com/openai/v1")
    );
    assert!(options_json(&model).get("http").and_then(|http| http.get("body")).is_none());
}

#[test]
fn does_not_project_oauth_account_metadata_into_the_request_body() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"baseURL": "https://openai.example/v1"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&oauth("secret", Some(json!({"server": "https://console.example", "orgID": "org_123"})))),
    )
    .expect("resolves");

    assert!(options_json(&model).get("http").and_then(|http| http.get("body")).is_none());
}

#[test]
fn keeps_non_chatgpt_oauth_credentials_on_the_configured_endpoint() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"baseURL": "https://openai.example/v1"})),
                headers: Some(json!({})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&oauth("oauth-token", Some(json!({"accountID": "acct_123"})))),
    )
    .expect("resolves");
    let headers = auth_headers(&model, &MapEnv::default(), "https://openai.example/v1/responses");

    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://openai.example/v1"));
    assert_eq!(header(&headers, "authorization"), Some("Bearer oauth-token"));
    assert_eq!(header(&headers, "chatgpt-account-id"), None);
}

#[test]
fn applies_plugin_projected_openai_endpoint_and_headers() {
    let model = resolve(
        &catalog(
            Some("@opencode/ai/providers/openai"),
            Options {
                settings: Some(json!({"baseURL": "https://chatgpt.com/backend-api/codex"})),
                headers: Some(json!({"chatgpt-account-id": "acct_123"})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&oauth_method("chatgpt-token", "chatgpt-browser", Some(json!({"accountID": "acct_123"})))),
    )
    .expect("resolves");
    let request = LlmRequest {
        system: vec![
            SystemPart::Text { text: "Base instructions.".into(), cache: None, metadata: None },
            SystemPart::Text { text: "Project instructions.".into(), cache: None, metadata: None },
        ],
        messages: vec![
            user("Hello"),
            Message::System(SystemMessage { id: None, content: vec![text_part("Updated instructions.")], tools: None }),
        ],
        ..LlmRequest::default()
    };
    let body = compile_body(&model, request);
    let headers = auth_headers(&model, &MapEnv::default(), "https://chatgpt.com/backend-api/codex/responses");

    assert_eq!(model.route.id, "openai-responses");
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://chatgpt.com/backend-api/codex"));
    assert_eq!(options_json(&model)["headers"]["chatgpt-account-id"], json!("acct_123"));
    assert_eq!(header(&headers, "authorization"), Some("Bearer chatgpt-token"));
    assert!(body.get("instructions").is_none());
    assert_eq!(
        body["input"],
        json!([
            {"type": "message", "role": "developer", "content": "Base instructions.\nProject instructions."},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hello"}]},
            {"type": "message", "role": "developer", "content": "Updated instructions."},
        ])
    );
}

#[test]
fn does_not_route_native_openai_compatible_packages_to_the_codex_backend() {
    let model = resolve(
        &catalog(
            Some("@opencode/ai/providers/openai-compatible"),
            Options { settings: Some(json!({"baseURL": "https://compatible.example/v1"})), ..Default::default() },
        ),
        Some(&oauth_method("chatgpt-token", "chatgpt-browser", Some(json!({"accountID": "acct_123"})))),
    )
    .expect("resolves");

    assert_eq!(model.route.id, "openai-compatible-chat");
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("https://compatible.example/v1"));
}

#[test]
fn maps_legacy_openai_organization_and_project_settings_to_headers() {
    let model = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options { settings: Some(json!({"organization": "org_123", "project": "proj_123"})), ..Default::default() },
        ),
        None,
    )
    .expect("resolves");
    let options = options_json(&model);

    assert_eq!(options["headers"]["OpenAI-Organization"], json!("org_123"));
    assert_eq!(options["headers"]["OpenAI-Project"], json!("proj_123"));
}

#[test]
fn loads_supported_aisdk_catalog_packages_as_native_routes() {
    let google = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/google")),
            Options { settings: Some(json!({"thinkingConfig": {"thinkingBudget": 1024}})), ..Default::default() },
        ),
        None,
    )
    .expect("google resolves");
    let openrouter = resolve(
        &catalog(
            Some(&aisdk("@openrouter/ai-sdk-provider")),
            Options { settings: Some(json!({"reasoning": {"effort": "high"}})), ..Default::default() },
        ),
        None,
    )
    .expect("openrouter resolves");
    let cerebras = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/cerebras")),
            Options { settings: Some(json!({"reasoningEffort": "high"})), ..Default::default() },
        ),
        None,
    )
    .expect("cerebras resolves");
    let deepinfra = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/deepinfra")),
            Options {
                settings: Some(json!({
                    "baseURL": "https://deepinfra.example/provider-root",
                    "reasoningEffort": "none",
                })),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("deepinfra resolves");
    let togetherai = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/togetherai")),
            Options { settings: Some(json!({"reasoningEffort": "high"})), ..Default::default() },
        ),
        None,
    )
    .expect("togetherai resolves");
    let groq = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/groq")),
            Options {
                settings: Some(json!({"reasoningEffort": "high", "parallelToolCalls": false})),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("groq resolves");
    let mistral = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/mistral")),
            Options {
                settings: Some(json!({
                    "safePrompt": true,
                    "promptCacheKey": "session-123",
                    "reasoningEffort": "high",
                })),
                ..Default::default()
            },
        ),
        None,
    )
    .expect("mistral resolves");
    let xai = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/xai")),
            Options { settings: Some(json!({"reasoningEffort": "high"})), ..Default::default() },
        ),
        None,
    )
    .expect("xai resolves");
    let bedrock = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/amazon-bedrock")),
            Options {
                settings: Some(json!({"region": "us-east-1", "topP": 0.8, "serviceTier": "priority"})),
                body: Some(json!({})),
                ..Default::default()
            },
        ),
        Some(&key("secret")),
    )
    .expect("bedrock resolves");
    let mantle = resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/amazon-bedrock/mantle")),
            Options {
                model_id: Some("openai.gpt-oss-120b"),
                settings: Some(json!({"region": "us-east-1", "topP": 0.6})),
                ..Default::default()
            },
        ),
        Some(&key("secret")),
    )
    .expect("mantle resolves");
    let workers = resolve(
        &catalog(
            Some("@opencode/ai/providers/cloudflare-workers-ai"),
            Options {
                model_id: Some("@cf/meta/llama-3.1-8b-instruct"),
                settings: Some(json!({"accountId": "account"})),
                ..Default::default()
            },
        ),
        Some(&key("workers-secret")),
    )
    .expect("workers resolves");

    assert_eq!(google.route.id, "gemini");
    assert_eq!(options_json(&google)["providerOptions"], json!({"thinkingConfig": {"thinkingBudget": 1024}}));
    assert_eq!(openrouter.route.id, "openrouter");
    assert_eq!(options_json(&openrouter)["providerOptions"], json!({"reasoning": {"effort": "high"}}));
    assert_eq!(cerebras.route.id, "cerebras-chat");
    assert_eq!(options_json(&cerebras)["providerOptions"], json!({"reasoningEffort": "high"}));
    assert_eq!(cerebras.provider, "test-provider");
    assert_eq!(deepinfra.route.id, "deepinfra-chat");
    assert_eq!(deepinfra.route.endpoint.base_url.as_deref(), Some("https://deepinfra.example/provider-root/openai"));
    assert_eq!(options_json(&deepinfra)["providerOptions"], json!({"reasoningEffort": "none"}));
    assert_eq!(deepinfra.provider, "test-provider");
    assert_eq!(togetherai.route.id, "togetherai-chat");
    assert_eq!(options_json(&togetherai)["providerOptions"], json!({"reasoningEffort": "high"}));
    assert_eq!(togetherai.provider, "test-provider");
    assert_eq!(groq.route.id, "groq-chat");
    assert_eq!(groq.route.protocol.id(), "groq-chat");
    assert_eq!(options_json(&groq)["providerOptions"], json!({"reasoningEffort": "high", "parallelToolCalls": false}));
    assert_eq!(groq.provider, "test-provider");
    assert_eq!(mistral.route.id, "mistral-chat");
    assert_eq!(
        options_json(&mistral)["providerOptions"],
        json!({"safePrompt": true, "promptCacheKey": "session-123", "reasoningEffort": "high"})
    );
    assert_eq!(mistral.provider, "test-provider");
    assert_eq!(xai.route.id, "xai-responses");
    assert_eq!(
        options_json(&xai)["providerOptions"],
        json!({"reasoningEffort": "high", "store": false, "include": ["reasoning.encrypted_content"]})
    );
    assert_eq!(bedrock.route.id, "bedrock-converse");
    assert_eq!(options_json(&bedrock)["topP"], json!(0.8));
    assert_eq!(options_json(&bedrock)["http"]["body"], json!({"serviceTier": {"type": "priority"}}));
    assert_eq!(mantle.route.id, "bedrock-mantle-chat");
    assert_eq!(options_json(&mantle)["topP"], json!(0.6));
    assert_eq!(
        workers.route.endpoint.base_url.as_deref(),
        Some("https://api.cloudflare.com/client/v4/accounts/account/ai/v1")
    );
}

#[test]
fn rejects_aisdk_packages_without_a_bridge() {
    let error = failure(resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/perplexity")),
            Options { settings: Some(json!({"baseURL": "https://perplexity.example/v1"})), ..Default::default() },
        ),
        None,
    ));

    match &error {
        Error::ModelInitialization { phase, package, detail, .. } => {
            assert_eq!(*phase, Phase::Load);
            assert_eq!(package, "aisdk:@ai-sdk/perplexity");
            assert_eq!(detail, "load failed for aisdk:@ai-sdk/perplexity");
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn rejects_models_without_a_provider_package() {
    let error = failure(resolve(&catalog(None, Options::default()), None));

    match &error {
        Error::UnsupportedPackage { package, .. } => assert_eq!(package, "unknown"),
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.to_string(), "Unsupported package for test-provider/test-model: unknown");
    assert!(resolver::has_package(&catalog(Some(&aisdk("@ai-sdk/openai")), Options::default()).info));
    assert!(!resolver::has_package(&catalog(None, Options::default()).info));
}

#[test]
fn rejects_native_provider_compaction() {
    let error = failure(resolve(
        &catalog(
            Some(&aisdk("@ai-sdk/openai")),
            Options {
                settings: Some(json!({"baseURL": "https://openai.example/v1", "compaction": {"type": "native"}})),
                ..Default::default()
            },
        ),
        None,
    ));

    match &error {
        Error::UnsupportedCompaction { route, .. } => assert_eq!(route, "openai-responses"),
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(
        error.to_string(),
        "Provider compaction is not supported by test-provider/test-model (openai-responses)"
    );
}

#[test]
fn applies_provider_timeouts_to_image_video_and_decision_models() {
    let expected = Some(RequestTimeout::Config(TimeoutOptions {
        total: Some(millis(12000.0)),
        headers: Some(millis(3000.0)),
        chunk: Some(millis(4000.0)),
    }));
    let image = match media("@opencode/ai/providers/openai/images", "timed", "gpt-image-2") {
        RuntimeModel::Image(image) => image,
        _ => panic!("expected an image model"),
    };
    let video = match media("@opencode/ai/providers/xai/videos", "timed", "grok-imagine-video-1.5") {
        RuntimeModel::Video(video) => video,
        _ => panic!("expected a video model"),
    };
    let decision = match media("@opencode/ai/providers/google/decisions", "timed", "gemini-2.5-flash") {
        RuntimeModel::Decision(decision) => decision,
        _ => panic!("expected a decision model"),
    };

    assert_eq!(image.transport.as_ref().and_then(|transport| transport.timeout.clone()), expected);
    assert_eq!(video.transport.as_ref().and_then(|transport| transport.timeout.clone()), expected);
    assert_eq!(decision.transport.as_ref().and_then(|transport| transport.timeout.clone()), expected);
}

#[tokio::test]
async fn resolves_a_stored_credential_through_the_provider_integration() {
    let harness = crate::provider::setup();
    let provider_id = ProviderID("test-provider".to_owned());
    let model = catalog_model("test-provider", "fixture-model", "api-fixture", json!({}));
    let _registration = register(
        &harness,
        provider("test-provider", "aisdk:@ai-sdk/openai", Some(json!({"baseURL": "https://openai.example/v1"}))),
        std::slice::from_ref(&model),
    )
    .await;
    harness
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("test-provider".to_owned()),
            value: key("stored-secret"),
            label: Some("stored".to_owned()),
            activate: Some(true),
        })
        .expect("the credential is created");
    let models = models(&harness);
    let resolver = service_resolver(&harness, &models);
    let reference = ModelRef { id: "fixture-model".to_owned(), provider_id: provider_id.clone(), variant: None };
    let resolved = resolver.resolve(Some(&reference)).await.expect("resolves").expect("a resolved model");
    let headers = auth_headers(&resolved.model, &MapEnv::default(), "https://openai.example/v1/responses");

    assert_eq!(resolved.model.id, "api-fixture");
    assert_eq!(resolved.model.provider, "test-provider");
    assert_eq!(resolved.reference, reference);
    assert_eq!(resolved.limit, model.limit);
    assert_eq!(resolved.capabilities, model.capabilities);
    assert_eq!(header(&headers, "authorization"), Some("Bearer stored-secret"));
}

#[tokio::test]
async fn uses_no_auth_for_an_enabled_provider_without_credentials() {
    let harness = crate::provider::setup();
    let provider_id = ProviderID("test-provider".to_owned());
    let model = catalog_model("test-provider", "fixture-model", "api-fixture", json!({}));
    let _registration = register(
        &harness,
        provider(
            "test-provider",
            "aisdk:@ai-sdk/openai",
            Some(json!({"baseURL": "https://openai.example/v1", "chunkTimeout": 4000, "transport": "websocket"})),
        ),
        std::slice::from_ref(&model),
    )
    .await;
    let models = models(&harness);
    let resolver = service_resolver(&harness, &models);
    let reference = ModelRef { id: "fixture-model".to_owned(), provider_id: provider_id.clone(), variant: None };
    let resolved = resolver.resolve(Some(&reference)).await.expect("resolves").expect("a resolved model");

    assert!(matches!(&resolved.model.route.auth, Auth::None));
    assert_eq!(resolved.chunk_timeout, Some(opencode_catalog::provider::HttpTimeout::Millis(4000.0)));
    assert_eq!(resolved.transport, Some(ProviderTransport::Websocket));
}

#[tokio::test]
async fn returns_none_for_unknown_and_empty_selections() {
    let harness = crate::provider::setup();
    let provider_id = ProviderID("test-provider".to_owned());
    let packaged = catalog_model("test-provider", "fixture-model", "api-fixture", json!({}));
    let _packaged = register(
        &harness,
        provider("test-provider", "aisdk:@ai-sdk/openai", Some(json!({"baseURL": "https://openai.example/v1"}))),
        std::slice::from_ref(&packaged),
    )
    .await;
    let unpackaged = catalog_model("unpackaged", "loose-model", "api-loose", json!({}));
    let _unpackaged = register(&harness, provider("unpackaged", "", None), std::slice::from_ref(&unpackaged)).await;
    let models = models(&harness);
    let resolver = service_resolver(&harness, &models);
    let missing = ModelRef { id: "missing".to_owned(), provider_id: provider_id.clone(), variant: None };

    assert!(resolver.resolve(Some(&missing)).await.expect("resolves").is_none());
    let fallback = resolver.fallback().await.expect("fallback").expect("a fallback model");
    assert_eq!(fallback.id, "fixture-model");
    let resolved = resolver.resolve(None).await.expect("resolves").expect("a resolved model");
    assert_eq!(resolved.reference.id, "fixture-model");
}

#[tokio::test]
async fn resolves_an_image_model_by_reference() {
    let harness = crate::provider::setup();
    let provider_id = ProviderID("media".to_owned());
    let image = catalog_model(
        "media",
        "image",
        "gpt-image-2",
        json!({"type": "image", "package": "@opencode/ai/providers/openai/images"}),
    );
    let chat = catalog_model("media", "chat", "api-chat", json!({}));
    let _registration = register(&harness, provider("media", "aisdk:@ai-sdk/openai", None), &[image, chat]).await;
    let models = models(&harness);
    let resolver = service_resolver(&harness, &models);
    let resolved = resolver
        .resolve_image(&ModelRef { id: "image".to_owned(), provider_id: provider_id.clone(), variant: None })
        .await
        .expect("resolves")
        .expect("an image model");
    let chat = resolver
        .resolve_image(&ModelRef { id: "chat".to_owned(), provider_id: provider_id.clone(), variant: None })
        .await
        .expect("resolves");

    assert_eq!(resolved.id, "gpt-image-2");
    assert!(chat.is_none());
}

#[tokio::test]
async fn keeps_resolved_catalog_values_independent_of_later_model_edits() {
    let harness = crate::provider::setup();
    let provider_id = ProviderID("test-provider".to_owned());
    let model = catalog_model("test-provider", "fixture-model", "api-fixture", json!({}));
    let _registration = register(
        &harness,
        provider("test-provider", "aisdk:@ai-sdk/openai", Some(json!({"baseURL": "https://openai.example/v1"}))),
        std::slice::from_ref(&model),
    )
    .await;
    let models = models(&harness);
    let resolver = service_resolver(&harness, &models);
    let current = models.get(&provider_id, "fixture-model").await.expect("reads models").expect("the model");
    let resolved = resolver.resolve_model(&current, None).await.expect("resolves");
    let _edited = models
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, "fixture-model", |model| {
                    model.limit.context = 100_000.0;
                    model.capabilities.tools = false;
                    model.variants.push(ModelVariant {
                        id: "high".to_owned(),
                        settings: None,
                        headers: None,
                        body: None,
                    });
                });
                Ok(())
            }
        })
        .await;
    let updated = models.get(&provider_id, "fixture-model").await.expect("reads models").expect("the model");

    assert_eq!(resolved.limit.context, 100.0);
    assert!(resolved.capabilities.tools);
    assert_eq!(updated.limit.context, 100_000.0);
    assert!(!updated.capabilities.tools);
    assert_eq!(updated.variants.len(), 1);
}

fn media(package: &str, provider: &'static str, model_id: &'static str) -> RuntimeModel {
    resolver::from_catalog_media(
        &catalog(
            Some(package),
            Options {
                provider: Some(provider),
                model_id: Some(model_id),
                settings: Some(json!({
                    "apiKey": "secret",
                    "timeout": 12000,
                    "headerTimeout": 3000,
                    "chunkTimeout": 4000,
                })),
                ..Default::default()
            },
        ),
        Some(&key("secret")),
        &MapEnv::default(),
        &resolver::NoBridge,
    )
    .expect("the media model resolves")
}
