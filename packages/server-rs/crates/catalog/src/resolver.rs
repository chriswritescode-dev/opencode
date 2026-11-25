use std::sync::{Arc, LazyLock};

use opencode_ai::providers::ProviderPackages;
use opencode_ai::{
    AiError, AiErrorReason, Auth, ClientTls, Env, HttpOptions, HttpTimeout, Model, RequestEncoding, RequestOptions,
    RequestTimeout, RuntimeModel, Settings, TimeoutOptions, merge_request_options,
};
use opencode_contract::codec::{self, Encode, encode_json};
use opencode_contract::generated::{
    CredentialValue, IntegrationID, ModelCapabilities, ModelCompatibility, ModelCost, ModelInfo, ModelInfoLimit,
    ModelRef, ModelSettings, ModelType, ModelTypeString2, ModelTypeString3, ModelTypeString4, ProviderCache,
    ProviderCompaction, ProviderEncoding, ProviderID, ProviderInfo, ProviderInfoActivation, ProviderSettings,
    ProviderSettingsChunkTimeout, ProviderTls, ProviderTransport,
};
use regex_lite::Regex;
use serde_json::Value;

use crate::aisdk_native::{JsonMap, is_aisdk, merge_headers, merge_overlay, package_name};
use crate::integration::IntegrationService;
use crate::model::ModelService;
use crate::provider::{self, ProviderService};

const CAPABILITY_KEYS: [&str; 3] = ["supportsPromptCacheKey", "streamUsage", "supportsEffortUpdates"];

const API_KEY_AUTH_PACKAGES: [&str; 33] = [
    "@ai-sdk/openai",
    "@ai-sdk/anthropic",
    "@ai-sdk/cerebras",
    "@ai-sdk/deepinfra",
    "@ai-sdk/openai-compatible",
    "@ai-sdk/google",
    "@ai-sdk/groq",
    "@ai-sdk/mistral",
    "@ai-sdk/togetherai",
    "@ai-sdk/xai",
    "@openrouter/ai-sdk-provider",
    "@ai-sdk/azure",
    "@opencode/ai/providers/openai",
    "@opencode/ai/providers/anthropic",
    "@opencode/ai/providers/anthropic-compatible",
    "@opencode/ai/providers/baseten",
    "@opencode/ai/providers/cerebras",
    "@opencode/ai/providers/cloudflare-ai-gateway",
    "@opencode/ai/providers/cloudflare-workers-ai",
    "@opencode/ai/providers/deepinfra",
    "@opencode/ai/providers/deepseek",
    "@opencode/ai/providers/fireworks",
    "@opencode/ai/providers/openai-compatible",
    "@opencode/ai/providers/openai-compatible/decisions",
    "@opencode/ai/providers/google",
    "@opencode/ai/providers/google/interactions",
    "@opencode/ai/providers/groq",
    "@opencode/ai/providers/mistral",
    "@opencode/ai/providers/togetherai",
    "@opencode/ai/providers/vercel-ai-gateway",
    "@opencode/ai/providers/xai",
    "@opencode/ai/providers/openrouter",
    "@opencode/ai/providers/azure",
];

const VARIANT_PROVIDER_KEYS: [&str; 6] = ["timeout", "headerTimeout", "chunkTimeout", "transport", "tls", "cache"];

static PROVIDER_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([^}]+)\}").expect("the provider variable pattern is valid"));

static PACKAGE_REGISTRY: LazyLock<ProviderPackages> = LazyLock::new(ProviderPackages::builtin);

#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub info: ModelInfo,
    pub settings: Option<ProviderSettings>,
}

impl RuntimeInfo {
    pub fn new(info: ModelInfo, settings: Option<ProviderSettings>) -> Self {
        Self { info, settings }
    }

    pub fn from_model(info: ModelInfo) -> Self {
        Self { info, settings: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Load,
    Init,
    Construct,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Variant unavailable for {}/{}: {}", provider_id.0, model_id, variant)]
    VariantUnavailable { provider_id: ProviderID, model_id: String, variant: String },
    #[error("Unsupported package for {}/{}: {}", provider_id.0, model_id, package)]
    UnsupportedPackage { provider_id: ProviderID, model_id: String, package: String },
    #[error("Cannot initialize {}/{}: {}", provider_id.0, model_id, detail)]
    ModelConfiguration { provider_id: ProviderID, model_id: String, package: String, detail: String },
    #[error("Cannot initialize {}/{}: {}", provider_id.0, model_id, detail)]
    ModelInitialization { provider_id: ProviderID, model_id: String, package: String, phase: Phase, detail: String },
    #[error(
        "Cannot initialize {}/{}: {} {} required to resolve the provider endpoint",
        provider_id.0,
        model_id,
        variables.join(", "),
        if variables.len() == 1 { "is" } else { "are" }
    )]
    UnresolvedProviderVariables { provider_id: ProviderID, model_id: String, variables: Vec<String> },
    #[error("Provider compaction is not supported by {}/{} ({})", provider_id.0, model_id, route)]
    UnsupportedCompaction { provider_id: ProviderID, model_id: String, route: String },
    #[error(transparent)]
    Authorization(#[from] crate::integration::AuthorizationError),
    #[error(transparent)]
    Model(#[from] crate::model::ModelError),
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    #[error(transparent)]
    Integration(#[from] crate::integration::IntegrationError),
}

pub trait AisdkBridge: Send + Sync {
    fn model(&self, info: &RuntimeInfo, _settings: &Settings, _environment: &dyn Env) -> Result<RuntimeModel, Error> {
        let package = info.info.package.clone().unwrap_or_else(|| "unknown".to_owned());
        Err(Error::ModelInitialization {
            provider_id: info.info.provider_id.clone(),
            model_id: info.info.id.clone(),
            package: package.clone(),
            phase: Phase::Load,
            detail: format!("load failed for {package}"),
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoBridge;

impl AisdkBridge for NoBridge {}

pub fn with_variant(model: &ModelInfo, variant: Option<&str>) -> Result<ModelInfo, Error> {
    let requested = variant.filter(|variant| *variant != "default");
    let Some(requested) = requested else {
        return Ok(model.clone());
    };
    let Some(variant) = model.variants.iter().find(|variant| variant.id == requested) else {
        return Err(Error::VariantUnavailable {
            provider_id: model.provider_id.clone(),
            model_id: model.id.clone(),
            variant: requested.to_owned(),
        });
    };
    let base = model.settings.as_ref().map(provider::json_object);
    let overlay = variant.settings.as_ref().map(|settings| {
        let mut json = provider::json_object(settings);
        for key in VARIANT_PROVIDER_KEYS {
            json.remove(key);
        }
        json
    });
    let settings = merge_overlay(base.as_ref(), overlay.as_ref()).map(provider::decode_object::<ModelSettings>);
    let headers = merge_headers(
        model.headers.as_ref().map(provider::headers_to_native).as_ref(),
        variant.headers.as_ref().map(provider::headers_to_native).as_ref(),
    )
    .map(provider::headers_from_native);
    let body = merge_overlay(
        model.body.as_ref().map(provider::body_to_native).as_ref(),
        variant.body.as_ref().map(provider::body_to_native).as_ref(),
    )
    .map(provider::body_from_native);
    Ok(ModelInfo { settings, headers, body, ..model.clone() })
}

pub fn from_catalog_model(
    info: &RuntimeInfo,
    credential: Option<&CredentialValue>,
    environment: &dyn Env,
    bridge: &dyn AisdkBridge,
) -> Result<Model, Error> {
    let resolved = prepare_runtime_model(info);
    if is_aisdk(resolved.info.package.as_deref().unwrap_or("")) {
        let base = resolved.settings.as_ref().map(provider::json_object);
        let credential_settings =
            native_credential_settings(&resolved, resolved.info.package.as_deref().unwrap_or(""), credential);
        let merged = merge_overlay(base.as_ref(), Some(&credential_settings)).unwrap_or_default();
        let mapped = prepare_provider_settings(&resolved, merged, environment)?;
        let runtime = RuntimeInfo {
            info: resolved.info.clone(),
            settings: Some(provider::decode_object::<ProviderSettings>(mapped.clone())),
        };
        let model = bridge.model(&runtime, &Settings::new(mapped), environment)?;
        let RuntimeModel::Language(model) = model else {
            return Err(unsupported(&resolved.info));
        };
        return finalize(&resolved, model);
    }
    let configured = resolved.settings.clone().unwrap_or_else(empty_provider_settings);
    let specifier = package_name(resolved.info.package.as_deref().unwrap_or(""));
    if specifier.is_empty() {
        return Err(unsupported(&resolved.info));
    }
    let mapped = prepare_provider_settings(&resolved, provider::native_settings(&configured), environment)?;
    let settings = package_settings(&resolved, mapped, &specifier, credential);
    if !provider::builtin_packages().contains(specifier.as_str()) {
        let model = bridge.model(&resolved, &Settings::new(settings), environment)?;
        let RuntimeModel::Language(model) = model else {
            return Err(unsupported(&resolved.info));
        };
        return finalize(&resolved, apply_runtime_policy(&resolved, &configured, model));
    }
    let model = PACKAGE_REGISTRY
        .model(&specifier, &resolved.info.model_id, &Settings::new(settings), environment)
        .map_err(|error| construction_error(&resolved, &specifier, error))?;
    let RuntimeModel::Language(model) = model else {
        return Err(unsupported(&resolved.info));
    };
    finalize(&resolved, apply_runtime_policy(&resolved, &configured, model))
}

pub fn from_catalog_media(
    info: &RuntimeInfo,
    credential: Option<&CredentialValue>,
    environment: &dyn Env,
    bridge: &dyn AisdkBridge,
) -> Result<RuntimeModel, Error> {
    let resolved = prepare_runtime_model(info);
    let configured = resolved.settings.clone().unwrap_or_else(empty_provider_settings);
    let specifier = package_name(resolved.info.package.as_deref().unwrap_or(""));
    if specifier.is_empty() || is_aisdk(&specifier) {
        return Err(unsupported(&resolved.info));
    }
    let mapped = prepare_provider_settings(&resolved, provider::native_settings(&configured), environment)?;
    let settings = package_settings(&resolved, mapped, &specifier, credential);
    if !provider::builtin_packages().contains(specifier.as_str()) {
        let runtime = bridge.model(&resolved, &Settings::new(settings), environment)?;
        return media_runtime(&resolved, &configured, runtime);
    }
    let runtime = PACKAGE_REGISTRY
        .model(&specifier, &resolved.info.model_id, &Settings::new(settings), environment)
        .map_err(|error| construction_error(&resolved, &specifier, error))?;
    media_runtime(&resolved, &configured, runtime)
}

pub fn has_package(model: &ModelInfo) -> bool {
    model.package.is_some()
}

pub struct Resolved {
    pub model: Model,
    pub reference: ModelRef,
    pub capabilities: ModelCapabilities,
    pub cost: Vec<ModelCost>,
    pub limit: ModelInfoLimit,
    pub compaction: Option<ProviderCompaction>,
    pub cache: Option<ProviderCache>,
    pub transport: Option<ProviderTransport>,
    pub chunk_timeout: Option<provider::HttpTimeout>,
}

pub struct Resolver {
    providers: Arc<ProviderService>,
    models: Arc<ModelService>,
    integrations: Arc<IntegrationService>,
    environment: Arc<dyn Env>,
    bridge: Arc<dyn AisdkBridge>,
}

impl Resolver {
    pub fn new(
        providers: Arc<ProviderService>,
        models: Arc<ModelService>,
        integrations: Arc<IntegrationService>,
        environment: Arc<dyn Env>,
        bridge: Arc<dyn AisdkBridge>,
    ) -> Arc<Self> {
        Arc::new(Self { providers, models, integrations, environment, bridge })
    }

    pub async fn resolve(&self, requested: Option<&ModelRef>) -> Result<Option<Resolved>, Error> {
        let Some(requested) = requested else {
            let Some(model) = self.fallback().await? else {
                return Ok(None);
            };
            return Ok(Some(self.resolve_model(&model, None).await?));
        };
        let Some(model) = self.models.get(&requested.provider_id, &requested.id).await? else {
            return Ok(None);
        };
        Ok(Some(self.resolve_model(&model, requested.variant.as_deref()).await?))
    }

    pub async fn fallback(&self) -> Result<Option<ModelInfo>, Error> {
        let model = self.models.default_model().await?;
        if let Some(model) = model.filter(has_package) {
            return Ok(Some(model));
        }
        let available = self.models.available(None).await?;
        Ok(available.into_iter().find(|model| has_package(model) && crate::model::supports_text(model)))
    }

    pub async fn resolve_model(&self, model: &ModelInfo, variant: Option<&str>) -> Result<Resolved, Error> {
        let (provider, credential, runtime) = self.prepare(model, variant).await?;
        let mut resolved =
            from_catalog_model(&runtime, credential.as_ref(), self.environment.as_ref(), self.bridge.as_ref())?;
        if is_keyless(provider.as_ref(), credential.as_ref(), &runtime) {
            resolved.route.auth = Auth::None;
        }
        Ok(Resolved {
            model: resolved,
            reference: ModelRef {
                id: model.id.clone(),
                provider_id: model.provider_id.clone(),
                variant: variant.map(str::to_owned),
            },
            capabilities: model.capabilities.clone(),
            cost: model.cost.clone(),
            limit: model.limit.clone(),
            compaction: runtime.settings.as_ref().and_then(|settings| settings.compaction.clone()),
            cache: runtime.settings.as_ref().and_then(|settings| settings.cache.clone()),
            transport: provider_transport(provider.as_ref()),
            chunk_timeout: provider
                .as_ref()
                .and_then(|provider| provider.settings.as_ref())
                .and_then(|settings| chunk_timeout(settings.chunk_timeout.as_ref())),
        })
    }

    pub async fn resolve_image(
        &self,
        reference: &ModelRef,
    ) -> Result<Option<opencode_ai::modalities::image::ImageModel>, Error> {
        let Some((info, runtime)) = self.load_media(reference, ModelType::String3(ModelTypeString3::Image)).await?
        else {
            return Ok(None);
        };
        let RuntimeModel::Image(model) = runtime else {
            return Err(unsupported(&info));
        };
        Ok(Some(model))
    }

    pub async fn resolve_video(
        &self,
        reference: &ModelRef,
    ) -> Result<Option<opencode_ai::modalities::video::VideoModel>, Error> {
        let Some((info, runtime)) = self.load_media(reference, ModelType::String4(ModelTypeString4::Video)).await?
        else {
            return Ok(None);
        };
        let RuntimeModel::Video(model) = runtime else {
            return Err(unsupported(&info));
        };
        Ok(Some(model))
    }

    pub async fn resolve_decision(
        &self,
        reference: &ModelRef,
    ) -> Result<Option<opencode_ai::modalities::decision::DecisionModel>, Error> {
        let Some((info, runtime)) = self.load_media(reference, ModelType::String2(ModelTypeString2::Decision)).await?
        else {
            return Ok(None);
        };
        let RuntimeModel::Decision(model) = runtime else {
            return Err(unsupported(&info));
        };
        Ok(Some(model))
    }

    async fn prepare(
        &self,
        selected: &ModelInfo,
        variant: Option<&str>,
    ) -> Result<(Option<ProviderInfo>, Option<CredentialValue>, RuntimeInfo), Error> {
        let provider = self.providers.get(&selected.provider_id).await?;
        let integration_id = provider
            .as_ref()
            .and_then(|provider| provider.integration_id.clone())
            .unwrap_or_else(|| IntegrationID(selected.provider_id.0.clone()));
        let connection = self.integrations.connection().active(&integration_id).await?;
        let credential = match connection.as_ref() {
            Some(connection) => self.integrations.connection().resolve(connection).await?,
            None => None,
        };
        let variant_model = with_variant(selected, variant)?;
        let settings = merge_provider_settings(
            provider.as_ref().and_then(|provider| provider.settings.as_ref()),
            variant_model.settings.as_ref(),
        );
        Ok((provider, credential, RuntimeInfo { info: variant_model, settings }))
    }

    async fn load_media(
        &self,
        reference: &ModelRef,
        model_type: ModelType,
    ) -> Result<Option<(ModelInfo, RuntimeModel)>, Error> {
        let available = self.models.available(Some(&[model_type])).await?;
        let Some(model) =
            available.into_iter().find(|model| model.provider_id == reference.provider_id && model.id == reference.id)
        else {
            return Ok(None);
        };
        let (provider, credential, mut runtime) = self.prepare(&model, reference.variant.as_deref()).await?;
        if is_keyless(provider.as_ref(), credential.as_ref(), &runtime) {
            let mut settings = runtime.settings.take().unwrap_or_else(empty_provider_settings);
            set_setting(&mut settings, "apiKey", Value::Bool(false));
            runtime.settings = Some(settings);
        }
        let loaded =
            from_catalog_media(&runtime, credential.as_ref(), self.environment.as_ref(), self.bridge.as_ref())?;
        Ok(Some((model, loaded)))
    }
}

fn prepare_runtime_model(info: &RuntimeInfo) -> RuntimeInfo {
    let Some(settings) = info.settings.as_ref() else {
        return info.clone();
    };
    if setting(settings, "apiKey").and_then(Value::as_str) != Some("") {
        return info.clone();
    }
    let mut settings = settings.clone();
    settings.rest.0.retain(|(key, _)| key != "apiKey");
    RuntimeInfo { info: info.info.clone(), settings: Some(settings) }
}

fn prepare_provider_settings(
    info: &RuntimeInfo,
    mut settings: JsonMap,
    environment: &dyn Env,
) -> Result<JsonMap, Error> {
    let Some(base_url) = settings.get("baseURL").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(settings);
    };
    let region = settings.get("region").and_then(Value::as_str).map(str::to_owned);
    let prepared = prepare_provider_url(info, &base_url, region.as_deref(), environment)?;
    if prepared != base_url {
        settings.insert("baseURL".to_owned(), Value::String(prepared));
    }
    Ok(settings)
}

fn prepare_provider_url(
    info: &RuntimeInfo,
    base_url: &str,
    region: Option<&str>,
    environment: &dyn Env,
) -> Result<String, Error> {
    if !base_url.contains("${") {
        return Ok(base_url.to_owned());
    }
    let prepared = PROVIDER_VARIABLE.replace_all(base_url, |captures: &regex_lite::Captures<'_>| {
        let Some(name) = captures.get(1) else {
            return placeholder(captures);
        };
        let value = match name.as_str() {
            "AWS_REGION" if region.is_some() => region.map(str::to_owned),
            _ => environment.var(name.as_str()),
        };
        value.unwrap_or_else(|| placeholder(captures))
    });
    let variables = unresolved_variables(&prepared);
    if !variables.is_empty() {
        return Err(Error::UnresolvedProviderVariables {
            provider_id: info.info.provider_id.clone(),
            model_id: info.info.id.clone(),
            variables,
        });
    }
    Ok(prepared.into_owned())
}

fn placeholder(captures: &regex_lite::Captures<'_>) -> String {
    captures.get(0).map(|matched| matched.as_str().to_owned()).unwrap_or_default()
}

fn unresolved_variables(base_url: &str) -> Vec<String> {
    let mut variables: Vec<String> = Vec::new();
    for captures in PROVIDER_VARIABLE.captures_iter(base_url) {
        let Some(name) = captures.get(1) else {
            continue;
        };
        if variables.iter().any(|existing| existing == name.as_str()) {
            continue;
        }
        variables.push(name.as_str().to_owned());
    }
    variables
}

fn native_credential_settings(info: &RuntimeInfo, specifier: &str, credential: Option<&CredentialValue>) -> JsonMap {
    let Some(credential) = credential else {
        return JsonMap::new();
    };
    match credential {
        CredentialValue::Key(key) => JsonMap::from_iter([("apiKey".to_owned(), Value::String(key.key.clone()))]),
        CredentialValue::Oauth(oauth) => token_settings(specifier, &oauth.access),
        CredentialValue::External(_) => {
            if specifier.starts_with("@opencode/ai/providers/amazon-bedrock") {
                return JsonMap::from_iter([("auth".to_owned(), Value::String("sigv4".to_owned()))]);
            }
            if info.info.provider_id.0 == "azure" {
                return token_settings(specifier, "azure-cli");
            }
            JsonMap::new()
        }
    }
}

fn token_settings(specifier: &str, token: &str) -> JsonMap {
    if matches!(specifier, "@opencode/ai/providers/anthropic" | "@opencode/ai/providers/anthropic-compatible") {
        return JsonMap::from_iter([("authToken".to_owned(), Value::String(token.to_owned()))]);
    }
    if specifier == "@opencode/ai/providers/google-vertex"
        || specifier.starts_with("@opencode/ai/providers/google-vertex/")
    {
        return JsonMap::from_iter([("accessToken".to_owned(), Value::String(token.to_owned()))]);
    }
    JsonMap::from_iter([("apiKey".to_owned(), Value::String(token.to_owned()))])
}

fn package_settings(
    info: &RuntimeInfo,
    mut settings: JsonMap,
    specifier: &str,
    credential: Option<&CredentialValue>,
) -> JsonMap {
    if credential.is_some() {
        for key in ["accessToken", "apiKey", "authToken"] {
            settings.remove(key);
        }
    }
    if let Some(canonical) = info.info.canonical.as_ref() {
        settings.insert("provider".to_owned(), Value::String(canonical.0.clone()));
    }
    for (key, value) in native_credential_settings(info, specifier, credential) {
        settings.insert(key, value);
    }
    if let Some(headers) = info.info.headers.as_ref() {
        settings.insert("headers".to_owned(), Value::Object(provider::json_object(headers)));
    }
    if let Some(body) = info.info.body.as_ref().filter(|body| !body.0.is_empty()) {
        settings.insert("body".to_owned(), Value::Object(provider::json_object(body)));
    }
    settings
}

fn apply_runtime_policy(info: &RuntimeInfo, configured: &ProviderSettings, mut model: Model) -> Model {
    model.provider = info
        .info
        .canonical
        .as_ref()
        .map(|canonical| canonical.0.clone())
        .unwrap_or_else(|| info.info.provider_id.0.clone());
    model.capabilities = merge_capabilities(model.capabilities, info.info.compatibility.as_ref());
    model.compatibility = merge_compatibility(model.compatibility, info.info.compatibility.as_ref());
    let overlay = request_overlay(configured);
    let options = merge_request_options(&[model.options.as_ref(), Some(&overlay)]);
    model.options = options;
    model
}

fn merge_capabilities(base: Option<Value>, compatibility: Option<&ModelCompatibility>) -> Option<Value> {
    let Some(compatibility) = compatibility else {
        return base;
    };
    let mut merged = base
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    if let Some(value) = compatibility.supports_prompt_cache_key {
        merged.insert("promptCacheKey".to_owned(), Value::Bool(value));
    }
    if let Some(value) = compatibility.stream_usage {
        merged.insert("streamUsage".to_owned(), Value::Bool(value));
    }
    if let Some(value) = compatibility.supports_effort_updates {
        let mut reasoning = merged.get("reasoning").and_then(Value::as_object).cloned().unwrap_or_default();
        reasoning.insert("updates".to_owned(), Value::Bool(value));
        merged.insert("reasoning".to_owned(), Value::Object(reasoning));
    }
    Some(Value::Object(merged))
}

fn merge_compatibility(base: Option<Value>, compatibility: Option<&ModelCompatibility>) -> Option<Value> {
    let Some(compatibility) = compatibility else {
        return base;
    };
    let mut overlay = provider::json_object(compatibility);
    for key in CAPABILITY_KEYS {
        overlay.remove(key);
    }
    let mut merged = base
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    for (key, value) in overlay {
        merged.insert(key, value);
    }
    Some(Value::Object(merged))
}

fn request_overlay(configured: &ProviderSettings) -> RequestOptions {
    let timeouts = provider::timeouts(configured);
    RequestOptions {
        timeout: Some(RequestTimeout::Config(TimeoutOptions {
            total: timeouts.timeout.map(request_timeout),
            headers: Some(request_timeout(timeouts.header_timeout)),
            chunk: Some(request_timeout(timeouts.chunk_timeout)),
        })),
        http: Some(HttpOptions {
            encoding: provider::encoding(configured).map(request_encoding),
            tls: provider::tls(configured).map(client_tls),
            ..HttpOptions::default()
        }),
        ..RequestOptions::default()
    }
}

fn media_overlay(configured: &ProviderSettings) -> RequestOptions {
    let timeouts = provider::timeouts(configured);
    RequestOptions {
        timeout: Some(RequestTimeout::Config(TimeoutOptions {
            total: timeouts.timeout.map(request_timeout),
            headers: Some(request_timeout(timeouts.header_timeout)),
            chunk: Some(request_timeout(timeouts.chunk_timeout)),
        })),
        http: Some(HttpOptions { tls: provider::tls(configured).map(client_tls), ..HttpOptions::default() }),
        ..RequestOptions::default()
    }
}

fn media_runtime(
    info: &RuntimeInfo,
    configured: &ProviderSettings,
    runtime: RuntimeModel,
) -> Result<RuntimeModel, Error> {
    let runtime = adapt_decision(runtime, info.info.compatibility.as_ref());
    let overlay = media_overlay(configured);
    match runtime {
        RuntimeModel::Decision(mut model) => {
            let transport = merge_request_options(&[model.transport.as_ref(), Some(&overlay)]);
            model.transport = transport;
            Ok(RuntimeModel::Decision(model))
        }
        RuntimeModel::Image(mut model) => {
            let transport = merge_request_options(&[model.transport.as_ref(), Some(&overlay)]);
            model.transport = transport;
            Ok(RuntimeModel::Image(model))
        }
        RuntimeModel::Video(mut model) => {
            let transport = merge_request_options(&[model.transport.as_ref(), Some(&overlay)]);
            model.transport = transport;
            Ok(RuntimeModel::Video(model))
        }
        RuntimeModel::Language(_)
        | RuntimeModel::Embedding(_)
        | RuntimeModel::Speech(_)
        | RuntimeModel::Transcription(_)
        | RuntimeModel::Extraction(_) => Err(unsupported(&info.info)),
    }
}

fn adapt_decision(runtime: RuntimeModel, compatibility: Option<&ModelCompatibility>) -> RuntimeModel {
    match runtime {
        RuntimeModel::Decision(mut model) => {
            if let Some(media_input) = compatibility.and_then(|compatibility| compatibility.media_input.as_ref()) {
                let mut mapped = model
                    .compatibility
                    .take()
                    .and_then(|value| match value {
                        Value::Object(map) => Some(map),
                        _ => None,
                    })
                    .unwrap_or_default();
                mapped.insert("mediaInput".to_owned(), json_value(media_input));
                model.compatibility = Some(Value::Object(mapped));
            }
            RuntimeModel::Decision(model)
        }
        other => other,
    }
}

fn finalize(info: &RuntimeInfo, model: Model) -> Result<Model, Error> {
    if let Some(base_url) = model.route.endpoint.base_url.as_deref() {
        let variables = unresolved_variables(base_url);
        if !variables.is_empty() {
            return Err(Error::UnresolvedProviderVariables {
                provider_id: info.info.provider_id.clone(),
                model_id: info.info.id.clone(),
                variables,
            });
        }
    }
    if matches!(
        info.settings.as_ref().and_then(|settings| settings.compaction.as_ref()),
        Some(ProviderCompaction::Native(_))
    ) {
        return Err(Error::UnsupportedCompaction {
            provider_id: info.info.provider_id.clone(),
            model_id: info.info.id.clone(),
            route: model.route.id.clone(),
        });
    }
    Ok(model)
}

fn construction_error(info: &RuntimeInfo, specifier: &str, error: AiError) -> Error {
    let provider_id = info.info.provider_id.clone();
    let model_id = info.info.id.clone();
    match error.reason {
        AiErrorReason::InvalidRequest { message, .. } => {
            Error::ModelConfiguration { provider_id, model_id, package: specifier.to_owned(), detail: message }
        }
        reason => {
            let detail = reason.to_string().trim().to_owned();
            Error::ModelInitialization {
                provider_id,
                model_id,
                package: specifier.to_owned(),
                phase: Phase::Construct,
                detail: if detail.is_empty() { format!("construct failed for {specifier}") } else { detail },
            }
        }
    }
}

fn unsupported(info: &ModelInfo) -> Error {
    Error::UnsupportedPackage {
        provider_id: info.provider_id.clone(),
        model_id: info.id.clone(),
        package: info.package.clone().unwrap_or_else(|| "unknown".to_owned()),
    }
}

fn empty_provider_settings() -> ProviderSettings {
    ProviderSettings {
        timeout: None,
        header_timeout: None,
        chunk_timeout: None,
        compaction: None,
        cache: None,
        transport: None,
        encoding: None,
        tls: None,
        rest: codec::JsonMap::default(),
    }
}

fn setting<'a>(settings: &'a ProviderSettings, key: &str) -> Option<&'a Value> {
    settings.rest.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

fn set_setting(settings: &mut ProviderSettings, key: &str, value: Value) {
    match settings.rest.0.iter_mut().find(|(name, _)| name == key) {
        Some(entry) => entry.1 = value,
        None => settings.rest.0.push((key.to_owned(), value)),
    }
}

fn merge_provider_settings(base: Option<&ProviderSettings>, model: Option<&ModelSettings>) -> Option<ProviderSettings> {
    let base = base.map(provider::json_object);
    let overlay = model.map(provider::json_object);
    let merged = merge_overlay(base.as_ref(), overlay.as_ref())?;
    Some(provider::decode_object::<ProviderSettings>(merged))
}

fn provider_transport(provider: Option<&ProviderInfo>) -> Option<ProviderTransport> {
    let settings = provider.and_then(|provider| provider.settings.as_ref());
    if settings.is_some_and(|settings| provider::tls(settings).is_some()) {
        return Some(ProviderTransport::Http);
    }
    settings.and_then(|settings| settings.transport)
}

fn is_keyless(provider: Option<&ProviderInfo>, credential: Option<&CredentialValue>, info: &RuntimeInfo) -> bool {
    provider.is_some_and(|provider| provider.activation == ProviderInfoActivation::Enabled)
        && credential.is_none()
        && !has_configured_auth(info)
        && uses_api_key_auth(info.info.package.as_deref())
}

fn has_configured_auth(info: &RuntimeInfo) -> bool {
    let Some(settings) = info.settings.as_ref() else {
        return false;
    };
    ["apiKey", "gatewayApiKey", "authToken", "accessToken"]
        .iter()
        .any(|key| setting(settings, key).and_then(Value::as_str).is_some_and(|value| !value.is_empty()))
}

fn uses_api_key_auth(package: Option<&str>) -> bool {
    let name = package_name(package.unwrap_or(""));
    if name.is_empty() {
        return false;
    }
    API_KEY_AUTH_PACKAGES.contains(&name.as_str())
        || name.starts_with("@opencode/ai/providers/openai/")
        || name.starts_with("@opencode/ai/providers/azure/")
}

fn chunk_timeout(value: Option<&ProviderSettingsChunkTimeout>) -> Option<provider::HttpTimeout> {
    match value? {
        ProviderSettingsChunkTimeout::Number(number) => {
            (number.0 > 0.0).then_some(provider::HttpTimeout::Millis(number.0))
        }
        ProviderSettingsChunkTimeout::Bool(_) => Some(provider::HttpTimeout::Disabled),
    }
}

fn request_timeout(value: provider::HttpTimeout) -> HttpTimeout {
    match value {
        provider::HttpTimeout::Millis(value) => {
            HttpTimeout::Millis(serde_json::Number::from_f64(value).expect("a finite timeout in milliseconds"))
        }
        provider::HttpTimeout::Disabled => HttpTimeout::Disabled(false),
    }
}

fn request_encoding(value: ProviderEncoding) -> RequestEncoding {
    match value {
        ProviderEncoding::Zstd => RequestEncoding::Zstd,
        ProviderEncoding::Gzip => RequestEncoding::Gzip,
        ProviderEncoding::None => RequestEncoding::None,
    }
}

fn client_tls(value: ProviderTls) -> ClientTls {
    ClientTls { cert: value.cert, key: value.key, passphrase: value.passphrase }
}

fn json_value(value: &impl Encode) -> Value {
    serde_json::from_str(&encode_json(value)).expect("an encoded JSON value")
}
