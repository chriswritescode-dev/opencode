use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::openai_chat::OpenAiChat;
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::protocols::openai_images::{self, ImageInput, OpenAiImages, pixel_size};
use crate::protocols::system_one::{self, SystemOne};
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, AiErrorReason, ProviderOptions, StringMap, merge_provider_options};

const ID: &str = "openai-compatible";
const ROUTE_ID: &str = "openai-compatible-chat";
const PATH: &str = "/chat/completions";
const DECLARED: [&str; 3] = ["apiKey", "baseURL", "provider"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    Ok(language_model(
        model_id,
        provider,
        ROUTE_ID,
        Some(provider),
        Arc::new(OpenAiChat),
        Endpoint {
            base_url: Some(base_url.to_string()),
            path: PathPart::Static(PATH.to_string()),
            query: StringMap::new(),
        },
        auth(settings),
        settings.request_options(&DECLARED),
        None,
        None,
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: provider.to_string(),
        protocol: Arc::new(OpenAiEmbeddings),
        endpoint: MediaEndpoint::path(openai_embeddings::PATH).with_base_url(base_url),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(provider.to_string()),
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let defaults = if model_id.contains("gpt-image") {
        None
    } else {
        let mut defaults = ProviderOptions::new();
        defaults.insert("response_format".to_string(), json!("b64_json"));
        Some(defaults)
    };
    let provider_options = merge_provider_options(&[defaults.as_ref(), provider_options.as_ref()]);
    let endpoint = MediaEndpoint::function(Arc::new(|request: &crate::modalities::image::ImageRequest| {
        openai_images::path(ImageInput::Edits, request)
    }))
    .with_base_url(base_url);
    Ok(RuntimeModel::Image(ImageModel::streamed(
        model_id,
        provider.to_string(),
        Arc::new(OpenAiImages::new(provider, Arc::new(pixel_size), ImageInput::Edits)),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: provider.to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(base_url),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(provider.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, Some(json!({ "mediaInput": "root" })), provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &[])
}

fn missing_base_url() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "OpenAI-compatible providers require a baseURL".to_string(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}
