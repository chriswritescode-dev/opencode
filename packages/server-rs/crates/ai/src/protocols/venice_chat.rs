use std::collections::BTreeMap;

use serde_json::{Map, Number, Value, json};

use crate::ExecutionContext;
use crate::protocols::openai_chat::{self, AssistantTransform, OpenAiChat};
use crate::protocols::shared;
use crate::protocols::utils::cache::CacheControl;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{AiError, AssistantMessage, ContentPart, LlmEvent, ProviderMetadata, ToolCallPart};

pub const ID: &str = "venice-chat";
pub const PATH: &str = "/chat/completions";

const MARKER: &str = "__ENCRYPTED_REASONING__";
const METADATA_KEY: &str = "venice";

pub struct VeniceChat;

pub fn compatibility() -> Value {
    json!({ "maxTokensField": "max_completion_tokens" })
}

pub fn capabilities() -> Value {
    json!({ "promptCacheKey": true })
}

impl Protocol for VeniceChat {
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
        let mut cache_control = CacheControl::new();
        let mut assistant =
            |message: &AssistantMessage, lowered: Map<String, Value>| apply_assistant_metadata(message, lowered);
        let mut body = openai_chat::from_request_with_assistant(
            &request.request,
            model,
            Some(&mut cache_control),
            Some(&mut assistant as &mut AssistantTransform<'_>),
        )?;
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        let max_tokens = options
            .max_completion_tokens
            .or(options.max_tokens)
            .or_else(|| request.request.generation.max_tokens.clone());
        set_or_remove(object, "max_completion_tokens", max_tokens.map(Value::Number));
        object.remove("reasoning_effort");
        let reasoning = match options.reasoning_effort {
            Some(effort) => {
                let mut reasoning = options.reasoning.unwrap_or_default();
                reasoning.insert("effort".to_string(), Value::String(effort));
                Some(reasoning)
            }
            None => options.reasoning,
        };
        set_or_remove(object, "reasoning", reasoning.map(Value::Object));
        let mut parameters = options.venice_parameters.unwrap_or_default();
        parameters.entry("include_venice_system_prompt".to_string()).or_insert(Value::Bool(false));
        object.insert("venice_parameters".to_string(), Value::Object(parameters));
        let retention =
            options.prompt_cache_retention.map(Value::String).or_else(|| object.get("prompt_cache_retention").cloned());
        set_or_remove(object, "prompt_cache_retention", retention);
        set_or_remove(object, "parallel_tool_calls", options.parallel_tool_calls.map(Value::Bool));
        set_or_remove(object, "min_p", options.min_p.map(Value::Number));
        set_or_remove(object, "top_k", request.request.generation.top_k.clone().map(Value::Number));
        set_or_remove(object, "repetition_penalty", options.repetition_penalty.map(Value::Number));
        set_or_remove(
            object,
            "stop_token_ids",
            options.stop_token_ids.map(|values| Value::Array(values.into_iter().map(Value::Number).collect())),
        );
        set_or_remove(object, "logprobs", options.logprobs.map(Value::Bool));
        set_or_remove(object, "top_logprobs", options.top_logprobs.map(Value::Number));
        set_or_remove(object, "max_temp", options.max_temp.map(Value::Number));
        set_or_remove(object, "min_temp", options.min_temp.map(Value::Number));
        set_or_remove(object, "response_format", options.response_format);
        set_or_remove(object, "user", options.user.map(Value::String));
        Ok(body)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        OpenAiChat.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        OpenAiChat.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(State {
            inner: OpenAiChat.new_state(request, context),
            pending: String::new(),
            reasoning: String::new(),
            encrypted: false,
            signature: None,
            tools: BTreeMap::new(),
        })
    }
}

fn set_or_remove(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            object.insert(key.to_string(), value);
        }
        None => {
            object.remove(key);
        }
    }
}

fn apply_assistant_metadata(message: &AssistantMessage, mut lowered: Map<String, Value>) -> Result<Value, AiError> {
    let mut signature = None;
    let mut encrypted = None;
    for part in &message.content {
        let Some(metadata) = shared::provider_metadata_value(part_provider_metadata(part), METADATA_KEY) else {
            continue;
        };
        if signature.is_none() {
            signature = metadata.get("messageThoughtSignature").and_then(Value::as_str).map(str::to_string);
        }
        if encrypted.is_none() {
            encrypted = metadata.get("encryptedReasoningContent").and_then(Value::as_str).map(str::to_string);
        }
    }
    if let Some(signature) = signature {
        lowered.insert("thought_signature".to_string(), Value::String(signature));
    }
    if let Some(encrypted) = encrypted {
        lowered.insert("reasoning_content".to_string(), Value::String(encrypted));
    }
    let calls = message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolCall(part) => Some(part),
            _ => None,
        })
        .collect::<Vec<&ToolCallPart>>();
    if let Some(Value::Array(tool_calls)) = lowered.get_mut("tool_calls") {
        for (index, call) in tool_calls.iter_mut().enumerate() {
            let Some(source) = calls.get(index) else {
                continue;
            };
            let Some(signature) = shared::provider_metadata_value(source.provider_metadata.as_ref(), METADATA_KEY)
                .and_then(|metadata| metadata.get("thoughtSignature"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Some(object) = call.as_object_mut() {
                object.insert("thought_signature".to_string(), Value::String(signature.to_string()));
            }
        }
    }
    Ok(Value::Object(lowered))
}

fn part_provider_metadata(part: &ContentPart) -> Option<&ProviderMetadata> {
    match part {
        ContentPart::Text(part) => part.provider_metadata.as_ref(),
        ContentPart::Reasoning(part) => part.provider_metadata.as_ref(),
        ContentPart::ToolCall(part) => part.provider_metadata.as_ref(),
        _ => None,
    }
}

struct Options {
    reasoning_effort: Option<String>,
    reasoning: Option<Map<String, Value>>,
    venice_parameters: Option<Map<String, Value>>,
    prompt_cache_retention: Option<String>,
    parallel_tool_calls: Option<bool>,
    max_completion_tokens: Option<Number>,
    max_tokens: Option<Number>,
    min_p: Option<Number>,
    repetition_penalty: Option<Number>,
    stop_token_ids: Option<Vec<Number>>,
    logprobs: Option<bool>,
    top_logprobs: Option<Number>,
    max_temp: Option<Number>,
    min_temp: Option<Number>,
    response_format: Option<Value>,
    user: Option<String>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("promptCacheKey"), "promptCacheKey")?;
    Ok(Options {
        reasoning_effort: optional_string(input.get("reasoningEffort"), "reasoningEffort")?,
        reasoning: optional_reasoning(input.get("reasoning"))?,
        venice_parameters: optional_parameters(input.get("veniceParameters"))?,
        prompt_cache_retention: optional_string(input.get("promptCacheRetention"), "promptCacheRetention")?,
        parallel_tool_calls: optional_bool(input.get("parallelToolCalls"), "parallelToolCalls")?,
        max_completion_tokens: optional_number(input.get("maxCompletionTokens"), "maxCompletionTokens")?,
        max_tokens: optional_number(input.get("maxTokens"), "maxTokens")?,
        min_p: optional_number(input.get("minP"), "minP")?,
        repetition_penalty: optional_number(input.get("repetitionPenalty"), "repetitionPenalty")?,
        stop_token_ids: optional_numbers(input.get("stopTokenIds"), "stopTokenIds")?,
        logprobs: optional_bool(input.get("logprobs"), "logprobs")?,
        top_logprobs: optional_number(input.get("topLogprobs"), "topLogprobs")?,
        max_temp: optional_number(input.get("maxTemp"), "maxTemp")?,
        min_temp: optional_number(input.get("minTemp"), "minTemp")?,
        response_format: optional_object(input.get("responseFormat"), "responseFormat")?,
        user: optional_string(input.get("user"), "user")?,
    })
}

const STRING_PARAMETERS: [(&str, &str); 2] =
    [("enableWebSearch", "enable_web_search"), ("characterSlug", "character_slug")];

const BOOL_PARAMETERS: [(&str, &str); 8] = [
    ("enableWebScraping", "enable_web_scraping"),
    ("enableWebCitations", "enable_web_citations"),
    ("enableXSearch", "enable_x_search"),
    ("stripThinkingResponse", "strip_thinking_response"),
    ("disableThinking", "disable_thinking"),
    ("includeVeniceSystemPrompt", "include_venice_system_prompt"),
    ("includeSearchResultsInStream", "include_search_results_in_stream"),
    ("returnSearchResultsAsDocuments", "return_search_results_as_documents"),
];

fn optional_parameters(value: Option<&Value>) -> Result<Option<Map<String, Value>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("veniceParameters"))?;
    let mut parameters = Map::new();
    for (field, value) in object {
        let field_name = field.as_str();
        if let Some((_, key)) = STRING_PARAMETERS.iter().find(|(name, _)| *name == field_name) {
            let Some(value) = value.as_str() else {
                return Err(option_error(&format!("veniceParameters.{field}")));
            };
            parameters.insert(key.to_string(), Value::String(value.to_string()));
        } else if let Some((_, key)) = BOOL_PARAMETERS.iter().find(|(name, _)| *name == field_name) {
            let Some(value) = value.as_bool() else {
                return Err(option_error(&format!("veniceParameters.{field}")));
            };
            parameters.insert(key.to_string(), Value::Bool(value));
        }
    }
    Ok(Some(parameters))
}

fn optional_reasoning(value: Option<&Value>) -> Result<Option<Map<String, Value>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("reasoning"))?;
    let mut reasoning = Map::new();
    for (field, value) in object {
        match field.as_str() {
            "effort" | "summary" => {
                let Some(value) = value.as_str() else {
                    return Err(option_error(&format!("reasoning.{field}")));
                };
                reasoning.insert(field.clone(), Value::String(value.to_string()));
            }
            "enabled" => {
                let Some(value) = value.as_bool() else {
                    return Err(option_error("reasoning.enabled"));
                };
                reasoning.insert(field.clone(), Value::Bool(value));
            }
            _ => {}
        }
    }
    Ok(Some(reasoning))
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

fn optional_number(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_numbers(value: Option<&Value>, field: &str) -> Result<Option<Vec<Number>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let values = value.as_array().ok_or_else(|| option_error(field))?;
    let mut numbers = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = value.as_number() else {
            return Err(option_error(field));
        };
        numbers.push(value.clone());
    }
    Ok(Some(numbers))
}

fn optional_object(value: Option<&Value>, field: &str) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_object() { Ok(Some(value.clone())) } else { Err(option_error(field)) }
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("{ID} providerOptions {field} is invalid"))
}

struct State {
    inner: Box<dyn ProtocolState>,
    pending: String,
    reasoning: String,
    encrypted: bool,
    signature: Option<String>,
    tools: BTreeMap<String, String>,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        if self.inner.terminal(&frame) {
            return Ok(Vec::new());
        }
        let Ok(mut event) = serde_json::from_str::<Value>(&frame) else {
            return Err(shared::event_error(ID, "Invalid venice-chat stream event", Some(frame)));
        };
        if !event.is_object() {
            return Err(shared::event_error(ID, "Invalid venice-chat stream event", Some(frame)));
        }
        if let Some(error) = event.get("error").and_then(Value::as_str) {
            return Err(AiError {
                reason: classify_provider_failure(ProviderFailure {
                    message: error_message(error, &event),
                    raw_body: Some(crate::json::encode_body(&event)),
                    ..ProviderFailure::default()
                }),
            });
        }
        let delta = event
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .filter(|delta| delta.is_object());
        let scalar =
            delta.and_then(|delta| delta.get("reasoning_content")).and_then(Value::as_str).unwrap_or("").to_string();
        let signature =
            delta.and_then(|delta| delta.get("thought_signature")).and_then(Value::as_str).map(str::to_string);
        let tool_signatures = tool_signatures(delta);
        let text = format!("{}{scalar}", self.pending);
        let marker = text.find(MARKER);
        let pending = if marker.is_some() || self.encrypted {
            0
        } else {
            (1..=text.len().min(MARKER.len() - 1)).rev().find(|length| text.ends_with(&MARKER[..*length])).unwrap_or(0)
        };
        let visible = if self.encrypted {
            String::new()
        } else if let Some(marker) = marker {
            text[..marker].to_string()
        } else {
            text[..text.len() - pending].to_string()
        };
        {
            let object = event.as_object_mut().expect("checked stream event object");
            rewrite_usage(object);
            rewrite_delta(object, &visible, marker.is_some());
        }
        let events = self.inner.step(crate::json::encode_body(&event))?;
        for (index, id, signature) in tool_signatures {
            let index = index.or_else(|| self.inner.latest_tool_index());
            let id = id.or_else(|| index.and_then(|index| self.inner.tool_id_at(index)));
            if let Some(id) = id.filter(|id| !id.is_empty()) {
                self.tools.insert(id, signature);
            }
        }
        if pending == 0 {
            self.pending.clear();
        } else {
            self.pending = text[text.len() - pending..].to_string();
        }
        self.reasoning.push_str(&scalar);
        self.encrypted = self.encrypted || marker.is_some();
        if signature.is_some() {
            self.signature = signature;
        }
        Ok(events)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let events = self.inner.halt()?;
        let mut output = Vec::with_capacity(events.len());
        for mut event in events {
            let kind = match &event {
                LlmEvent::ReasoningEnd { .. } => Kind::ReasoningEnd,
                LlmEvent::TextEnd { .. } => Kind::TextEnd,
                LlmEvent::ToolCall { .. } => Kind::ToolCall,
                LlmEvent::ToolInputEnd { .. } => Kind::ToolInputEnd,
                _ => {
                    output.push(event);
                    continue;
                }
            };
            let id = match &event {
                LlmEvent::ReasoningEnd { id, .. }
                | LlmEvent::TextEnd { id, .. }
                | LlmEvent::ToolCall { id, .. }
                | LlmEvent::ToolInputEnd { id, .. } => id.clone(),
                _ => unreachable!(),
            };
            let mut metadata = event_provider_metadata(&event).cloned().unwrap_or_default();
            let mut venice = metadata.get(METADATA_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
            if let Some(signature) = self.signature.as_deref().filter(|signature| !signature.is_empty()) {
                venice.insert("messageThoughtSignature".to_string(), Value::String(signature.to_string()));
            }
            if matches!(kind, Kind::ToolCall | Kind::ToolInputEnd)
                && let Some(signature) = self.tools.get(&id).filter(|signature| !signature.is_empty())
            {
                venice.insert("thoughtSignature".to_string(), Value::String(signature.clone()));
            }
            if matches!(kind, Kind::ReasoningEnd) && self.encrypted {
                venice.insert("encryptedReasoningContent".to_string(), Value::String(self.reasoning.clone()));
            }
            metadata.insert(METADATA_KEY.to_string(), Value::Object(venice));
            if matches!(kind, Kind::ReasoningEnd) && !self.pending.is_empty() {
                output.push(LlmEvent::ReasoningDelta {
                    id: id.clone(),
                    text: self.pending.clone(),
                    provider_metadata: None,
                });
            }
            set_event_provider_metadata(&mut event, metadata);
            output.push(event);
        }
        Ok(output)
    }

    fn continuation(&mut self) -> Option<Result<String, AiError>> {
        self.inner.continuation()
    }

    fn terminal(&self, frame: &Frame) -> bool {
        self.inner.terminal(frame)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    ReasoningEnd,
    TextEnd,
    ToolCall,
    ToolInputEnd,
}

fn error_message(error: &str, event: &Value) -> String {
    let mut message = error.to_string();
    let empty = Vec::new();
    for issue in event.get("issues").and_then(Value::as_array).unwrap_or(&empty) {
        let Some(text) = issue.get("message").and_then(Value::as_str) else {
            continue;
        };
        let path = issue
            .get("path")
            .and_then(Value::as_array)
            .map(|path| {
                path.iter()
                    .filter_map(|segment| match segment {
                        Value::String(segment) => Some(segment.clone()),
                        Value::Number(segment) => Some(segment.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<String>>()
                    .join(".")
            })
            .unwrap_or_default();
        message.push_str("; ");
        if path.is_empty() {
            message.push_str(text);
        } else {
            message.push_str(&format!("{path}: {text}"));
        }
    }
    message
}

fn tool_signatures(delta: Option<&Value>) -> Vec<(Option<i64>, Option<String>, String)> {
    let Some(calls) = delta.and_then(|delta| delta.get("tool_calls")).and_then(Value::as_array) else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let signature =
                call.get("thought_signature").and_then(Value::as_str).filter(|signature| !signature.is_empty())?;
            let index = call.get("index").and_then(Value::as_i64);
            let id = call.get("id").and_then(Value::as_str).map(str::to_string);
            Some((index, id, signature.to_string()))
        })
        .collect()
}

fn rewrite_usage(object: &mut Map<String, Value>) {
    let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(details) = usage.get_mut("prompt_tokens_details").and_then(Value::as_object_mut) else {
        return;
    };
    if details.get("cache_write_tokens").is_none_or(Value::is_null) {
        match details.get("cache_creation_input_tokens").cloned() {
            Some(value) if !value.is_null() => {
                details.insert("cache_write_tokens".to_string(), value);
            }
            _ => {
                details.remove("cache_write_tokens");
            }
        }
    }
}

fn rewrite_delta(object: &mut Map<String, Value>, visible: &str, marker: bool) {
    let Some(choice) = object.get_mut("choices").and_then(Value::as_array_mut).and_then(|choices| choices.first_mut())
    else {
        return;
    };
    let Some(delta) = choice.get_mut("delta").and_then(Value::as_object_mut) else {
        return;
    };
    if visible.is_empty() {
        delta.remove("reasoning_content");
    } else {
        delta.insert("reasoning_content".to_string(), Value::String(visible.to_string()));
    }
    if delta.get("reasoning_details").is_none_or(Value::is_null) {
        if marker {
            delta.insert("reasoning_details".to_string(), Value::Array(Vec::new()));
        } else {
            delta.remove("reasoning_details");
        }
    }
}

fn event_provider_metadata(event: &LlmEvent) -> Option<&ProviderMetadata> {
    match event {
        LlmEvent::ReasoningEnd { provider_metadata, .. }
        | LlmEvent::TextEnd { provider_metadata, .. }
        | LlmEvent::ToolCall { provider_metadata, .. }
        | LlmEvent::ToolInputEnd { provider_metadata, .. } => provider_metadata.as_ref(),
        _ => None,
    }
}

fn set_event_provider_metadata(event: &mut LlmEvent, metadata: ProviderMetadata) {
    match event {
        LlmEvent::ReasoningEnd { provider_metadata, .. }
        | LlmEvent::TextEnd { provider_metadata, .. }
        | LlmEvent::ToolCall { provider_metadata, .. }
        | LlmEvent::ToolInputEnd { provider_metadata, .. } => *provider_metadata = Some(metadata),
        _ => {}
    }
}
