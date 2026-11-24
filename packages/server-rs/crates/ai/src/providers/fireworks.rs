use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::openai_chat::OpenAiChat;
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "fireworks";
const BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
const PATH: &str = "/chat/completions";
const ROUTE_ID: &str = "fireworks-chat";
const METADATA_KEY: &str = "fireworks";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    Ok(language_model(
        model_id,
        ID,
        ROUTE_ID,
        Some(METADATA_KEY),
        Arc::new(OpenAiChat),
        Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), PATH),
        auth(settings),
        settings.request_options(&DECLARED),
        None,
        Some(json!({ "promptCacheKey": true })),
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiEmbeddings),
        endpoint: MediaEndpoint::path(openai_embeddings::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["FIREWORKS_API_KEY"], |credential| {
        credential.bearer()
    })
}
