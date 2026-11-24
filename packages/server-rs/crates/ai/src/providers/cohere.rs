use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::cohere_chat::{self, CohereChat};
use crate::protocols::openai_chat::{self, OpenAiChat};
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::providers::{RuntimeModel, Settings, embedding_options, language_model, language_model_with_route};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::AiError;

pub const ID: &str = "cohere";

const COMPATIBILITY_BASE_URL: &str = "https://api.cohere.ai/compatibility/v1";
const CHAT_ROUTE_ID: &str = "cohere-chat-completions";
const METADATA_KEY: &str = "cohere";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(cohere_chat::DEFAULT_BASE_URL);
    Ok(language_model(
        model_id,
        ID,
        cohere_chat::ID,
        Some(METADATA_KEY),
        Arc::new(CohereChat),
        Endpoint::new(base_url, cohere_chat::PATH),
        auth(settings),
        settings.request_options(&DECLARED),
        None,
        None,
    ))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: CHAT_ROUTE_ID.to_string(),
        protocol: Arc::new(OpenAiChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(settings.string("baseURL").unwrap_or(COMPATIBILITY_BASE_URL), openai_chat::PATH),
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
        Some(json!({ "maxTokensField": "max_tokens", "reasoningField": "reasoning_content" })),
        Some(json!({ "tools": { "strict": false } })),
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = embedding_options(settings, &DECLARED);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiEmbeddings),
        endpoint: MediaEndpoint::path(openai_embeddings::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(COMPATIBILITY_BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["COHERE_API_KEY"], |credential| {
        credential.bearer()
    })
}
