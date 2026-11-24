use std::sync::Arc;

use opencode_api_types::js::encode_component;
use serde_json::Value;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::cloudflare_run_decisions::{self, CloudflareRunDecisions};
use crate::protocols::{anthropic_messages, openai_chat, openai_responses};
use crate::providers::cloudflare_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options, open_responses_options};
use crate::route::auth::{ApiKey, Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "cloudflare-ai-gateway";

const NAME: &str = "CloudflareAIGateway";
const RESPONSES_ID: &str = "cloudflare-ai-gateway-responses";
const MESSAGES_ID: &str = "cloudflare-ai-gateway-messages";
const CHAT_ID: &str = "cloudflare-ai-gateway-chat";
const AUTH_ENV_VARS: [&str; 2] = ["CLOUDFLARE_API_TOKEN", "CF_AIG_TOKEN"];
const DECLARED: [&str; 12] = [
    "accountId",
    "apiKey",
    "baseURL",
    "body",
    "cacheKey",
    "cacheTtl",
    "collectLog",
    "gatewayApiKey",
    "gatewayId",
    "headers",
    "metadata",
    "skipCache",
];

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    if model_id.starts_with("openai/") {
        return responses_model(model_id, settings, env);
    }
    if model_id.starts_with("anthropic/") {
        return messages_model(model_id, settings, env);
    }
    chat_model(model_id, settings, env)
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let native = is_native(settings);
    let base_url =
        if native { passthrough_base(settings, "openai")? } else { cloudflare_shared::base_url(settings, NAME)? };
    let gateway_id = gateway_id(settings);
    let base =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let route = Route {
        id: RESPONSES_ID.to_string(),
        protocol: Arc::new(openai_responses::OpenAiResponses),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(base_url, PathPart::Static(openai_responses::PATH.to_string())),
        auth: if native { passthrough_auth(settings) } else { rest_auth(settings) },
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: merged_options(settings, Some(&base), headers(settings, (!native).then_some(gateway_id.as_str()))),
    };
    let model_id = if native { model_id.strip_prefix("openai/").unwrap_or(model_id) } else { model_id };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let native = is_native(settings);
    let base_url =
        if native { passthrough_base(settings, "anthropic/v1")? } else { cloudflare_shared::base_url(settings, NAME)? };
    let gateway_id = gateway_id(settings);
    let route = Route {
        id: MESSAGES_ID.to_string(),
        protocol: Arc::new(anthropic_messages::AnthropicMessages::default()),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(base_url, PathPart::Default),
        auth: if native { passthrough_auth(settings) } else { rest_auth(settings) },
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: merged_options(settings, None, headers(settings, (!native).then_some(gateway_id.as_str()))),
    };
    let model_id = if native {
        model_id.strip_prefix("anthropic/").unwrap_or(model_id).replace('.', "-")
    } else {
        model_id.to_string()
    };
    Ok(language_model_with_route(&model_id, ID, route, None, None))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let gateway_id = gateway_id(settings);
    let route = Route {
        id: CHAT_ID.to_string(),
        protocol: Arc::new(openai_chat::OpenAiChat),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(
            cloudflare_shared::base_url(settings, NAME)?,
            PathPart::Static(openai_chat::PATH.to_string()),
        ),
        auth: rest_auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: merged_options(settings, None, headers(settings, Some(gateway_id.as_str()))),
    };
    Ok(language_model_with_route(model_id.strip_prefix("workers-ai/").unwrap_or(model_id), ID, route, None, None))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let transport = transport.map(|options| RequestOptions { headers: None, ..options });
    let gateway_id = gateway_id(settings);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: cloudflare_run_decisions::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(CloudflareRunDecisions),
        endpoint: MediaEndpoint::path("").with_base_url(cloudflare_shared::run_base_url(settings, NAME)?),
        auth: rest_auth(settings),
        headers: Some(headers(settings, Some(gateway_id.as_str()))),
        provider_metadata_key: Some(ID.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn is_native(settings: &Settings) -> bool {
    settings.string("baseURL").is_none()
}

fn gateway_id(settings: &Settings) -> String {
    settings.string("gatewayId").map(str::trim).filter(|value| !value.is_empty()).unwrap_or("default").to_string()
}

fn passthrough_base(settings: &Settings, provider_path: &str) -> Result<String, AiError> {
    let account_id = cloudflare_shared::require_account_id(settings, NAME)?;
    Ok(format!(
        "https://gateway.ai.cloudflare.com/v1/{}/{}/{provider_path}",
        encode_component(&account_id),
        encode_component(&gateway_id(settings))
    ))
}

fn rest_auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: gateway_credential(settings) }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}

fn passthrough_auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: gateway_credential(settings) }, &AUTH_ENV_VARS, |credential| {
        credential.bearer_header("cf-aig-authorization")
    })
}

fn gateway_credential(settings: &Settings) -> Option<ApiKey> {
    match settings.string("gatewayApiKey") {
        Some(value) => Some(ApiKey::Value(value.to_string())),
        None => settings.api_key(),
    }
}

fn headers(settings: &Settings, gateway_id: Option<&str>) -> StringMap {
    let mut headers = StringMap::new();
    if let Some(gateway_id) = gateway_id {
        headers.insert("cf-aig-gateway-id".to_string(), gateway_id.to_string());
    }
    if let Some(metadata) = settings.value("metadata").and_then(|metadata| serde_json::to_string(metadata).ok()) {
        headers.insert("cf-aig-metadata".to_string(), metadata);
    }
    if let Some(cache_ttl) = settings.value("cacheTtl") {
        headers.insert("cf-aig-cache-ttl".to_string(), header_value(cache_ttl));
    }
    if let Some(cache_key) = settings.value("cacheKey") {
        headers.insert("cf-aig-cache-key".to_string(), header_value(cache_key));
    }
    if let Some(skip_cache) = settings.value("skipCache") {
        headers.insert("cf-aig-skip-cache".to_string(), header_value(skip_cache));
    }
    if let Some(collect_log) = settings.value("collectLog") {
        headers.insert("cf-aig-collect-log".to_string(), header_value(collect_log));
    }
    if let Some(configured) = settings.headers() {
        for (key, value) in configured.iter() {
            headers.insert(key.to_string(), value.to_string());
        }
    }
    headers
}

fn header_value(value: &Value) -> String {
    value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())
}

fn merged_options(settings: &Settings, base: Option<&RequestOptions>, headers: StringMap) -> Option<RequestOptions> {
    let settings_options = settings.request_options(&DECLARED);
    let header_options = RequestOptions { headers: Some(headers), ..RequestOptions::default() };
    merge_request_options(&[base, settings_options.as_ref(), Some(&header_options)])
}

fn endpoint(base_url: String, path: PathPart) -> Endpoint {
    Endpoint { base_url: Some(base_url), path, query: StringMap::new() }
}
