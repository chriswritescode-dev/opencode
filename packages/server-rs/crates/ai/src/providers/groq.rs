use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::protocols::openai_chat::{self, OpenAiChat};
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub const ID: &str = "groq";

const BASE_URL: &str = "https://api.groq.com/openai/v1";
const PATH: &str = "/chat/completions";
const ROUTE_ID: &str = "groq-chat";
const METADATA_KEY: &str = "groq";
const GPT_OSS_PREFIX: &str = "openai/gpt-oss-";

pub struct GroqChat;

impl Protocol for GroqChat {
    fn id(&self) -> &str {
        ROUTE_ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let options = decode_options(request.request.provider_options.as_ref())?;
        let mut body = openai_chat::from_request(&request.request, model)?;
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        if model.id.starts_with(GPT_OSS_PREFIX) {
            object.remove("reasoning_format");
            if let Some(include_reasoning) = options.include_reasoning {
                object.insert("include_reasoning".to_string(), Value::Bool(include_reasoning));
            }
        } else {
            object.insert("reasoning_format".to_string(), Value::String("parsed".to_string()));
        }
        if let Some(user) = options.user {
            object.insert("user".to_string(), Value::String(user));
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

struct Options {
    include_reasoning: Option<bool>,
    user: Option<String>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    Ok(Options {
        include_reasoning: optional_bool(input.get("includeReasoning"), "includeReasoning")?,
        user: optional_string(input.get("user"), "user")?,
    })
}

fn optional_bool(value: Option<&Value>, field: &str) -> Result<Option<bool>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("{ROUTE_ID} providerOptions {field} is invalid"))
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth = bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["GROQ_API_KEY"]);
    Ok(language_model(
        model_id,
        ID,
        ROUTE_ID,
        Some(METADATA_KEY),
        Arc::new(GroqChat),
        Endpoint::new(base_url, PATH),
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        Some(json!({
            "maxTokensField": "max_completion_tokens",
            "reasoningField": "reasoning",
            "requireReasoning": false
        })),
        Some(json!({ "tools": { "strict": false } })),
    ))
}
