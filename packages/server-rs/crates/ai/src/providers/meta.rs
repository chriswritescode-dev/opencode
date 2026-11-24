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
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, ToolDefinition, merge_request_options};

pub const ID: &str = "meta";

const METADATA_KEY: &str = "meta";
const BASE_URL: &str = "https://api.meta.ai/v1";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

#[derive(Debug, Clone, Default)]
pub struct WebSearchOptions {
    pub search_context_size: Option<String>,
    pub user_location: Option<UserLocation>,
}

#[derive(Debug, Clone, Default)]
pub struct UserLocation {
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub timezone: Option<String>,
}

pub fn web_search(options: WebSearchOptions) -> ToolDefinition {
    let mut native = Map::new();
    native.insert("type".to_string(), json!("web_search"));
    insert_optional(&mut native, "search_context_size", options.search_context_size);
    if let Some(location) = options.user_location {
        native.insert("user_location".to_string(), approximate_location(location));
    }
    hosted_tool("web_search", "Search the web with Meta's hosted search tool.", native)
}

#[derive(Debug, Clone, Default)]
pub struct ImageGenerationOptions {
    pub size: Option<String>,
    pub output_format: Option<String>,
    pub reasoning_strength: Option<String>,
    pub enable_image_search: Option<bool>,
    pub enable_web_search: Option<bool>,
    pub enable_shell: Option<bool>,
}

pub fn image_generation(options: ImageGenerationOptions) -> ToolDefinition {
    let mut native = Map::new();
    native.insert("type".to_string(), json!("image_generation"));
    insert_optional(&mut native, "size", options.size);
    insert_optional(&mut native, "output_format", options.output_format);
    insert_optional(&mut native, "reasoning_strength", options.reasoning_strength);
    insert_optional(&mut native, "enable_image_search", options.enable_image_search);
    insert_optional(&mut native, "enable_web_search", options.enable_web_search);
    insert_optional(&mut native, "enable_shell", options.enable_shell);
    hosted_tool("image_generation", "Generate or edit an image with Muse Image.", native)
}

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
    bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["META_API_KEY"])
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

fn hosted_tool(name: &str, description: &str, native: Map<String, Value>) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        output_schema: None,
        freeform: None,
        cache: None,
        native: Some([("meta".to_string(), Value::Object(native))].into_iter().collect()),
    }
}

fn approximate_location(location: UserLocation) -> Value {
    let mut value = Map::new();
    value.insert("type".to_string(), json!("approximate"));
    insert_optional(&mut value, "city", location.city);
    insert_optional(&mut value, "region", location.region);
    insert_optional(&mut value, "country", location.country);
    insert_optional(&mut value, "timezone", location.timezone);
    Value::Object(value)
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value.into());
    }
}
