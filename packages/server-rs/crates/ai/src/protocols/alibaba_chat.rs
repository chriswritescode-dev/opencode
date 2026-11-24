use serde_json::{Map, Number, Value, json};

use crate::ExecutionContext;
use crate::protocols::openai_chat::{self, OpenAiChat};
use crate::protocols::shared::{fit_thinking_budget, invalid_request};
use crate::protocols::utils::cache::CacheControl;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub struct AlibabaChat;

const ID: &str = "alibaba-chat";

pub fn compatibility() -> Value {
    json!({ "maxTokensField": "max_completion_tokens", "reasoningField": "reasoning_content", "zaiToolStream": false })
}

pub fn capabilities() -> Value {
    json!({ "tools": { "strict": false } })
}

impl Protocol for AlibabaChat {
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
        let mut body = openai_chat::from_request_with(&request.request, model, Some(&mut CacheControl::new()))?;
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        if let Some(enable_thinking) = options.enable_thinking {
            object.insert("enable_thinking".to_string(), Value::Bool(enable_thinking));
        }
        if let Some(budget) = options.thinking_budget {
            let max_tokens = request.request.generation.max_tokens.as_ref();
            object.insert("thinking_budget".to_string(), Value::Number(fit_thinking_budget(&budget, max_tokens, 1)));
        }
        if let Some(preserve_thinking) = options.preserve_thinking {
            object.insert("preserve_thinking".to_string(), Value::Bool(preserve_thinking));
        }
        if let Some(clear_thinking) = options.clear_thinking {
            object.insert("clear_thinking".to_string(), Value::Bool(clear_thinking));
        }
        if let Some(thinking) = options.thinking {
            object.insert("thinking".to_string(), thinking);
        }
        if let Some(tool_stream) = options.tool_stream {
            object.insert("tool_stream".to_string(), Value::Bool(tool_stream));
        }
        let parallel_tool_calls = options.parallel_tool_calls.or_else(|| {
            request
                .request
                .tool_choice
                .as_ref()
                .and_then(|choice| choice.disable_parallel_tool_use)
                .map(|disabled| !disabled)
        });
        if let Some(parallel_tool_calls) = parallel_tool_calls {
            object.insert("parallel_tool_calls".to_string(), Value::Bool(parallel_tool_calls));
        }
        if let Some(repetition_penalty) = options.repetition_penalty {
            object.insert("repetition_penalty".to_string(), Value::Number(repetition_penalty));
        }
        if let Some(top_k) = request.request.generation.top_k.clone() {
            object.insert("top_k".to_string(), Value::Number(top_k));
        }
        if let Some(response_format) = options.response_format {
            object.insert("response_format".to_string(), response_format);
        }
        if let Some(enable_search) = options.enable_search {
            object.insert("enable_search".to_string(), Value::Bool(enable_search));
        }
        if let Some(search_options) = options.search_options {
            object.insert("search_options".to_string(), search_options);
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
    enable_thinking: Option<bool>,
    thinking_budget: Option<Number>,
    preserve_thinking: Option<bool>,
    clear_thinking: Option<bool>,
    thinking: Option<Value>,
    tool_stream: Option<bool>,
    parallel_tool_calls: Option<bool>,
    repetition_penalty: Option<Number>,
    response_format: Option<Value>,
    enable_search: Option<bool>,
    search_options: Option<Value>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("reasoningEffort"), "reasoningEffort")?;
    Ok(Options {
        enable_thinking: optional_bool(input.get("enableThinking"), "enableThinking")?,
        thinking_budget: optional_int(input.get("thinkingBudget"), "thinkingBudget")?,
        preserve_thinking: optional_bool(input.get("preserveThinking"), "preserveThinking")?,
        clear_thinking: optional_bool(input.get("clearThinking"), "clearThinking")?,
        thinking: optional_thinking(input.get("thinking"))?,
        tool_stream: optional_bool(input.get("toolStream"), "toolStream")?,
        parallel_tool_calls: optional_bool(input.get("parallelToolCalls"), "parallelToolCalls")?,
        repetition_penalty: optional_number(input.get("repetitionPenalty"), "repetitionPenalty")?,
        response_format: optional_response_format(input.get("responseFormat"))?,
        enable_search: optional_bool(input.get("enableSearch"), "enableSearch")?,
        search_options: optional_search_options(input.get("searchOptions"))?,
    })
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(option_error("thinking.type"));
    };
    let mut thinking = Map::new();
    thinking.insert("type".to_string(), Value::String(kind.clone()));
    Ok(Some(Value::Object(thinking)))
}

fn optional_response_format(value: Option<&Value>) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("responseFormat"))?;
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(option_error("responseFormat.type"));
    };
    let mut format = Map::new();
    format.insert("type".to_string(), Value::String(kind.clone()));
    if let Some(schema) = object.get("json_schema") {
        if !schema.is_object() {
            return Err(option_error("responseFormat.json_schema"));
        }
        format.insert("json_schema".to_string(), schema.clone());
    }
    Ok(Some(Value::Object(format)))
}

fn optional_search_options(value: Option<&Value>) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("searchOptions"))?;
    let mut options = Map::new();
    if let Some(forced_search) = optional_bool(object.get("forced_search"), "searchOptions.forced_search")? {
        options.insert("forced_search".to_string(), Value::Bool(forced_search));
    }
    if let Some(strategy) = optional_string(object.get("search_strategy"), "searchOptions.search_strategy")? {
        options.insert("search_strategy".to_string(), Value::String(strategy));
    }
    if let Some(extension) =
        optional_bool(object.get("enable_search_extension"), "searchOptions.enable_search_extension")?
    {
        options.insert("enable_search_extension".to_string(), Value::Bool(extension));
    }
    Ok(Some(Value::Object(options)))
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

fn optional_int(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(value) => match value.as_number() {
            Some(number) if number.is_i64() || number.is_u64() => Ok(Some(number.clone())),
            _ => Err(option_error(field)),
        },
    }
}

fn optional_number(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(value) => match value.as_number() {
            Some(number) => Ok(Some(number.clone())),
            None => Err(option_error(field)),
        },
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("alibaba-chat providerOptions {field} is invalid"))
}
