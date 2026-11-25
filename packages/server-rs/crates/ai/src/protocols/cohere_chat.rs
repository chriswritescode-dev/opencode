use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::protocols::shared;
use crate::protocols::utils::{lifecycle, tool_stream};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, ContentPart, FinishReason, FinishReasonDetails, LlmEvent, LlmRequest, Message, ProviderMetadata,
    SystemPart, ToolChoiceType, ToolContent, ToolDefinition, ToolResultValue, Usage,
};

pub const ID: &str = "cohere-chat";
pub const DEFAULT_BASE_URL: &str = "https://api.cohere.com/v2";
pub const PATH: &str = "/chat";

const ROUTE: &str = "Cohere Chat";
const METADATA_KEY: &str = "cohere";
const MISSING_TOOL_CALL: &str = "Cohere tool call is missing id or name";
const INVALID_EVENT: &str = "Invalid cohere-chat stream event";

pub struct CohereChat;

impl Protocol for CohereChat {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        from_request(&request.request, model)
    }

    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        Some(PATH.to_string())
    }

    fn framing(&self) -> Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, _request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(State { lifecycle: lifecycle::initial(), tools: tool_stream::empty() })
    }
}

struct Thinking {
    kind: Option<String>,
    token_budget: Option<Number>,
}

struct Options {
    thinking: Option<Thinking>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    Ok(Options { thinking: optional_thinking(input.get("thinking"))? })
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Thinking>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let kind = match object.get("type") {
        None => None,
        Some(Value::String(kind)) if kind == "enabled" || kind == "disabled" => Some(kind.clone()),
        Some(_) => return Err(option_error("thinking.type")),
    };
    let token_budget = match object.get("tokenBudget") {
        None => None,
        Some(Value::Number(number)) if is_positive_integer(number) => Some(number.clone()),
        Some(_) => return Err(option_error("thinking.tokenBudget")),
    };
    Ok(Some(Thinking { kind, token_budget }))
}

fn is_positive_integer(number: &Number) -> bool {
    number.as_f64().is_some_and(|value| value.fract() == 0.0 && value > 0.0)
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("{ID} providerOptions {field} is invalid"))
}

fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let options = decode_options(request.provider_options.as_ref())?;
    let definitions = shared::flatten_tools(&request.tools);
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        let content = if request.system.len() == 1 {
            Value::String(system_text(&request.system[0]).to_string())
        } else {
            Value::Array(request.system.iter().map(|part| text_content(system_text(part))).collect())
        };
        messages.push(message_object("system", Some(content), None, None));
    }
    for message in &request.messages {
        match message {
            Message::System(_) => {
                let update = shared::wrapped_system_update(ROUTE, message)?;
                messages.push(message_object("user", Some(Value::String(update.text)), None, None));
            }
            Message::Tool(message) => {
                for part in &message.content {
                    let ContentPart::ToolResult(part) = part else {
                        let message = format!("{ROUTE} tool messages only support tool-result content for now");
                        return Err(shared::invalid_request(message));
                    };
                    if let ToolResultValue::Content { value } = &part.result
                        && value.iter().any(|item| matches!(item, ToolContent::File { .. }))
                    {
                        return Err(shared::invalid_request(
                            "Cohere Chat does not support file content in tool results",
                        ));
                    }
                    let mut object = Map::new();
                    object.insert("role".to_string(), Value::String("tool".to_string()));
                    object.insert("tool_call_id".to_string(), Value::String(part.id.clone()));
                    object.insert("content".to_string(), Value::String(shared::tool_result_text(part)));
                    messages.push(Value::Object(object));
                }
            }
            Message::User(message) => messages.push(lower_message("user", &message.content)?),
            Message::Assistant(message) => messages.push(lower_message("assistant", &message.content)?),
        }
    }
    let selected = match request.tool_choice.as_ref() {
        Some(tool_choice) if tool_choice.r#type == ToolChoiceType::Tool => tool_choice.name.as_deref(),
        _ => None,
    };
    let selected_tools =
        definitions.iter().filter(|tool| selected.is_none_or(|selected| tool.name == selected)).collect::<Vec<_>>();
    if selected.is_some() && selected_tools.is_empty() {
        return Err(shared::invalid_request("Cohere Chat tool choice must name an available tool"));
    }
    if selected_tools.iter().any(|tool| tool.native.is_some()) {
        return Err(shared::invalid_request("Cohere Chat does not support provider-defined tools"));
    }
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), Value::Bool(true));
    if !selected_tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(selected_tools.iter().map(|tool| lower_tool(tool)).collect()));
    }
    if let Some(tool_choice) = tool_choice_value(request) {
        body.insert("tool_choice".to_string(), Value::String(tool_choice.to_string()));
    }
    if let Some(thinking) = options.thinking {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String(thinking.kind.unwrap_or_else(|| "enabled".to_string())));
        if let Some(token_budget) = thinking.token_budget {
            let fitted = shared::fit_thinking_budget(&token_budget, request.generation.max_tokens.as_ref(), 1);
            object.insert("token_budget".to_string(), Value::Number(fitted));
        }
        body.insert("thinking".to_string(), Value::Object(object));
    }
    if let Some(max_tokens) = request.generation.max_tokens.clone() {
        body.insert("max_tokens".to_string(), Value::Number(max_tokens));
    }
    if let Some(temperature) = request.generation.temperature.clone() {
        body.insert("temperature".to_string(), Value::Number(temperature));
    }
    if let Some(top_p) = request.generation.top_p.clone() {
        body.insert("p".to_string(), Value::Number(top_p));
    }
    if let Some(top_k) = request.generation.top_k.clone() {
        body.insert("k".to_string(), Value::Number(top_k));
    }
    if let Some(seed) = request.generation.seed.clone() {
        body.insert("seed".to_string(), Value::Number(seed));
    }
    if let Some(stop) = request.generation.stop.as_ref() {
        body.insert(
            "stop_sequences".to_string(),
            Value::Array(stop.iter().map(|value| Value::String(value.clone())).collect()),
        );
    }
    if let Some(frequency_penalty) = request.generation.frequency_penalty.clone() {
        body.insert("frequency_penalty".to_string(), Value::Number(frequency_penalty));
    }
    if let Some(presence_penalty) = request.generation.presence_penalty.clone() {
        body.insert("presence_penalty".to_string(), Value::Number(presence_penalty));
    }
    Ok(Value::Object(body))
}

fn system_text(part: &SystemPart) -> &str {
    let SystemPart::Text { text, .. } = part;
    text
}

fn tool_choice_value(request: &LlmRequest) -> Option<&'static str> {
    match request.tool_choice.as_ref().map(|tool_choice| &tool_choice.r#type) {
        Some(ToolChoiceType::None) => Some("NONE"),
        Some(ToolChoiceType::Required | ToolChoiceType::Tool) => Some("REQUIRED"),
        _ => None,
    }
}

fn lower_tool(tool: &ToolDefinition) -> Value {
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(tool.name.clone()));
    function.insert("description".to_string(), Value::String(tool.description.clone()));
    function.insert("parameters".to_string(), tool.input_schema.clone());
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    object.insert("function".to_string(), Value::Object(function));
    Value::Object(object)
}

fn lower_message(role: &str, parts: &[ContentPart]) -> Result<Value, AiError> {
    let mut content = Vec::new();
    let mut calls = Vec::new();
    let mut plans = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(part) => content.push(text_content(&part.text)),
            ContentPart::Reasoning(part) if role == "assistant" => {
                if tool_plan(part.provider_metadata.as_ref()) {
                    plans.push(part.text.clone());
                } else {
                    let mut object = Map::new();
                    object.insert("type".to_string(), Value::String("thinking".to_string()));
                    object.insert("thinking".to_string(), Value::String(part.text.clone()));
                    content.push(Value::Object(object));
                }
            }
            ContentPart::ToolCall(part) if role == "assistant" => {
                let mut function = Map::new();
                function.insert("name".to_string(), Value::String(part.name.clone()));
                function.insert("arguments".to_string(), Value::String(crate::json::encode_body(&part.input)));
                let mut call = Map::new();
                call.insert("id".to_string(), Value::String(part.id.clone()));
                call.insert("type".to_string(), Value::String("function".to_string()));
                call.insert("function".to_string(), Value::Object(function));
                calls.push(Value::Object(call));
            }
            ContentPart::Media(part) if role == "user" => {
                let media_type = shared::media_type(&part.media);
                if !media_type.starts_with("image/") {
                    return Err(unsupported_content(role));
                }
                let url = match shared::media_url(&part.media) {
                    Some(url) => url.to_string(),
                    None => shared::require_inline_media(ROUTE, &part.media)?.data_url,
                };
                let mut media = Map::new();
                media.insert("url".to_string(), Value::String(url));
                let mut object = Map::new();
                object.insert("type".to_string(), Value::String("image_url".to_string()));
                object.insert("image_url".to_string(), Value::Object(media));
                content.push(Value::Object(object));
            }
            _ => return Err(unsupported_content(role)),
        }
    }
    let content = (!content.is_empty()).then_some(Value::Array(content));
    let calls = (!calls.is_empty()).then_some(calls);
    let plan = (!plans.is_empty()).then_some(plans.join(""));
    Ok(message_object(role, content, calls, plan))
}

fn unsupported_content(role: &str) -> AiError {
    let types = match role {
        "user" => "text and media",
        _ => "text, reasoning, and tool-call",
    };
    shared::invalid_request(format!("{ROUTE} {role} messages only support {types} content for now"))
}

fn tool_plan(metadata: Option<&ProviderMetadata>) -> bool {
    shared::provider_metadata_value(metadata, METADATA_KEY).and_then(|metadata| metadata.get("toolPlan"))
        == Some(&Value::Bool(true))
}

fn text_content(text: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(object)
}

fn message_object(
    role: &str,
    content: Option<Value>,
    tool_calls: Option<Vec<Value>>,
    tool_plan: Option<String>,
) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(role.to_string()));
    if let Some(content) = content {
        object.insert("content".to_string(), content);
    }
    if let Some(tool_calls) = tool_calls {
        object.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if let Some(tool_plan) = tool_plan {
        object.insert("tool_plan".to_string(), Value::String(tool_plan));
    }
    Value::Object(object)
}

struct State {
    lifecycle: lifecycle::State,
    tools: tool_stream::State,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let value: Value = match serde_json::from_str(&frame) {
            Ok(value) => value,
            Err(_) => return Err(invalid_event(&frame)),
        };
        let Some(object) = value.as_object() else {
            return Err(invalid_event(&frame));
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            return Err(invalid_event(&frame));
        };
        let mut events = Vec::new();
        match kind {
            "message-start" => {
                self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
            }
            "content-start" | "content-delta" => {
                let index = event_index(object, &frame)?;
                let content = child(child(child(object, "delta", &frame)?, "message", &frame)?, "content", &frame)?;
                let text = optional_string_field(content, "text", &frame)?;
                let thinking = optional_string_field(content, "thinking", &frame)?;
                if kind == "content-delta" {
                    let id = index_id(index);
                    self.lifecycle = match thinking {
                        Some(thinking) => {
                            lifecycle::reasoning_delta(self.lifecycle.clone(), &mut events, &id, &thinking, None)
                        }
                        None => lifecycle::text_delta(
                            self.lifecycle.clone(),
                            &mut events,
                            &id,
                            text.as_deref().unwrap_or_default(),
                            None,
                        ),
                    };
                }
            }
            "content-end" => {
                let id = index_id(event_index(object, &frame)?);
                let state = lifecycle::reasoning_end(self.lifecycle.clone(), &mut events, &id, None, None);
                self.lifecycle = lifecycle::text_end(state, &mut events, &id, None, None);
            }
            "tool-plan-delta" => {
                let plan = child(child(object, "delta", &frame)?, "message", &frame)?
                    .get("tool_plan")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_event(&frame))?;
                let mut inner = Map::new();
                inner.insert("toolPlan".to_string(), Value::Bool(true));
                let mut metadata = ProviderMetadata::new();
                metadata.insert(METADATA_KEY.to_string(), Value::Object(inner));
                self.lifecycle =
                    lifecycle::reasoning_delta(self.lifecycle.clone(), &mut events, "tool-plan", plan, Some(metadata));
            }
            "tool-call-start" | "tool-call-delta" => {
                let key = index_key(event_index(object, &frame)?);
                let call = child(child(child(object, "delta", &frame)?, "message", &frame)?, "tool_calls", &frame)?;
                let id = optional_string_field(call, "id", &frame)?;
                let function = child(call, "function", &frame)?;
                let name = optional_string_field(function, "name", &frame)?;
                let text = optional_string_field(function, "arguments", &frame)?.unwrap_or_default();
                let outcome = tool_stream::append_or_start(
                    ID,
                    &mut self.tools,
                    key,
                    tool_stream::AppendDelta {
                        id: id.as_deref(),
                        name: name.as_deref(),
                        text: &text,
                        provider_metadata: None,
                    },
                    MISSING_TOOL_CALL,
                )?;
                events.extend(outcome.events);
            }
            "tool-call-end" => {
                let key = index_key(event_index(object, &frame)?);
                let outcome = tool_stream::finish(ID, &mut self.tools, key, None)?;
                events.extend(outcome.events);
            }
            "message-end" => {
                let delta = child(object, "delta", &frame)?;
                let raw = delta
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_event(&frame))?
                    .to_string();
                let usage = match delta.get("usage") {
                    Some(value) => Some(filter_usage(value).ok_or_else(|| invalid_event(&frame))?),
                    None => None,
                };
                let pending = tool_stream::finish_all(ID, &mut self.tools)?;
                events.extend(pending.events);
                let usage = usage.as_ref().map(map_usage);
                self.lifecycle =
                    lifecycle::finish(self.lifecycle.clone(), &mut events, finish_reason(&raw), usage, None);
            }
            _ => {}
        }
        Ok(events)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(frame) else {
            return false;
        };
        value.get("type").and_then(Value::as_str) == Some("message-end")
    }
}

fn invalid_event(frame: &str) -> AiError {
    shared::event_error(ID, INVALID_EVENT, Some(frame.to_string()))
}

fn child<'a>(object: &'a Map<String, Value>, key: &str, frame: &str) -> Result<&'a Map<String, Value>, AiError> {
    object.get(key).and_then(Value::as_object).ok_or_else(|| invalid_event(frame))
}

fn optional_string_field(object: &Map<String, Value>, key: &str, frame: &str) -> Result<Option<String>, AiError> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_event(frame)),
    }
}

fn event_index<'a>(object: &'a Map<String, Value>, frame: &str) -> Result<&'a Number, AiError> {
    object.get("index").and_then(Value::as_number).ok_or_else(|| invalid_event(frame))
}

fn index_id(index: &Number) -> String {
    match index.as_i64() {
        Some(index) => index.to_string(),
        None => index.as_f64().map(|index| index.to_string()).unwrap_or_default(),
    }
}

fn index_key(index: &Number) -> i64 {
    index.as_i64().or_else(|| index.as_f64().map(|index| index as i64)).unwrap_or_default()
}

fn filter_usage(value: &Value) -> Option<Map<String, Value>> {
    let object = value.as_object()?;
    let mut usage = Map::new();
    for (key, value) in object {
        match key.as_str() {
            "tokens" | "billed_units" => {
                usage.insert(key.clone(), Value::Object(filter_counts(value)?));
            }
            "cached_tokens" => {
                value.as_number()?;
                usage.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    Some(usage)
}

fn filter_counts(value: &Value) -> Option<Map<String, Value>> {
    let object = value.as_object()?;
    let mut counts = Map::new();
    for (key, value) in object {
        if matches!(key.as_str(), "input_tokens" | "output_tokens" | "reasoning_tokens") {
            value.as_number()?;
            counts.insert(key.clone(), value.clone());
        }
    }
    Some(counts)
}

fn map_usage(usage: &Map<String, Value>) -> Usage {
    let tokens = usage.get("tokens").and_then(Value::as_object);
    let count = |key: &str| tokens.and_then(|tokens| tokens.get(key)).and_then(Value::as_u64);
    let input = count("input_tokens");
    let output = count("output_tokens");
    let cached = usage.get("cached_tokens").and_then(Value::as_u64);
    let mut metadata = ProviderMetadata::new();
    metadata.insert(METADATA_KEY.to_string(), Value::Object(usage.clone()));
    Usage {
        context_tokens: None,
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: shared::subtract_tokens(input, cached),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: None,
        reasoning_tokens: count("reasoning_tokens"),
        total_tokens: shared::total_tokens(input, output, None),
        cost: None,
        provider_metadata: Some(metadata),
    }
}

fn finish_reason(raw: &str) -> FinishReasonDetails {
    let normalized = match raw {
        "COMPLETE" | "STOP_SEQUENCE" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "TOOL_CALL" => FinishReason::ToolCalls,
        "ERROR" | "TIMEOUT" => FinishReason::Error,
        _ => FinishReason::Unknown,
    };
    FinishReasonDetails { normalized, raw: Some(raw.to_string()), category: None, explanation: None }
}
