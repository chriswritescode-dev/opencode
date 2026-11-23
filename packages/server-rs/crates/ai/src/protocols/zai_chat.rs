use std::sync::LazyLock;

use regex_lite::Regex;
use serde_json::{Map, Value, json};

use crate::ExecutionContext;
use crate::protocols::openai_chat::{self, OpenAiChat};
use crate::protocols::shared::invalid_request;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub struct ZaiChat;

const ID: &str = "zai-chat";

static TOOL_STREAMING_MODEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|/)glm-(\d+)(?:\.(\d+))?(?:-|$)").expect("zai tool streaming pattern"));

pub fn compatibility() -> Value {
    json!({ "maxTokensField": "max_tokens", "reasoningField": "reasoning_content", "zaiToolStream": false })
}

pub fn capabilities() -> Value {
    json!({ "tools": { "strict": false } })
}

impl Protocol for ZaiChat {
    fn id(&self) -> &str {
        ID
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
        if let Some(thinking) = options.thinking {
            object.insert("thinking".to_string(), Value::Object(thinking));
        }
        let tool_stream = match options.tool_stream {
            Some(tool_stream) => Some(tool_stream),
            None => {
                let has_tools = object.get("tools").and_then(Value::as_array).is_some_and(|tools| !tools.is_empty());
                (has_tools && supports_tool_streaming(&model.id)).then_some(true)
            }
        };
        if let Some(tool_stream) = tool_stream {
            object.insert("tool_stream".to_string(), Value::Bool(tool_stream));
        }
        if let Some(do_sample) = options.do_sample {
            object.insert("do_sample".to_string(), Value::Bool(do_sample));
        }
        if let Some(response_format) = options.response_format {
            object.insert("response_format".to_string(), response_format);
        }
        if let Some(request_id) = options.request_id {
            object.insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(user_id) = options.user_id {
            object.insert("user_id".to_string(), Value::String(user_id));
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
    thinking: Option<Map<String, Value>>,
    tool_stream: Option<bool>,
    do_sample: Option<bool>,
    response_format: Option<Value>,
    request_id: Option<String>,
    user_id: Option<String>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("reasoningEffort"), "reasoningEffort")?;
    Ok(Options {
        thinking: optional_thinking(input.get("thinking"))?,
        tool_stream: optional_bool(input.get("toolStream"), "toolStream")?,
        do_sample: optional_bool(input.get("doSample"), "doSample")?,
        response_format: optional_response_format(input.get("responseFormat"))?,
        request_id: optional_string(input.get("requestID"), "requestID")?,
        user_id: optional_string(input.get("userID"), "userID")?,
    })
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Map<String, Value>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let mut thinking = Map::new();
    match object.get("type") {
        None => {}
        Some(Value::String(kind)) => {
            thinking.insert("type".to_string(), Value::String(kind.clone()));
        }
        Some(_) => return Err(option_error("thinking.type")),
    }
    match object.get("clear_thinking") {
        None => {}
        Some(Value::Bool(value)) => {
            thinking.insert("clear_thinking".to_string(), Value::Bool(*value));
        }
        Some(_) => return Err(option_error("thinking.clear_thinking")),
    }
    Ok(Some(thinking))
}

fn optional_response_format(value: Option<&Value>) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("responseFormat"))?;
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(option_error("responseFormat.type"));
    };
    Ok(Some(json!({ "type": kind })))
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_bool(value: Option<&Value>, field: &str) -> Result<Option<bool>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("zai-chat providerOptions {field} is invalid"))
}

fn supports_tool_streaming(model_id: &str) -> bool {
    let Some(captures) = TOOL_STREAMING_MODEL.captures(model_id) else {
        return false;
    };
    let major = captures.get(1).and_then(|value| value.as_str().parse::<u32>().ok());
    let minor = captures.get(2).and_then(|value| value.as_str().parse::<u32>().ok()).unwrap_or(0);
    major.is_some_and(|major| major > 4 || (major == 4 && minor >= 6))
}
