use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::openai_decisions::{self, OpenAiDecisions};
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::gateway_protocol::{GatewayProtocol, Prepare, PreparedBody};
use crate::protocols::{anthropic_messages, open_responses, openai_chat};
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, LlmRequest, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "vercel-ai-gateway";

const BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
const MESSAGES_ID: &str = "vercel-ai-gateway-messages";
const RESPONSES_ID: &str = "vercel-ai-gateway-responses";
const CHAT_ID: &str = "vercel-ai-gateway-chat";
const AUTH_ENV_VARS: [&str; 2] = ["AI_GATEWAY_API_KEY", "VERCEL_OIDC_TOKEN"];
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    if model_id.starts_with("openai/gpt-") || model_id.starts_with("spacexai/grok-") {
        return responses_model(model_id, settings, env);
    }
    if model_id.starts_with("meta/muse-") {
        return chat_model(model_id, settings, env);
    }
    messages_model(model_id, settings, env)
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base = RequestOptions {
        headers: Some(StringMap::from_pairs([("anthropic-version".to_string(), "2023-06-01".to_string())])),
        ..RequestOptions::default()
    };
    let settings_options = settings.request_options(&DECLARED);
    let options = merge_request_options(&[Some(&base), settings_options.as_ref()]);
    let route = Route {
        id: MESSAGES_ID.to_string(),
        protocol: Arc::new(GatewayProtocol::new(
            MESSAGES_ID,
            Arc::new(anthropic_messages::AnthropicMessages::default()),
            prepare(Api::Messages),
        )),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(settings, anthropic_messages::PATH),
        auth: auth(settings),
        headers: Some(Arc::new(session_affinity)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options,
    };
    Ok(language_model_with_route(
        model_id,
        ID,
        route,
        Some(json!({ "requireSignature": model_id.starts_with("anthropic/") })),
        None,
    ))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let settings_options = settings.request_options(&DECLARED);
    let options = merge_request_options(&[Some(&base), settings_options.as_ref()]);
    let route = Route {
        id: RESPONSES_ID.to_string(),
        protocol: Arc::new(GatewayProtocol::new(
            RESPONSES_ID,
            Arc::new(open_responses::OpenResponses),
            prepare(Api::Responses),
        )),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(settings, open_responses::PATH),
        auth: auth(settings),
        headers: Some(Arc::new(session_affinity)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options,
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: CHAT_ID.to_string(),
        protocol: Arc::new(GatewayProtocol::new(CHAT_ID, Arc::new(openai_chat::OpenAiChat), prepare(Api::Chat))),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: endpoint(settings, openai_chat::PATH),
        auth: auth(settings),
        headers: Some(Arc::new(session_affinity)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, Some(json!({ "reasoningField": "reasoning" })), None))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: openai_decisions::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiDecisions),
        endpoint: MediaEndpoint::path(openai_decisions::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(ID.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}

fn endpoint(settings: &Settings, path: &str) -> Endpoint {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL).to_string();
    Endpoint { base_url: Some(base_url), path: PathPart::Static(path.to_string()), query: StringMap::new() }
}

fn session_affinity(request: &LlmRequest) -> Vec<(String, String)> {
    match request.prompt_cache_key.as_ref() {
        Some(key) => vec![("x-session-affinity".to_string(), key.clone())],
        None => Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum Api {
    Messages,
    Responses,
    Chat,
}

struct DecodedOptions {
    gateway: Option<Map<String, Value>>,
    upstream: Option<Map<String, Value>>,
    reasoning_effort: Option<String>,
    thinking: Option<ThinkingOption>,
    cache_ttl: Option<String>,
    cache_anchor_items: Option<Number>,
}

struct ThinkingOption {
    kind: String,
    budget_tokens: Option<Number>,
}

fn prepare(api: Api) -> Prepare {
    Arc::new(move |request| prepare_request(api, request))
}

fn prepare_request(api: Api, request: &LlmRequest) -> Result<PreparedBody, AiError> {
    let options = decode_options(request)?;
    let mut body = Map::new();
    if let Some(provider_options) = provider_options_body(&options) {
        body.insert("providerOptions".to_string(), provider_options);
    }
    match api {
        Api::Messages => messages_prepared(request, options, body),
        Api::Responses => responses_prepared(request, options, body),
        Api::Chat => chat_prepared(request, options, body),
    }
}

fn messages_prepared(
    request: &LlmRequest,
    options: DecodedOptions,
    body: Map<String, Value>,
) -> Result<PreparedBody, AiError> {
    let Some(effort) = options.reasoning_effort else {
        return Ok(PreparedBody { request: request.clone(), body });
    };
    let enabled = effort != "none";
    let thinking = request
        .provider_options
        .as_ref()
        .and_then(|provider| provider.get("thinking"))
        .cloned()
        .unwrap_or_else(|| json!({ "type": if enabled { "adaptive" } else { "disabled" } }));
    let mut next = request.clone();
    let provider_options = next.provider_options.get_or_insert_with(Map::new);
    provider_options.insert("thinking".to_string(), thinking);
    if enabled {
        provider_options.insert("effort".to_string(), Value::String(effort));
    } else {
        provider_options.remove("effort");
    }
    Ok(PreparedBody { request: next, body })
}

fn responses_prepared(
    request: &LlmRequest,
    options: DecodedOptions,
    mut body: Map<String, Value>,
) -> Result<PreparedBody, AiError> {
    if let Some(cache_ttl) = options.cache_ttl {
        body.insert("cache_ttl".to_string(), Value::String(cache_ttl));
    }
    if let Some(cache_anchor_items) = options.cache_anchor_items {
        body.insert("cache_anchor_items".to_string(), Value::Number(cache_anchor_items));
    }
    Ok(PreparedBody { request: request.clone(), body })
}

fn chat_prepared(
    request: &LlmRequest,
    options: DecodedOptions,
    mut body: Map<String, Value>,
) -> Result<PreparedBody, AiError> {
    if let Some(thinking) = options.thinking {
        let mut reasoning = Map::new();
        reasoning.insert("enabled".to_string(), Value::Bool(thinking.kind != "disabled"));
        if let Some(budget_tokens) = thinking.budget_tokens {
            reasoning.insert("max_tokens".to_string(), Value::Number(budget_tokens));
        }
        body.insert("reasoning".to_string(), Value::Object(reasoning));
    }
    Ok(PreparedBody { request: request.clone(), body })
}

fn provider_options_body(options: &DecodedOptions) -> Option<Value> {
    if options.upstream.is_none() && options.gateway.is_none() {
        return None;
    }
    let Some(gateway) = options.gateway.as_ref() else {
        return options.upstream.clone().map(Value::Object);
    };
    let mut provider = options.upstream.clone().unwrap_or_default();
    provider.insert("gateway".to_string(), Value::Object(gateway.clone()));
    Some(Value::Object(provider))
}

fn decode_options(request: &LlmRequest) -> Result<DecodedOptions, AiError> {
    let options = request.provider_options.clone().unwrap_or_default();
    Ok(DecodedOptions {
        gateway: optional_object(options.get("gateway"), "gateway")?,
        upstream: optional_object(options.get("upstream"), "upstream")?,
        reasoning_effort: optional_string(options.get("reasoningEffort"), "reasoningEffort")?,
        thinking: optional_thinking(options.get("thinking"))?,
        cache_ttl: optional_string(options.get("cacheTTL"), "cacheTTL")?,
        cache_anchor_items: optional_number(options.get("cacheAnchorItems"), "cacheAnchorItems")?,
    })
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<ThinkingOption>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let kind = match object.get("type") {
        Some(Value::String(kind)) => kind.clone(),
        _ => return Err(option_error("thinking.type")),
    };
    let budget_tokens = optional_number(object.get("budgetTokens"), "thinking.budgetTokens")?;
    let legacy_budget = optional_number(object.get("budget_tokens"), "thinking.budget_tokens")?;
    Ok(Some(ThinkingOption { kind, budget_tokens: budget_tokens.or(legacy_budget) }))
}

fn optional_object(value: Option<&Value>, field: &str) -> Result<Option<Map<String, Value>>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_number(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("vercel-ai-gateway providerOptions {field} is invalid"))
}
