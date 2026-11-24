use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::embedding::EmbeddingRequest;
use crate::protocols::gemini::{DEFAULT_BASE_URL, Gemini};
use crate::protocols::google_decisions::{self, GoogleDecisions};
use crate::protocols::google_embed_content::{self, GoogleEmbedContent};
use crate::protocols::google_interactions::{self, GoogleInteractions};
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "google";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    Ok(language_model(
        model_id,
        ID,
        "gemini",
        Some("google"),
        Arc::new(Gemini),
        Endpoint {
            base_url: Some(settings.string("baseURL").unwrap_or(DEFAULT_BASE_URL).to_string()),
            path: PathPart::Default,
            query: StringMap::new(),
        },
        auth(settings),
        settings.request_options(&["apiKey", "baseURL"]),
        None,
        None,
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::function(Arc::new(|request: &EmbeddingRequest| {
        format!("/models/{}:batchEmbedContents", request.model.id)
    }))
    .with_base_url(settings.string("baseURL").unwrap_or(google_embed_content::DEFAULT_BASE_URL));
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: google_embed_content::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(GoogleEmbedContent),
        endpoint,
        auth: auth(settings),
        headers: None,
        provider_metadata_key: None,
        options: transport,
        max_values: Some(google_embed_content::MAX_BATCH),
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

pub fn interactions_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    Ok(language_model(
        model_id,
        ID,
        google_interactions::ID,
        Some("google"),
        Arc::new(GoogleInteractions),
        Endpoint {
            base_url: Some(settings.string("baseURL").unwrap_or(google_interactions::DEFAULT_BASE_URL).to_string()),
            path: PathPart::Static(google_interactions::PATH.to_string()),
            query: StringMap::new(),
        },
        auth(settings),
        settings.request_options(&["apiKey", "baseURL"]),
        None,
        None,
    ))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: google_decisions::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(GoogleDecisions),
        endpoint: MediaEndpoint::path(google_decisions::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(google_decisions::DEFAULT_BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some("google".to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(
        ProviderAuthOptions { auth: None, api_key: settings.api_key() },
        &["GOOGLE_GENERATIVE_AI_API_KEY"],
        |credential| credential.header("x-goog-api-key"),
    )
}
