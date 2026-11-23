use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::protocols::anthropic_messages::{AnthropicMessagesPlain, PATH as MESSAGES_PATH};
use crate::protocols::open_responses::{OpenResponses, PATH as RESPONSES_PATH};
use crate::protocols::openai_chat::{self, OpenAiChat, PATH as CHAT_PATH};
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub const ID: &str = "moonshotai";

const CHAT_BASE_URL: &str = "https://api.moonshot.ai/v1";
const MESSAGES_BASE_URL: &str = "https://api.moonshot.ai/anthropic/v1";
const RESPONSES_BASE_URL: &str = "https://api.moonshot.ai/v1";
const METADATA_KEY: &str = "moonshot";
const AUTH_ENV_VARS: [&str; 2] = ["MOONSHOT_API_KEY", "MOONSHOTAI_API_KEY"];
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub struct MoonshotChat;

impl Protocol for MoonshotChat {
    fn id(&self) -> &str {
        "moonshot-chat"
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let options = decode_options(request.request.provider_options.as_ref())?;
        let mut body = openai_chat::from_request(&request.request, model)?;
        if let (Some(thinking), Some(object)) = (options.thinking, body.as_object_mut()) {
            object.insert("thinking".to_string(), Value::Object(thinking));
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
        id: "moonshot-chat".to_string(),
        protocol: Arc::new(MoonshotChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, CHAT_BASE_URL, CHAT_PATH),
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
        Some(json!({
            "maxTokensField": "max_tokens",
            "sanitizer": "moonshot",
            "reasoningField": "reasoning_content"
        })),
        Some(json!({ "tools": { "strict": false }, "promptCacheKey": true })),
    ))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "moonshot-messages".to_string(),
        protocol: Arc::new(AnthropicMessagesPlain::default()),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, MESSAGES_BASE_URL, MESSAGES_PATH),
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
        Some(json!({ "requireSignature": false, "sanitizer": "moonshot" })),
        None,
    ))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "moonshot-responses".to_string(),
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
    Ok(language_model_with_route(model_id, ID, route, Some(json!({ "sanitizer": "moonshot" })), None))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}

fn endpoint(settings: &Settings, base_url: &str, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(base_url), path)
}

struct Options {
    thinking: Option<Map<String, Value>>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("reasoningEffort"), "reasoningEffort")?;
    Ok(Options { thinking: optional_thinking(input.get("thinking"))? })
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
    match object.get("keep") {
        None => {}
        Some(Value::Null) => {
            thinking.insert("keep".to_string(), Value::Null);
        }
        Some(Value::String(keep)) => {
            thinking.insert("keep".to_string(), Value::String(keep.clone()));
        }
        Some(_) => return Err(option_error("thinking.keep")),
    }
    Ok(Some(thinking))
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("moonshot-chat providerOptions {field} is invalid"))
}
