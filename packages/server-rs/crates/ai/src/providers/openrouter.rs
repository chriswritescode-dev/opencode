use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::modalities::video::VideoModel;
use crate::protocols::open_responses::{self, OpenResponses, PATH as RESPONSES_PATH};
use crate::protocols::openai_chat::{self, AssistantTransform, OpenAiChat, PATH as CHAT_PATH};
use crate::protocols::openai_images::{ImageInput, OpenAiImages};
use crate::protocols::openrouter_video;
use crate::protocols::shared;
use crate::protocols::system_one::{self, SystemOne};
use crate::protocols::utils::cache::CacheControl;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, AssistantMessage, ContentPart, RequestOptions, merge_request_options};

pub const ID: &str = "openrouter";

const BASE_URL: &str = "https://openrouter.ai/api/v1";
const CHAT_ID: &str = "openrouter";
const CHAT_PROTOCOL_ID: &str = "openrouter-chat";
const RESPONSES_ID: &str = "openrouter-responses";
const METADATA_KEY: &str = "openrouter";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];
const REASONING_MINIMUM_BUDGET: i64 = 1024;
const KNOWN_OPTIONS: [&str; 8] =
    ["models", "provider", "plugins", "debug", "user", "usage", "web_search_options", "reasoning"];

pub struct OpenRouterChat;

impl Protocol for OpenRouterChat {
    fn id(&self) -> &str {
        CHAT_PROTOCOL_ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let mut cache_control = CacheControl::new();
        let mut assistant = |source: &AssistantMessage, lowered: Map<String, Value>| assistant_message(source, lowered);
        let body = openai_chat::from_request_with_assistant(
            &request.request,
            model,
            Some(&mut cache_control),
            Some(&mut assistant as &mut AssistantTransform<'_>),
        )?;
        Ok(body_options(
            body,
            request.request.provider_options.as_ref(),
            request.request.generation.max_tokens.as_ref(),
        ))
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

pub struct OpenRouterResponses;

impl Protocol for OpenRouterResponses {
    fn id(&self) -> &str {
        RESPONSES_ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let body = open_responses::from_request(&request.request, model)?;
        let Some(mut object) = body.as_object().cloned() else {
            return Ok(body);
        };
        push_shared_options(&mut object, request.request.provider_options.as_ref());
        Ok(Value::Object(object))
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        OpenResponses.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        OpenResponses.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        OpenResponses.new_state(request, context)
    }
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: CHAT_ID.to_string(),
        protocol: Arc::new(OpenRouterChat),
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
        protocol: Arc::new(OpenRouterResponses),
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

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL).to_string();
    let endpoint = MediaEndpoint::function(std::sync::Arc::new(|_request: &crate::modalities::image::ImageRequest| {
        crate::protocols::openai_images::PATH.to_string()
    }))
    .with_base_url(base_url);
    Ok(RuntimeModel::Image(ImageModel::streamed(
        model_id,
        ID.to_string(),
        std::sync::Arc::new(OpenAiImages::new(ID, std::sync::Arc::new(image_sizing), ImageInput::References)),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

pub fn video_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::path(openrouter_video::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(openrouter_video::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Video(VideoModel::queued(
        model_id,
        ID.to_string(),
        openrouter_video::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

fn image_sizing(request: &crate::modalities::image::ImageRequest) -> Option<Map<String, Value>> {
    let resolution = request.resolution.as_deref().map(|resolution| {
        let upper = resolution.to_ascii_uppercase();
        match upper.as_str() {
            "0.5K" => "512".to_string(),
            "0.75K" => "768".to_string(),
            _ => upper,
        }
    });
    let mut sizing = Map::new();
    if let Some(aspect) = &request.aspect {
        sizing.insert("aspect_ratio".to_string(), Value::String(aspect.clone()));
    }
    if let Some(resolution) =
        resolution.filter(|resolution| matches!(resolution.as_str(), "512" | "768" | "1K" | "1.5K" | "2K" | "4K"))
    {
        sizing.insert("resolution".to_string(), Value::String(resolution));
    }
    Some(sizing)
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, Some(json!({ "mediaInput": "state" })), provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["OPENROUTER_API_KEY"], |credential| {
        credential.bearer()
    })
}

fn endpoint(settings: &Settings, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), path)
}

fn assistant_message(source: &AssistantMessage, mut message: Map<String, Value>) -> Result<Value, AiError> {
    let reasoning = source
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Reasoning(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let details = message.get("reasoning_details").and_then(Value::as_array).cloned();
    message.remove("reasoning_content");
    message.remove("reasoning_text");
    if !reasoning.is_empty() && details.as_ref().is_some_and(|details| !details.is_empty()) {
        message.insert("reasoning".to_string(), Value::String(reasoning));
    } else {
        message.remove("reasoning");
    }
    match details {
        Some(details) => {
            message.insert("reasoning_details".to_string(), Value::Array(details));
        }
        None => {
            message.remove("reasoning_details");
        }
    }
    Ok(Value::Object(message))
}

fn body_options(body: Value, provider_options: Option<&Map<String, Value>>, max_tokens: Option<&Number>) -> Value {
    let empty = Map::new();
    let input = provider_options.unwrap_or(&empty);
    let body = body.as_object().cloned().unwrap_or_default();
    let mut result = Map::new();
    for (key, value) in input {
        if key == "promptCacheKey" || KNOWN_OPTIONS.contains(&key.as_str()) {
            continue;
        }
        result.insert(key.clone(), value.clone());
    }
    for (key, value) in &body {
        result.insert(key.clone(), value.clone());
    }
    match input.get("usage") {
        None | Some(Value::Bool(true)) => {
            let mut usage = Map::new();
            usage.insert("include".to_string(), Value::Bool(true));
            result.insert("usage".to_string(), Value::Object(usage));
        }
        Some(Value::Bool(false)) => {
            let mut usage = Map::new();
            usage.insert("include".to_string(), Value::Bool(false));
            result.insert("usage".to_string(), Value::Object(usage));
        }
        Some(Value::Object(usage)) => {
            result.insert("usage".to_string(), Value::Object(usage.clone()));
        }
        Some(_) => {}
    }
    push_shared_options(&mut result, Some(input));
    if let Some(web_search_options) = input.get("web_search_options").and_then(Value::as_object) {
        result.insert("web_search_options".to_string(), Value::Object(web_search_options.clone()));
    }
    if let Some(reasoning) = input.get("reasoning").and_then(Value::as_object) {
        result.insert("reasoning".to_string(), fit_reasoning(reasoning, max_tokens));
    }
    Value::Object(result)
}

fn push_shared_options(target: &mut Map<String, Value>, provider_options: Option<&Map<String, Value>>) {
    let Some(input) = provider_options else {
        return;
    };
    if let Some(models) = input.get("models").and_then(string_array) {
        target.insert("models".to_string(), models);
    }
    if let Some(provider) = input.get("provider").and_then(Value::as_object) {
        target.insert("provider".to_string(), Value::Object(provider.clone()));
    }
    if let Some(plugins) = input.get("plugins").and_then(object_array) {
        target.insert("plugins".to_string(), plugins);
    }
    if let Some(debug) = input.get("debug").and_then(Value::as_object) {
        target.insert("debug".to_string(), Value::Object(debug.clone()));
    }
    if let Some(user) = input.get("user").and_then(Value::as_str) {
        target.insert("user".to_string(), Value::String(user.to_string()));
    }
}

fn fit_reasoning(reasoning: &Map<String, Value>, max_tokens: Option<&Number>) -> Value {
    let Some(budget) = reasoning.get("max_tokens").and_then(Value::as_number) else {
        return Value::Object(reasoning.clone());
    };
    let mut fitted = reasoning.clone();
    fitted.insert(
        "max_tokens".to_string(),
        Value::Number(shared::fit_thinking_budget(budget, max_tokens, REASONING_MINIMUM_BUDGET)),
    );
    Value::Object(fitted)
}

fn string_array(value: &Value) -> Option<Value> {
    let items = value.as_array()?;
    items.iter().all(Value::is_string).then(|| Value::Array(items.clone()))
}

fn object_array(value: &Value) -> Option<Value> {
    let items = value.as_array()?;
    items.iter().all(Value::is_object).then(|| Value::Array(items.clone()))
}
