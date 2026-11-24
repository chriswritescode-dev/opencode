use std::sync::Arc;

use serde_json::{Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::open_responses::{OpenResponses, PATH as RESPONSES_PATH};
use crate::protocols::openai_chat::{self, OpenAiChat, PATH as CHAT_PATH};
use crate::protocols::openai_embeddings::{self, OpenAiEmbeddings};
use crate::protocols::openai_images::{self, ImageInput, OpenAiImages, pixel_size};
use crate::protocols::system_one::{self, SystemOne};
use crate::protocols::utils::cache::CacheControl;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "digitalocean";

const BASE_URL: &str = "https://inference.do-ai.run/v1";
const CHAT_ID: &str = "digitalocean";
const RESPONSES_ID: &str = "digitalocean-responses";
const METADATA_KEY: &str = "digitalocean";
const AUTH_ENV_VARS: [&str; 3] = ["DIGITALOCEAN_ACCESS_TOKEN", "DIGITALOCEAN_API_KEY", "DO_INFERENCE_API_KEY"];
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub struct DigitalOceanChat;

impl Protocol for DigitalOceanChat {
    fn id(&self) -> &str {
        "digitalocean-chat"
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        openai_chat::from_request_with(&request.request, model, Some(&mut CacheControl::new()))
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        OpenAiChat.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        OpenAiChat.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        OpenAiChat.new_state(request, context)
    }
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: CHAT_ID.to_string(),
        protocol: Arc::new(DigitalOceanChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, CHAT_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, Some(json!({ "promptCacheKey": true }))))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let settings_options = settings.request_options(&DECLARED);
    let options = merge_request_options(&[Some(&base), settings_options.as_ref()]);
    let route = Route {
        id: RESPONSES_ID.to_string(),
        protocol: Arc::new(OpenResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options,
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
            .with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::function(Arc::new(|request: &crate::modalities::image::ImageRequest| {
        openai_images::path(ImageInput::Edits, request)
    }))
    .with_base_url(settings.string("baseURL").unwrap_or(BASE_URL));
    Ok(RuntimeModel::Image(ImageModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(OpenAiImages::new(ID, Arc::new(pixel_size), ImageInput::Edits)),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth: auth(settings),
        headers: Some(StringMap::from_pairs([("TypeSafe-Danger-Zone-Images".to_string(), "true".to_string())])),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS)
}

fn endpoint(settings: &Settings, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), path)
}
