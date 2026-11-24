use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::alibaba_chat::{self, AlibabaChat};
use crate::protocols::alibaba_messages::AlibabaMessages;
use crate::protocols::alibaba_responses::AlibabaResponses;
use crate::protocols::open_responses::PATH as RESPONSES_PATH;
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, LlmRequest};

pub const ID: &str = "alibaba";

const METADATA_KEY: &str = "alibaba";
const AUTH_ENV_VARS: [&str; 2] = ["DASHSCOPE_API_KEY", "ALIBABA_API_KEY"];
const DECLARED: [&str; 4] = ["apiKey", "baseURL", "region", "workspaceID"];
const CHAT_PATH: &str = "/chat/completions";
const MESSAGES_PATH: &str = "/messages";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "alibaba-chat".to_string(),
        protocol: Arc::new(AlibabaChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings, "/compatible-mode/v1")?, CHAT_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(
        model_id,
        ID,
        route,
        Some(alibaba_chat::compatibility()),
        Some(alibaba_chat::capabilities()),
    ))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "alibaba-messages".to_string(),
        protocol: Arc::new(AlibabaMessages),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings, "/apps/anthropic/v1")?, MESSAGES_PATH),
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
        id: "alibaba-responses".to_string(),
        protocol: Arc::new(AlibabaResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings, "/compatible-mode/v1")?, RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiEmbeddings),
        endpoint: MediaEndpoint::path(openai_embeddings::PATH)
            .with_base_url(base_url(settings, "/compatible-mode/v1")?),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

fn base_url(settings: &Settings, suffix: &str) -> Result<String, AiError> {
    if let Some(base_url) = settings.string("baseURL") {
        return Ok(base_url.to_string());
    }
    let Some(region) = settings.string("region") else {
        return Err(invalid_request("Alibaba requires region or baseURL"));
    };
    let host = match settings.string("workspaceID") {
        Some(workspace_id) => format!("{workspace_id}.{region}.maas.aliyuncs.com"),
        None => region_host(region)
            .map(str::to_string)
            .ok_or_else(|| invalid_request(format!("Alibaba region {region} requires workspaceID or baseURL")))?,
    };
    Ok(format!("https://{host}{suffix}"))
}

fn region_host(region: &str) -> Option<&'static str> {
    match region {
        "ap-southeast-1" => Some("dashscope-intl.aliyuncs.com"),
        "cn-beijing" => Some("dashscope.aliyuncs.com"),
        "cn-hongkong" => Some("cn-hongkong.dashscope.aliyuncs.com"),
        "us-east-1" => Some("dashscope-us.aliyuncs.com"),
        _ => None,
    }
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}

fn anthropic_version(_request: &LlmRequest) -> Vec<(String, String)> {
    vec![("anthropic-version".to_string(), "2023-06-01".to_string())]
}
