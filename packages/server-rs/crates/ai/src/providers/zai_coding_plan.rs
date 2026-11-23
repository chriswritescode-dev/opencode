use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::protocols::open_responses::{self, OpenResponses};
use crate::protocols::zai_chat::{self, ZaiChat};
use crate::protocols::zai_messages::ZaiMessages;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, LlmRequest};

pub const ID: &str = "zai-coding-plan";

const CHAT_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const MESSAGES_BASE_URL: &str = "https://api.z.ai/api/anthropic/v1";
const RESPONSES_BASE_URL: &str = "https://api.z.ai/api/v1";
const METADATA_KEY: &str = "zai";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "zai-coding-chat".to_string(),
        protocol: Arc::new(ZaiChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, CHAT_BASE_URL, "/chat/completions"),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, Some(zai_chat::compatibility()), Some(zai_chat::capabilities())))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "zai-coding-messages".to_string(),
        protocol: Arc::new(ZaiMessages),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, MESSAGES_BASE_URL, "/messages"),
        auth: auth(settings),
        headers: Some(Arc::new(anthropic_version)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, Some(json!({ "requireSignature": false })), None))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "zai-coding-responses".to_string(),
        protocol: Arc::new(OpenResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, RESPONSES_BASE_URL, open_responses::PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["ZAI_API_KEY"], |credential| {
        credential.bearer()
    })
}

fn endpoint(settings: &Settings, base_url: &str, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(base_url), path)
}

fn anthropic_version(_request: &LlmRequest) -> Vec<(String, String)> {
    vec![("anthropic-version".to_string(), "2023-06-01".to_string())]
}
