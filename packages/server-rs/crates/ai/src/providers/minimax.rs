use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::modalities::video::VideoModel;
use crate::protocols::anthropic_messages::{AnthropicMessagesPlain, PATH as MESSAGES_PATH};
use crate::protocols::minimax_images;
use crate::protocols::minimax_video;
use crate::protocols::open_responses::{OpenResponses, PATH as RESPONSES_PATH};
use crate::protocols::openai_chat::{self, OpenAiChat, PATH as CHAT_PATH};
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, LlmRequest};

pub const ID: &str = "minimax";

const MESSAGES_BASE_URL: &str = "https://api.minimax.io/anthropic/v1";
const CHAT_BASE_URL: &str = "https://api.minimax.io/v1";
const RESPONSES_BASE_URL: &str = "https://api.minimax.io/v1";
const METADATA_KEY: &str = "minimax";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub struct MiniMaxChat;

impl Protocol for MiniMaxChat {
    fn id(&self) -> &str {
        "minimax-chat"
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let options = decode_options(request.request.provider_options.as_ref())?;
        let mut body = openai_chat::from_request(&request.request, model)?;
        if let Some(object) = body.as_object_mut() {
            if let Some(thinking) = options.thinking {
                object.insert("thinking".to_string(), Value::Object(thinking));
            }
            object.insert("reasoning_split".to_string(), Value::Bool(options.reasoning_split.unwrap_or(true)));
        }
        Ok(body)
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
        id: "minimax-messages".to_string(),
        protocol: Arc::new(AnthropicMessagesPlain::default()),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, MESSAGES_BASE_URL, MESSAGES_PATH),
        auth: auth(settings),
        headers: Some(Arc::new(anthropic_version)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, Some(json!({ "requireSignature": false })), None))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "minimax-chat".to_string(),
        protocol: Arc::new(MiniMaxChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, CHAT_BASE_URL, CHAT_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, Some(json!({ "tools": { "strict": false } }))))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "minimax-responses".to_string(),
        protocol: Arc::new(OpenResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, RESPONSES_BASE_URL, RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = crate::providers::modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::path(minimax_images::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(minimax_images::DEFAULT_BASE_URL));
    let route = ImageModel::inline(
        model_id,
        ID.to_string(),
        minimax_images::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    );
    Ok(RuntimeModel::Image(route))
}

pub fn video_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = crate::providers::modality_options(settings, &DECLARED);
    let base_url =
        settings.string("baseURL").map(video_base_url).unwrap_or_else(|| minimax_video::DEFAULT_BASE_URL.to_string());
    let hailuo = minimax_video::is_hailuo(model_id);
    let endpoint = MediaEndpoint::path(if hailuo { minimax_video::HAILUO_PATH } else { minimax_video::PATH })
        .with_base_url(base_url);
    let protocol = if hailuo { minimax_video::hailuo_protocol() } else { minimax_video::h3_protocol() };
    Ok(RuntimeModel::Video(VideoModel::queued(
        model_id,
        ID.to_string(),
        protocol,
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

fn video_base_url(base_url: &str) -> String {
    let mut value = base_url.trim_end_matches('/');
    if let Some(rest) = value.strip_suffix("/v1") {
        value = rest;
    }
    value.trim_end_matches('/').to_string()
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["MINIMAX_API_KEY"])
}

fn endpoint(settings: &Settings, base_url: &str, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(base_url), path)
}

fn anthropic_version(_request: &LlmRequest) -> Vec<(String, String)> {
    vec![("anthropic-version".to_string(), "2023-06-01".to_string())]
}

struct Options {
    thinking: Option<Map<String, Value>>,
    reasoning_split: Option<bool>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    Ok(Options {
        thinking: optional_thinking(input.get("thinking"))?,
        reasoning_split: optional_bool(input.get("reasoningSplit"), "reasoningSplit")?,
    })
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Map<String, Value>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let mut thinking = Map::new();
    match object.get("type") {
        Some(Value::String(kind)) => {
            thinking.insert("type".to_string(), Value::String(kind.clone()));
        }
        _ => return Err(option_error("thinking.type")),
    }
    Ok(Some(thinking))
}

fn optional_bool(value: Option<&Value>, field: &str) -> Result<Option<bool>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("minimax-chat providerOptions {field} is invalid"))
}
