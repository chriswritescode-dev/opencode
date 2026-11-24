use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::anthropic_messages::PATH as MESSAGES_PATH;
use crate::protocols::meta_images;
use crate::protocols::meta_messages::MetaMessages;
use crate::protocols::meta_responses::MetaResponses;
use crate::protocols::open_responses::PATH as RESPONSES_PATH;
use crate::protocols::openai_chat::{OpenAiChat, PATH as CHAT_PATH};
use crate::providers::{RuntimeModel, Settings, language_model_with_route, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, merge_request_options};

pub const ID: &str = "meta";

const METADATA_KEY: &str = "meta";
const BASE_URL: &str = "https://api.meta.ai/v1";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "meta-responses".to_string(),
        protocol: Arc::new(MetaResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings), RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: responses_options(settings),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "meta-chat".to_string(),
        protocol: Arc::new(OpenAiChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings), CHAT_PATH),
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
        Some(json!({ "maxTokensField": "max_completion_tokens" })),
        Some(json!({ "promptCacheKey": true })),
    ))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "meta-messages".to_string(),
        protocol: Arc::new(MetaMessages),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings), MESSAGES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: messages_options(settings),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let transport = crate::providers::modality_options(settings, &DECLARED).0;
    let endpoint = MediaEndpoint::function(Arc::new(|request: &crate::modalities::image::ImageRequest| {
        meta_images::path(!request.images.is_empty())
    }))
    .with_base_url(base_url(settings));
    let route = ImageModel::inline(
        model_id,
        ID.to_string(),
        meta_images::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        None,
    );
    Ok(RuntimeModel::Image(route))
}

fn base_url(settings: &Settings) -> &str {
    settings.string("baseURL").unwrap_or(BASE_URL)
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["META_API_KEY"], |credential| {
        credential.bearer()
    })
}

fn responses_options(settings: &Settings) -> Option<RequestOptions> {
    with_provider_options(settings, open_responses_options::defaults())
}

fn messages_options(settings: &Settings) -> Option<RequestOptions> {
    let mut thinking = Map::new();
    thinking.insert("type".to_string(), Value::String("adaptive".to_string()));
    thinking.insert("display".to_string(), Value::String("omitted".to_string()));
    let mut options = Map::new();
    options.insert("thinking".to_string(), Value::Object(thinking));
    with_provider_options(settings, options)
}

fn with_provider_options(settings: &Settings, provider_options: Map<String, Value>) -> Option<RequestOptions> {
    let defaults = RequestOptions { provider_options: Some(provider_options), ..RequestOptions::default() };
    let settings_options = settings.request_options(&DECLARED);
    merge_request_options(&[Some(&defaults), settings_options.as_ref()])
}
