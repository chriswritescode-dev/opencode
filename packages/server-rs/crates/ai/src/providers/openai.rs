use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::modalities::speech::SpeechModel;
use crate::modalities::transcription::TranscriptionModel;
use crate::protocols::openai_chat;
use crate::protocols::openai_decisions::{self, OpenAiDecisions};
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::protocols::openai_images::{self, ImageInput, OpenAiImages, pixel_size};
use crate::protocols::openai_responses;
use crate::protocols::openai_speech;
use crate::protocols::openai_transcription;
use crate::providers::open_responses_options;
use crate::providers::openai_options;
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, ProviderOptions, RequestOptions, StringMap, ToolDefinition, merge_request_options};

const ID: &str = "openai";
const DECLARED: [&str; 5] = ["apiKey", "baseURL", "organization", "project", "queryParams"];

#[derive(Debug, Clone, Default)]
pub struct ImageGenerationOptions {
    pub action: Option<String>,
    pub background: Option<String>,
    pub input_fidelity: Option<String>,
    pub output_compression: Option<u64>,
    pub output_format: Option<String>,
    pub partial_images: Option<u64>,
    pub quality: Option<String>,
    pub size: Option<String>,
}

pub fn image_generation(options: ImageGenerationOptions) -> ToolDefinition {
    let mut native = Map::new();
    native.insert("type".to_string(), json!("image_generation"));
    insert_optional(&mut native, "action", options.action);
    insert_optional(&mut native, "background", options.background);
    insert_optional(&mut native, "input_fidelity", options.input_fidelity);
    insert_optional(&mut native, "output_compression", options.output_compression);
    insert_optional(&mut native, "output_format", options.output_format);
    insert_optional(&mut native, "partial_images", options.partial_images);
    insert_optional(&mut native, "quality", options.quality);
    insert_optional(&mut native, "size", options.size);
    hosted_tool(
        "image_generation",
        "Generate or edit an image using OpenAI's hosted image generation tool.",
        json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        native,
    )
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let defaults = with_provider_options(open_responses_options::defaults());
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = request_options(settings);
    let options = merge_request_options(&[Some(&defaults), model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model(
        model_id,
        ID,
        "openai-responses",
        Some("openai"),
        Arc::new(openai_responses::OpenAiResponses),
        endpoint(
            settings.string("baseURL").unwrap_or(openai_responses::DEFAULT_BASE_URL),
            openai_responses::PATH,
            settings.query_params().unwrap_or_default(),
        ),
        auth(settings),
        options,
        None,
        None,
    ))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = request_options(settings);
    let options = merge_request_options(&[model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model(
        model_id,
        ID,
        "openai-chat",
        Some("openai"),
        Arc::new(openai_chat::OpenAiChat),
        endpoint(
            settings.string("baseURL").unwrap_or(openai_chat::DEFAULT_BASE_URL),
            openai_chat::PATH,
            settings.query_params().unwrap_or_default(),
        ),
        auth(settings),
        options,
        None,
        Some(json!({ "promptCacheKey": true })),
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let mut transport = transport.unwrap_or_default();
    let mut headers = StringMap::new();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(query) = settings.query_params() {
        let mut http = transport.http.clone().unwrap_or_default();
        let mut merged = http.query.clone().unwrap_or_default();
        for (key, value) in query.iter() {
            merged.insert(key.to_string(), value.to_string());
        }
        http.query = Some(merged);
        transport.http = Some(http);
    }
    let transport = (transport != RequestOptions::default()).then_some(transport);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiEmbeddings),
        endpoint: MediaEndpoint::path(openai_embeddings::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(openai_embeddings::DEFAULT_BASE_URL)),
        auth: auth(settings),
        headers: (!headers.is_empty()).then_some(headers),
        provider_metadata_key: None,
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let declared = ["apiKey", "baseURL", "organization", "project", "queryParams"];
    let (transport, provider_options) = modality_options(settings, &declared);
    let mut transport = transport.unwrap_or_default();
    let mut headers = transport.headers.take().unwrap_or_default();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(existing) = settings.headers() {
        for (name, value) in existing.iter() {
            headers.insert(name.to_string(), value.to_string());
        }
    }
    transport.headers = (!headers.is_empty()).then_some(headers);
    if let Some(query) = settings.query_params() {
        let mut http = transport.http.clone().unwrap_or_default();
        let mut merged = http.query.clone().unwrap_or_default();
        for (key, value) in query.iter() {
            merged.insert(key.to_string(), value.to_string());
        }
        http.query = Some(merged);
        transport.http = Some(http);
    }
    let transport = (transport != RequestOptions::default()).then_some(transport);
    let base_url = settings.string("baseURL").unwrap_or(openai_images::DEFAULT_BASE_URL).to_string();
    let image_input = if settings.string("baseURL").is_some_and(|base| base.starts_with("https://chatgpt.com/")) {
        ImageInput::JsonEdits
    } else {
        ImageInput::Edits
    };
    let endpoint =
        MediaEndpoint::function(std::sync::Arc::new(move |request: &crate::modalities::image::ImageRequest| {
            openai_images::path(image_input, request)
        }))
        .with_base_url(base_url);
    Ok(RuntimeModel::Image(ImageModel::streamed(
        model_id,
        ID.to_string(),
        std::sync::Arc::new(OpenAiImages::new(ID, std::sync::Arc::new(pixel_size), image_input)),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn speech_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let declared = ["apiKey", "baseURL", "organization", "project", "queryParams"];
    let (transport, provider_options) = modality_options(settings, &declared);
    let mut transport = transport.unwrap_or_default();
    let mut headers = transport.headers.take().unwrap_or_default();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(existing) = settings.headers() {
        for (name, value) in existing.iter() {
            headers.insert(name.to_string(), value.to_string());
        }
    }
    transport.headers = (!headers.is_empty()).then_some(headers);
    if let Some(query) = settings.query_params() {
        let mut http = transport.http.clone().unwrap_or_default();
        let mut merged = http.query.clone().unwrap_or_default();
        for (key, value) in query.iter() {
            merged.insert(key.to_string(), value.to_string());
        }
        http.query = Some(merged);
        transport.http = Some(http);
    }
    let transport = (transport != RequestOptions::default()).then_some(transport);
    let endpoint = MediaEndpoint::path(openai_speech::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(openai_speech::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Speech(SpeechModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(openai_speech::protocol()),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn transcription_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = media_options(settings);
    let endpoint = MediaEndpoint::path(openai_transcription::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(openai_transcription::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Transcription(TranscriptionModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(openai_transcription::protocol()),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = media_options(settings);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: openai_decisions::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(OpenAiDecisions),
        endpoint: MediaEndpoint::path(openai_decisions::PATH)
            .with_base_url(settings.string("baseURL").unwrap_or(openai_decisions::DEFAULT_BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some("openai".to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &["OPENAI_API_KEY"])
}

fn endpoint(base_url: &str, path: &str, query: StringMap) -> Endpoint {
    Endpoint { base_url: Some(base_url.to_string()), path: PathPart::Static(path.to_string()), query }
}

fn with_provider_options(options: Map<String, Value>) -> RequestOptions {
    RequestOptions { provider_options: Some(options), ..RequestOptions::default() }
}

fn request_options(settings: &Settings) -> Option<RequestOptions> {
    let options = settings.request_options(&DECLARED);
    let mut headers = StringMap::new();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(existing) = options.as_ref().and_then(|options| options.headers.as_ref()) {
        for (name, value) in existing.iter() {
            headers.insert(name.to_string(), value.to_string());
        }
    }
    let merged = RequestOptions { headers: (!headers.is_empty()).then_some(headers), ..options.unwrap_or_default() };
    (merged != RequestOptions::default()).then_some(merged)
}

fn media_options(settings: &Settings) -> (Option<RequestOptions>, Option<ProviderOptions>) {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let mut transport = transport.unwrap_or_default();
    let mut headers = transport.headers.take().unwrap_or_default();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(existing) = settings.headers() {
        for (name, value) in existing.iter() {
            headers.insert(name.to_string(), value.to_string());
        }
    }
    if let Some(query) = settings.query_params() {
        let mut http = transport.http.take().unwrap_or_default();
        let mut merged = http.query.take().unwrap_or_default();
        for (key, value) in query.iter() {
            merged.insert(key.to_string(), value.to_string());
        }
        http.query = Some(merged);
        transport.http = Some(http);
    }
    transport.headers = (!headers.is_empty()).then_some(headers);
    ((transport != RequestOptions::default()).then_some(transport), provider_options)
}

fn hosted_tool(name: &str, description: &str, input_schema: Value, native: Map<String, Value>) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        output_schema: None,
        freeform: None,
        cache: None,
        native: Some([("openai".to_string(), Value::Object(native))].into_iter().collect()),
    }
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value.into());
    }
}
