use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use regex_lite::Regex;
use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::id::IdSource;
use crate::protocols::shared;
use crate::protocols::utils::gemini_generate_content;
use crate::protocols::utils::lifecycle;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, AiErrorReason, AssistantMessage, ContentPart, FinishReason, FinishReasonDetails,
    InvalidProviderOutputClassification, LlmEvent, LlmRequest, MediaAsset, MediaSource, Message, ProviderMetadata,
    SanitizerKind, SystemPart, ToolChoice, ToolChoiceType, ToolContent, ToolDefinition, ToolMessage, ToolResultPart,
    ToolResultValue, Usage, UserMessage,
};

pub const ID: &str = "gemini";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

const CONTINUATION: &str = "CONTINUATION";
const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";
const MIN_THINKING_BUDGET: i64 = 512;
const ADAPTER: &str = "Gemini";

const RECOVERABLE_FINISH_REASONS: [&str; 3] = ["MALFORMED_RESPONSE", "MALFORMED_FUNCTION_CALL", "UNEXPECTED_TOOL_CALL"];

static OMITS_CALL_IDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^gemini(?:-live)?-(\d+)").unwrap());
static LEGACY_TOOL_MEDIA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)gemini-2[.-]5(?:[.-]|$)").unwrap());

pub struct Gemini;

#[derive(Debug, Clone, Default)]
struct Options {
    cached_content: Option<String>,
    service_tier: Option<String>,
    safety_settings: Option<Vec<Value>>,
    thinking_config: Option<ThinkingConfigInput>,
}

#[derive(Debug, Clone, Default)]
struct ThinkingConfigInput {
    thinking_budget: Option<Number>,
    include_thoughts: Option<bool>,
    thinking_level: Option<String>,
}

fn decode_options(options: Option<&Map<String, Value>>) -> Options {
    let Some(options) = options else {
        return Options::default();
    };
    Options {
        cached_content: options.get("cachedContent").and_then(Value::as_str).map(str::to_string),
        service_tier: options.get("serviceTier").and_then(Value::as_str).map(str::to_string),
        safety_settings: decode_safety_settings(options.get("safetySettings")),
        thinking_config: decode_thinking_config(options.get("thinkingConfig")),
    }
}

fn decode_safety_settings(value: Option<&Value>) -> Option<Vec<Value>> {
    value?.as_array()?.iter().map(lower_safety_setting).collect()
}

fn lower_safety_setting(entry: &Value) -> Option<Value> {
    let category = entry.get("category")?.as_str()?;
    let threshold = entry.get("threshold")?.as_str()?;
    let mut lowered = Map::new();
    lowered.insert("category".to_string(), Value::String(category.to_string()));
    lowered.insert("threshold".to_string(), Value::String(threshold.to_string()));
    Some(Value::Object(lowered))
}

fn decode_thinking_config(value: Option<&Value>) -> Option<ThinkingConfigInput> {
    let object = value?.as_object()?;
    Some(ThinkingConfigInput {
        thinking_budget: object.get("thinkingBudget").and_then(Value::as_number).cloned(),
        include_thoughts: object.get("includeThoughts").and_then(Value::as_bool),
        thinking_level: object.get("thinkingLevel").and_then(Value::as_str).map(str::to_string),
    })
}

fn omits_function_call_ids(model_id: &str) -> bool {
    OMITS_CALL_IDS
        .captures(model_id)
        .and_then(|captures| captures.get(1))
        .and_then(|digits| digits.as_str().parse::<u64>().ok())
        .is_some_and(|major| major < 3)
}

fn routes_legacy_tool_media(model_id: &str) -> bool {
    LEGACY_TOOL_MEDIA.is_match(model_id)
}

fn provider_metadata_value<'a>(metadata: Option<&'a ProviderMetadata>, key: &str) -> Option<&'a Value> {
    metadata?.get(key)
}

fn thought_signature(metadata: Option<&ProviderMetadata>, key: &str) -> Option<String> {
    provider_metadata_value(metadata, key)?.get("thoughtSignature")?.as_str().map(str::to_string)
}

fn signature_metadata(key: &str, signature: Option<&str>) -> Option<ProviderMetadata> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    let mut inner = Map::new();
    inner.insert("thoughtSignature".to_string(), Value::String(signature.to_string()));
    let mut metadata = ProviderMetadata::new();
    metadata.insert(key.to_string(), Value::Object(inner));
    Some(metadata)
}

fn text_part(text: String) -> Value {
    let mut part = Map::new();
    part.insert("text".to_string(), Value::String(text));
    Value::Object(part)
}

#[derive(Debug, Clone)]
struct Content {
    role: &'static str,
    parts: Vec<Value>,
}

impl Content {
    fn new(role: &'static str, parts: Vec<Value>) -> Self {
        Self { role, parts }
    }

    fn into_value(self) -> Value {
        let mut value = Map::new();
        value.insert("role".to_string(), Value::String(self.role.to_string()));
        value.insert("parts".to_string(), Value::Array(self.parts));
        Value::Object(value)
    }
}

fn has_function_response(parts: &[Value]) -> bool {
    parts.iter().any(|part| part.get("functionResponse").is_some())
}

struct Lowering {
    contents: Vec<Content>,
    pending_media: Option<Vec<Value>>,
    metadata_key: String,
    omit_call_ids: bool,
    legacy_tool_media: bool,
}

impl Lowering {
    fn flush_media(&mut self) {
        let Some(pending) = self.pending_media.take() else {
            return;
        };
        let mut parts = vec![text_part("Attached media from tool result:".to_string())];
        parts.extend(pending);
        self.contents.push(Content::new("user", parts));
    }

    fn push_system(&mut self, message: &Message) -> Result<(), AiError> {
        let part = shared::wrapped_system_update(ADAPTER, message)?;
        let text = text_part(part.text);
        let previous = self
            .contents
            .last_mut()
            .filter(|previous| previous.role == "user" && !has_function_response(&previous.parts));
        match previous {
            Some(previous) => previous.parts.push(text),
            None => self.contents.push(Content::new("user", vec![text])),
        }
        Ok(())
    }

    fn push_user(&mut self, message: &UserMessage) -> Result<(), AiError> {
        let mut parts = Vec::new();
        for part in &message.content {
            match part {
                ContentPart::Text(part) => parts.push(text_part(part.text.clone())),
                ContentPart::Media(part) => {
                    parts.push(gemini_generate_content::media_part(ADAPTER, &part.media)?);
                }
                _ => {
                    return Err(shared::invalid_request(
                        "Gemini user messages only support text and media content for now",
                    ));
                }
            }
        }
        self.contents.push(Content::new("user", parts));
        Ok(())
    }

    fn push_assistant(&mut self, message: &AssistantMessage) -> Result<(), AiError> {
        let mut parts = Vec::new();
        let mut has_signed_tool_call = false;
        for part in &message.content {
            match part {
                ContentPart::Text(part) => {
                    let mut lowered = Map::new();
                    lowered.insert("text".to_string(), Value::String(part.text.clone()));
                    if let Some(signature) = thought_signature(part.provider_metadata.as_ref(), &self.metadata_key) {
                        lowered.insert("thoughtSignature".to_string(), Value::String(signature));
                    }
                    parts.push(Value::Object(lowered));
                }
                ContentPart::Media(part) => {
                    let mut lowered = gemini_generate_content::media_part(ADAPTER, &part.media)?
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    if let Some(signature) = thought_signature(part.provider_metadata.as_ref(), &self.metadata_key) {
                        lowered.insert("thoughtSignature".to_string(), Value::String(signature));
                    }
                    parts.push(Value::Object(lowered));
                }
                ContentPart::Reasoning(part) => {
                    let mut lowered = Map::new();
                    lowered.insert("text".to_string(), Value::String(part.text.clone()));
                    lowered.insert("thought".to_string(), Value::Bool(true));
                    if let Some(signature) = thought_signature(part.provider_metadata.as_ref(), &self.metadata_key) {
                        lowered.insert("thoughtSignature".to_string(), Value::String(signature));
                    }
                    parts.push(Value::Object(lowered));
                }
                ContentPart::ToolCall(part) => {
                    let signature = thought_signature(part.provider_metadata.as_ref(), &self.metadata_key);
                    let mut function_call = Map::new();
                    if !self.omit_call_ids {
                        function_call.insert("id".to_string(), Value::String(part.id.clone()));
                    }
                    function_call.insert("name".to_string(), Value::String(part.name.clone()));
                    function_call.insert("args".to_string(), part.input.clone());
                    let mut lowered = Map::new();
                    lowered.insert("functionCall".to_string(), Value::Object(function_call));
                    let emitted = match &signature {
                        Some(signature) => Some(signature.clone()),
                        None if !has_signed_tool_call => Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_string()),
                        None => None,
                    };
                    if let Some(signature) = emitted {
                        lowered.insert("thoughtSignature".to_string(), Value::String(signature));
                    }
                    if signature.is_some() {
                        has_signed_tool_call = true;
                    }
                    parts.push(Value::Object(lowered));
                }
                _ => {
                    return Err(shared::invalid_request(
                        "Gemini assistant messages only support text, reasoning, tool-call, and media content for now",
                    ));
                }
            }
        }
        self.contents.push(Content::new("model", parts));
        Ok(())
    }

    fn lower_tool_result(&mut self, part: &ToolResultPart) -> Result<Value, AiError> {
        let ToolResultValue::Content { value } = &part.result else {
            let mut response = Map::new();
            response.insert("name".to_string(), Value::String(part.name.clone()));
            response.insert("content".to_string(), Value::String(shared::tool_result_text(part)));
            return Ok(self.function_response(part, response, Vec::new()));
        };
        let mut texts = Vec::new();
        let mut media = Vec::new();
        for item in value {
            if let ToolContent::Text { text } = item {
                texts.push(text.clone());
                continue;
            }
            let Some(media_part) = gemini_generate_content::tool_media(item) else {
                continue;
            };
            let lowered = gemini_generate_content::media_part(ADAPTER, &media_part.media)?;
            if self.legacy_tool_media || lowered.get("fileData").is_some() {
                self.pending_media.get_or_insert_with(Vec::new).push(lowered);
            } else {
                media.push(lowered);
            }
        }
        let mut response = Map::new();
        response.insert("name".to_string(), Value::String(part.name.clone()));
        response.insert("content".to_string(), Value::String(texts.join("\n")));
        Ok(self.function_response(part, response, media))
    }

    fn function_response(&self, part: &ToolResultPart, response: Map<String, Value>, parts: Vec<Value>) -> Value {
        let mut function_response = Map::new();
        if !self.omit_call_ids {
            function_response.insert("id".to_string(), Value::String(part.id.clone()));
        }
        function_response.insert("name".to_string(), Value::String(part.name.clone()));
        function_response.insert("response".to_string(), Value::Object(response));
        if !parts.is_empty() {
            function_response.insert("parts".to_string(), Value::Array(parts));
        }
        let mut lowered = Map::new();
        lowered.insert("functionResponse".to_string(), Value::Object(function_response));
        Value::Object(lowered)
    }

    fn push_tool(&mut self, message: &ToolMessage) -> Result<(), AiError> {
        let mut parts = Vec::new();
        for part in &message.content {
            let ContentPart::ToolResult(part) = part else {
                continue;
            };
            parts.push(self.lower_tool_result(part)?);
        }
        let previous = self
            .contents
            .last_mut()
            .filter(|previous| previous.role == "user" && has_function_response(&previous.parts));
        match previous {
            Some(previous) => previous.parts.extend(parts),
            None => self.contents.push(Content::new("user", parts)),
        }
        Ok(())
    }
}

fn lower_messages(request: &LlmRequest, model: &Model) -> Result<Vec<Value>, AiError> {
    let provider_metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let mut lowering = Lowering {
        contents: Vec::new(),
        pending_media: None,
        metadata_key: provider_metadata_key,
        omit_call_ids: omits_function_call_ids(&model.id),
        legacy_tool_media: routes_legacy_tool_media(&model.id),
    };
    for message in &request.messages {
        if !matches!(message, Message::Tool(_)) {
            lowering.flush_media();
        }
        match message {
            Message::System(_) => lowering.push_system(message)?,
            Message::User(message) => lowering.push_user(message)?,
            Message::Assistant(message) => lowering.push_assistant(message)?,
            Message::Tool(message) => lowering.push_tool(message)?,
        }
    }
    lowering.flush_media();
    Ok(lowering.contents.into_iter().map(Content::into_value).collect())
}

fn lower_tool(tool: &ToolDefinition) -> Value {
    let mut lowered = Map::new();
    lowered.insert("name".to_string(), Value::String(tool.name.clone()));
    lowered.insert("description".to_string(), Value::String(tool.description.clone()));
    lowered.insert("parametersJsonSchema".to_string(), tool.input_schema.clone());
    Value::Object(lowered)
}

fn lower_tool_config(tool_choice: &ToolChoice) -> Result<Value, AiError> {
    let mode = match tool_choice.r#type {
        ToolChoiceType::Auto => "AUTO",
        ToolChoiceType::None => "NONE",
        ToolChoiceType::Required | ToolChoiceType::Tool => "ANY",
    };
    let mut function_calling_config = Map::new();
    function_calling_config.insert("mode".to_string(), Value::String(mode.to_string()));
    if matches!(tool_choice.r#type, ToolChoiceType::Tool) {
        let name = tool_choice
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| shared::invalid_request("Gemini tool choice requires a tool name"))?;
        function_calling_config
            .insert("allowedFunctionNames".to_string(), Value::Array(vec![Value::String(name.to_string())]));
    }
    let mut lowered = Map::new();
    lowered.insert("functionCallingConfig".to_string(), Value::Object(function_calling_config));
    Ok(Value::Object(lowered))
}

fn fit_budget(budget: &Number, max_tokens: Option<&Number>) -> Number {
    let Some(max_tokens) = max_tokens else {
        return budget.clone();
    };
    match (budget.as_i64(), max_tokens.as_i64()) {
        (Some(budget), Some(maximum)) => {
            Number::from(shared::fit_thinking_budget(budget, Some(maximum), MIN_THINKING_BUDGET))
        }
        _ => {
            let value = budget.as_f64().unwrap_or_default();
            let maximum = max_tokens.as_f64().unwrap_or_default();
            if value <= maximum / 2.0 {
                budget.clone()
            } else {
                number_from_f64((MIN_THINKING_BUDGET as f64).max((maximum / 2.0).floor()))
            }
        }
    }
}

fn number_from_f64(value: f64) -> Number {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        return Number::from(value as i64);
    }
    Number::from_f64(value).unwrap_or_else(|| Number::from(0))
}

fn lower_thinking_config(config: &ThinkingConfigInput, max_tokens: Option<&Number>) -> Value {
    let mut lowered = Map::new();
    if let Some(budget) = &config.thinking_budget {
        lowered.insert("thinkingBudget".to_string(), Value::Number(fit_budget(budget, max_tokens)));
    }
    if let Some(include_thoughts) = config.include_thoughts {
        lowered.insert("includeThoughts".to_string(), Value::Bool(include_thoughts));
    }
    if let Some(level) = &config.thinking_level {
        lowered.insert("thinkingLevel".to_string(), Value::String(level.clone()));
    }
    if config.include_thoughts.is_none() {
        lowered.insert("includeThoughts".to_string(), Value::Bool(true));
    }
    Value::Object(lowered)
}

fn generation_config(request: &LlmRequest, options: &Options) -> Option<Value> {
    let generation = &request.generation;
    let mut config = Map::new();
    if let Some(max_tokens) = generation.max_tokens.clone() {
        config.insert("maxOutputTokens".to_string(), Value::Number(max_tokens));
    }
    if let Some(temperature) = generation.temperature.clone() {
        config.insert("temperature".to_string(), Value::Number(temperature));
    }
    if let Some(top_p) = generation.top_p.clone() {
        config.insert("topP".to_string(), Value::Number(top_p));
    }
    if let Some(top_k) = generation.top_k.clone() {
        config.insert("topK".to_string(), Value::Number(top_k));
    }
    if let Some(frequency_penalty) = generation.frequency_penalty.clone() {
        config.insert("frequencyPenalty".to_string(), Value::Number(frequency_penalty));
    }
    if let Some(presence_penalty) = generation.presence_penalty.clone() {
        config.insert("presencePenalty".to_string(), Value::Number(presence_penalty));
    }
    if let Some(seed) = generation.seed.clone() {
        config.insert("seed".to_string(), Value::Number(seed));
    }
    if let Some(stop) = generation.stop.as_ref() {
        config.insert(
            "stopSequences".to_string(),
            Value::Array(stop.iter().map(|value| Value::String(value.clone())).collect()),
        );
    }
    if let Some(thinking) = &options.thinking_config {
        config.insert("thinkingConfig".to_string(), lower_thinking_config(thinking, generation.max_tokens.as_ref()));
    }
    if config.is_empty() {
        return None;
    }
    Some(Value::Object(config))
}

fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let flattened = shared::flatten_tool_request(request);
    let has_tools = !flattened.0.is_empty();
    let options = decode_options(request.provider_options.as_ref());
    let mut body = Map::new();
    if let Some(cached_content) = options.cached_content.clone() {
        body.insert("cachedContent".to_string(), Value::String(cached_content));
    }
    body.insert("contents".to_string(), Value::Array(lower_messages(&flattened.1, model)?));
    if let Some(safety_settings) = options.safety_settings.clone() {
        body.insert("safetySettings".to_string(), Value::Array(safety_settings));
    }
    if let Some(service_tier) = options.service_tier.clone() {
        body.insert("serviceTier".to_string(), Value::String(service_tier));
    }
    if !request.system.is_empty() {
        let parts = request
            .system
            .iter()
            .map(|part| {
                let SystemPart::Text { text, .. } = part;
                text_part(text.clone())
            })
            .collect::<Vec<Value>>();
        let mut instruction = Map::new();
        instruction.insert("parts".to_string(), Value::Array(parts));
        body.insert("systemInstruction".to_string(), Value::Object(instruction));
    }
    if has_tools {
        let mut declarations = Map::new();
        declarations
            .insert("functionDeclarations".to_string(), Value::Array(flattened.0.iter().map(lower_tool).collect()));
        body.insert("tools".to_string(), Value::Array(vec![Value::Object(declarations)]));
    }
    if has_tools && let Some(tool_choice) = request.tool_choice.as_ref() {
        body.insert("toolConfig".to_string(), lower_tool_config(tool_choice)?);
    }
    if let Some(config) = generation_config(request, &options) {
        body.insert("generationConfig".to_string(), config);
    }
    Ok(Value::Object(body))
}

#[derive(Debug, Clone, Default, PartialEq)]
struct GeminiUsage {
    cached_content_token_count: Option<Option<Number>>,
    thoughts_token_count: Option<Option<Number>>,
    prompt_token_count: Option<Option<Number>>,
    tool_use_prompt_token_count: Option<Option<Number>>,
    candidates_token_count: Option<Option<Number>>,
    total_token_count: Option<Option<Number>>,
    cost: Option<Number>,
}

impl GeminiUsage {
    fn decode(value: &Value) -> Option<GeminiUsage> {
        let object = value.as_object()?;
        Some(GeminiUsage {
            cached_content_token_count: decode_token_count(object, "cachedContentTokenCount")?,
            thoughts_token_count: decode_token_count(object, "thoughtsTokenCount")?,
            prompt_token_count: decode_token_count(object, "promptTokenCount")?,
            tool_use_prompt_token_count: decode_token_count(object, "toolUsePromptTokenCount")?,
            candidates_token_count: decode_token_count(object, "candidatesTokenCount")?,
            total_token_count: decode_token_count(object, "totalTokenCount")?,
            cost: object
                .get("cost")
                .and_then(Value::as_number)
                .filter(|cost| cost.as_f64().is_some_and(|cost| cost.is_finite() && cost >= 0.0))
                .cloned(),
        })
    }

    fn encode(&self) -> Map<String, Value> {
        let mut encoded = Map::new();
        let fields = [
            ("cachedContentTokenCount", &self.cached_content_token_count),
            ("thoughtsTokenCount", &self.thoughts_token_count),
            ("promptTokenCount", &self.prompt_token_count),
            ("toolUsePromptTokenCount", &self.tool_use_prompt_token_count),
            ("candidatesTokenCount", &self.candidates_token_count),
            ("totalTokenCount", &self.total_token_count),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                encoded.insert(key.to_string(), option_number(value));
            }
        }
        if let Some(cost) = &self.cost {
            encoded.insert("cost".to_string(), Value::Number(cost.clone()));
        }
        encoded
    }

    fn cached_content_token_count(&self) -> Option<u64> {
        count(&self.cached_content_token_count)
    }

    fn thoughts_token_count(&self) -> Option<u64> {
        count(&self.thoughts_token_count)
    }

    fn prompt_token_count(&self) -> Option<u64> {
        count(&self.prompt_token_count)
    }

    fn tool_use_prompt_token_count(&self) -> Option<u64> {
        count(&self.tool_use_prompt_token_count)
    }

    fn candidates_token_count(&self) -> Option<u64> {
        count(&self.candidates_token_count)
    }

    fn total_token_count(&self) -> Option<u64> {
        count(&self.total_token_count)
    }
}

fn decode_token_count(object: &Map<String, Value>, key: &str) -> Option<Option<Option<Number>>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::Null) => Some(Some(None)),
        Some(Value::Number(number)) => Some(Some(Some(number.clone()))),
        Some(_) => None,
    }
}

fn option_number(value: &Option<Number>) -> Value {
    match value {
        Some(number) => Value::Number(number.clone()),
        None => Value::Null,
    }
}

fn count(value: &Option<Option<Number>>) -> Option<u64> {
    value.as_ref()?.as_ref()?.as_u64()
}

fn provider_metadata(key: &str, value: Value) -> ProviderMetadata {
    let mut metadata = ProviderMetadata::new();
    metadata.insert(key.to_string(), value);
    metadata
}

struct TokenCounts {
    prompt: Option<u64>,
    tool_use_prompt: Option<u64>,
    cached_content: Option<u64>,
    candidates: Option<u64>,
    thoughts: Option<u64>,
    total: Option<u64>,
    cost: Option<Number>,
}

fn build_usage(counts: TokenCounts, metadata: Value, provider_metadata_key: &str) -> Usage {
    let input = shared::sum_tokens(&[counts.prompt, counts.tool_use_prompt]);
    let non_cached = shared::subtract_tokens(input, counts.cached_content);
    let output = counts.candidates.map(|candidates| candidates.saturating_add(counts.thoughts.unwrap_or(0)));
    Usage {
        context_tokens: None,
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: non_cached,
        cache_read_input_tokens: counts.cached_content,
        cache_write_input_tokens: None,
        reasoning_tokens: counts.thoughts,
        total_tokens: shared::total_tokens(input, output, counts.total),
        cost: counts.cost,
        provider_metadata: Some(provider_metadata(provider_metadata_key, metadata)),
    }
}

fn map_usage(usage: Option<&GeminiUsage>, provider_metadata_key: &str) -> Option<Usage> {
    let usage = usage?;
    let metadata = Value::Object(usage.encode());
    let counts = TokenCounts {
        prompt: usage.prompt_token_count(),
        tool_use_prompt: usage.tool_use_prompt_token_count(),
        cached_content: usage.cached_content_token_count(),
        candidates: usage.candidates_token_count(),
        thoughts: usage.thoughts_token_count(),
        total: usage.total_token_count(),
        cost: usage.cost.clone(),
    };
    Some(build_usage(counts, metadata, provider_metadata_key))
}

fn sum_costs(requests: &[GeminiUsage]) -> Option<Number> {
    let costs = requests.iter().filter_map(|usage| usage.cost.as_ref()).collect::<Vec<&Number>>();
    if costs.is_empty() {
        return None;
    }
    let total = costs.iter().map(|cost| cost.as_f64().unwrap_or_default()).sum::<f64>();
    Some(number_from_f64(total))
}

fn map_continued_usage(requests: &[GeminiUsage], provider_metadata_key: &str) -> Option<Usage> {
    let first = requests.first()?;
    let sum = |field: fn(&GeminiUsage) -> Option<u64>| {
        shared::sum_tokens(&requests.iter().map(field).collect::<Vec<Option<u64>>>())
    };
    let candidates = sum(GeminiUsage::candidates_token_count);
    let thoughts = sum(GeminiUsage::thoughts_token_count);
    let mut metadata = Map::new();
    let fields = [
        ("promptTokenCount", sum(GeminiUsage::prompt_token_count)),
        ("toolUsePromptTokenCount", sum(GeminiUsage::tool_use_prompt_token_count)),
        ("cachedContentTokenCount", sum(GeminiUsage::cached_content_token_count)),
        ("candidatesTokenCount", candidates),
        ("thoughtsTokenCount", thoughts),
        ("totalTokenCount", sum(GeminiUsage::total_token_count)),
    ];
    for (key, value) in fields {
        if let Some(value) = value {
            metadata.insert(key.to_string(), Value::Number(Number::from(value)));
        }
    }
    metadata.insert("requests".to_string(), Value::Number(Number::from(requests.len() as u64)));
    let counts = TokenCounts {
        prompt: first.prompt_token_count(),
        tool_use_prompt: first.tool_use_prompt_token_count(),
        cached_content: first.cached_content_token_count(),
        candidates,
        thoughts,
        total: None,
        cost: sum_costs(requests),
    };
    Some(build_usage(counts, Value::Object(metadata), provider_metadata_key))
}

fn terminal_finish_reason(finish_reason: Option<&str>) -> Option<&str> {
    finish_reason.filter(|reason| !reason.is_empty() && *reason != "FINISH_REASON_UNSPECIFIED")
}

fn map_finish_reason(finish_reason: Option<&str>, has_tool_calls: bool) -> FinishReason {
    let Some(finish_reason) = finish_reason else {
        return if has_tool_calls { FinishReason::ToolCalls } else { FinishReason::Unknown };
    };
    match finish_reason {
        "STOP" => {
            if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        }
        "MAX_TOKENS" => FinishReason::Length,
        "MALFORMED_FUNCTION_CALL"
        | "UNEXPECTED_TOOL_CALL"
        | "NO_IMAGE"
        | "TOO_MANY_TOOL_CALLS"
        | "MISSING_THOUGHT_SIGNATURE"
        | "MALFORMED_RESPONSE" => FinishReason::Error,
        reason => {
            if gemini_generate_content::content_filtered(Some(reason)) {
                FinishReason::ContentFilter
            } else {
                FinishReason::Unknown
            }
        }
    }
}

enum Part {
    Text { text: String, thought: Option<bool>, signature: Option<String> },
    InlineData { data: String, mime_type: String, signature: Option<String> },
    FileData,
    FunctionCall { id: Option<String>, name: String, args: Option<Value>, signature: Option<String> },
    FunctionResponse,
}

fn optional_string_field(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn optional_nullable_string_field(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn decode_text_part(input: &Value) -> Option<Part> {
    let text = input.get("text")?.as_str()?.to_string();
    let thought = match input.get("thought") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return None,
    };
    let signature = optional_nullable_string_field(input.as_object()?, "thoughtSignature")?;
    Some(Part::Text { text, thought, signature })
}

fn decode_inline_data(input: &Value) -> Option<Part> {
    let inline_data = input.get("inlineData")?.as_object()?;
    let data = inline_data.get("data")?.as_str()?.to_string();
    let mime_type = inline_data.get("mimeType")?.as_str()?.to_string();
    let signature = optional_nullable_string_field(input.as_object()?, "thoughtSignature")?;
    Some(Part::InlineData { data, mime_type, signature })
}

fn decode_file_data(input: &Value) -> bool {
    input.get("fileData").and_then(Value::as_object).is_some_and(|file| {
        file.get("mimeType").is_some_and(Value::is_string) && file.get("fileUri").is_some_and(Value::is_string)
    })
}

fn decode_function_call(input: &Value) -> Option<Part> {
    let function_call = input.get("functionCall")?.as_object()?;
    let id = optional_nullable_string_field(function_call, "id")?;
    let name = function_call.get("name")?.as_str()?.to_string();
    let args = function_call.get("args").cloned();
    let signature = optional_nullable_string_field(input.as_object()?, "thoughtSignature")?;
    Some(Part::FunctionCall { id, name, args, signature })
}

fn decode_function_response(input: &Value) -> Option<Part> {
    let function_response = input.get("functionResponse")?.as_object()?;
    optional_string_field(function_response, "id")?;
    function_response.get("name")?.as_str()?;
    function_response.get("response")?;
    if let Some(parts) = function_response.get("parts") {
        for part in parts.as_array()? {
            let inline_data = part.get("inlineData")?.as_object()?;
            inline_data.get("mimeType")?.as_str()?;
            inline_data.get("data")?.as_str()?;
            optional_nullable_string_field(part.as_object()?, "thoughtSignature")?;
        }
    }
    Some(Part::FunctionResponse)
}

fn decode_part(input: &Value) -> Option<Part> {
    if input.get("text").is_some_and(Value::is_string)
        && let Some(part) = decode_text_part(input)
    {
        return Some(part);
    }
    if input.get("inlineData").is_some()
        && let Some(part) = decode_inline_data(input)
    {
        return Some(part);
    }
    if decode_file_data(input) {
        return Some(Part::FileData);
    }
    if input.get("functionCall").is_some()
        && let Some(part) = decode_function_call(input)
    {
        return Some(part);
    }
    decode_function_response(input)
}

struct State {
    route: String,
    provider_metadata_key: String,
    finish_reason: Option<String>,
    has_tool_calls: bool,
    prompt_feedback: Option<Value>,
    usage: Option<GeminiUsage>,
    continued_usage: Option<Vec<GeminiUsage>>,
    lifecycle: lifecycle::State,
    reasoning_signature: Option<String>,
    text_signature: Option<String>,
    reasoning_id: Option<String>,
    text_id: Option<String>,
    next_reasoning_id: u64,
    next_text_id: u64,
    seen_call_ids: HashSet<String>,
    continuation_token: Option<String>,
    ids: Arc<dyn IdSource>,
}

impl State {
    fn end_reasoning(&mut self, events: &mut Vec<LlmEvent>) {
        let Some(id) = self.reasoning_id.take() else {
            return;
        };
        let metadata = signature_metadata(&self.provider_metadata_key, self.reasoning_signature.as_deref());
        let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
        self.lifecycle = lifecycle::reasoning_end(state, events, &id, metadata, None);
        self.reasoning_signature = None;
    }

    fn end_text(&mut self, events: &mut Vec<LlmEvent>) {
        let Some(id) = self.text_id.take() else {
            return;
        };
        let metadata = signature_metadata(&self.provider_metadata_key, self.text_signature.as_deref());
        let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
        self.lifecycle = lifecycle::text_end(state, events, &id, metadata, None);
        self.text_signature = None;
    }

    fn track_checkpoint(&mut self, event: &Value) {
        let candidate = event.get("candidates").and_then(Value::as_array).and_then(|candidates| candidates.first());
        let terminal = terminal_finish_reason(
            candidate.and_then(|candidate| candidate.get("finishReason")).and_then(Value::as_str),
        );
        match candidate.and_then(|candidate| candidate.get("continuationToken")).and_then(Value::as_str) {
            Some(token) => self.continuation_token = Some(token.to_string()),
            None if terminal.is_some() => self.continuation_token = None,
            None => {}
        }
    }
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event: Value = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(_) => {
                return Err(shared::event_error(
                    &self.route,
                    format!("Invalid {} stream event", self.route),
                    Some(frame),
                ));
            }
        };
        self.track_checkpoint(&event);
        if let Some(error) = event.get("error").filter(|error| error.is_object()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .or_else(|| error.get("status").and_then(Value::as_str).filter(|status| !status.is_empty()))
                .unwrap_or("Gemini provider error")
                .to_string();
            let status = error.get("code").and_then(Value::as_u64).and_then(|code| u16::try_from(code).ok());
            let reason = classify_provider_failure(ProviderFailure {
                message,
                status,
                raw_body: Some(crate::json::encode_body(&event)),
                ..ProviderFailure::default()
            });
            return Err(AiError { reason });
        }
        let candidate = event.get("candidates").and_then(Value::as_array).and_then(|candidates| candidates.first());
        let finish_reason = terminal_finish_reason(
            candidate.and_then(|candidate| candidate.get("finishReason")).and_then(Value::as_str),
        );
        if let Some(prompt_feedback) = event.get("promptFeedback").filter(|value| !value.is_null()) {
            self.prompt_feedback = Some(prompt_feedback.clone());
        }
        if let Some(value) = event.get("usageMetadata").filter(|value| value.is_object())
            && let Some(usage) = GeminiUsage::decode(value)
        {
            self.usage = Some(usage);
        }
        if let Some(reason) = finish_reason {
            if map_finish_reason(Some(reason), self.has_tool_calls) == FinishReason::Error {
                return Err(AiError {
                    reason: AiErrorReason::InvalidProviderOutput {
                        message: format!("Gemini stopped with {reason}"),
                        body: Some(crate::json::encode_body(&event)),
                        http: None,
                        classification: RECOVERABLE_FINISH_REASONS
                            .contains(&reason)
                            .then_some(InvalidProviderOutputClassification::IncompleteStream),
                        route: Some(self.route.clone()),
                    },
                });
            }
            self.finish_reason = Some(reason.to_string());
        }
        let Some(content) = candidate.and_then(|candidate| candidate.get("content")).filter(|value| value.is_object())
        else {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        let parts = content.get("parts").and_then(Value::as_array);
        for input in parts.into_iter().flatten() {
            let Some(object) = input.as_object() else {
                return Err(self.invalid_event(&event));
            };
            if !object.contains_key("text")
                && !object.contains_key("inlineData")
                && !object.contains_key("functionCall")
                && !object.contains_key("functionResponse")
            {
                continue;
            }
            let Some(part) = decode_part(input) else {
                return Err(self.invalid_event(&event));
            };
            let signature = match &part {
                Part::Text { signature, .. }
                | Part::InlineData { signature, .. }
                | Part::FunctionCall { signature, .. } => signature.clone(),
                Part::FileData | Part::FunctionResponse => None,
            }
            .filter(|signature| !signature.is_empty());
            if let Some(signature) = &signature {
                match &part {
                    Part::Text { thought: Some(true), .. } => self.reasoning_signature = Some(signature.clone()),
                    Part::Text { .. } => self.text_signature = Some(signature.clone()),
                    _ => {}
                }
            }
            match part {
                Part::InlineData { data, mime_type, .. } => {
                    let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
                    self.lifecycle = lifecycle::step_start(state, &mut events);
                    events.push(LlmEvent::Media {
                        media: MediaAsset {
                            source: MediaSource::Base64 { data, media_type: mime_type },
                            info: None,
                            provider_metadata: None,
                        },
                        provider_metadata: signature_metadata(&self.provider_metadata_key, signature.as_deref()),
                    });
                }
                Part::Text { text, thought, .. } if !text.is_empty() => {
                    if thought.unwrap_or(false) {
                        self.end_text(&mut events);
                        if self.reasoning_id.is_none() {
                            self.reasoning_id = Some(format!("reasoning-{}", self.next_reasoning_id));
                            self.next_reasoning_id += 1;
                        }
                        let id = self.reasoning_id.clone().unwrap_or_default();
                        let metadata = signature_metadata(&self.provider_metadata_key, signature.as_deref());
                        let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
                        self.lifecycle = lifecycle::reasoning_delta(state, &mut events, &id, &text, metadata);
                    } else {
                        self.end_reasoning(&mut events);
                        if self.text_id.is_none() {
                            self.text_id = Some(format!("text-{}", self.next_text_id));
                            self.next_text_id += 1;
                        }
                        let id = self.text_id.clone().unwrap_or_default();
                        let metadata = signature_metadata(&self.provider_metadata_key, self.text_signature.as_deref());
                        let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
                        self.lifecycle = lifecycle::text_delta(state, &mut events, &id, &text, metadata);
                        self.text_signature = None;
                    }
                }
                Part::FunctionCall { id, name, args, .. } => {
                    let input = args.unwrap_or_else(|| Value::Object(Map::new()));
                    let duplicate = id.as_ref().is_some_and(|id| self.seen_call_ids.contains(id));
                    if let Some(id) = &id {
                        self.seen_call_ids.insert(id.clone());
                    }
                    let id = match id {
                        Some(id) if !duplicate => id,
                        _ => format!("tool_{}", self.ids.uuid().replace('-', "")),
                    };
                    self.end_reasoning(&mut events);
                    self.end_text(&mut events);
                    let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
                    self.lifecycle = lifecycle::step_start(state, &mut events);
                    events.push(LlmEvent::ToolCall {
                        id,
                        name,
                        namespace: None,
                        input,
                        provider_executed: None,
                        provider_metadata: signature_metadata(&self.provider_metadata_key, signature.as_deref()),
                    });
                    self.has_tool_calls = true;
                }
                Part::Text { .. } | Part::FileData | Part::FunctionResponse => {}
            }
        }
        Ok(events)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let prompt_block_reason = if self.finish_reason.is_none() {
            self.prompt_feedback
                .as_ref()
                .and_then(|feedback| feedback.get("blockReason"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let finish_reason = self.finish_reason.clone().or_else(|| prompt_block_reason.clone());
        if finish_reason.is_none() && self.usage.is_none() && self.continued_usage.is_none() {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        self.end_reasoning(&mut events);
        self.end_text(&mut events);
        let normalized = match &prompt_block_reason {
            Some(_) => FinishReason::ContentFilter,
            None => map_finish_reason(finish_reason.as_deref(), self.has_tool_calls),
        };
        let reason = FinishReasonDetails { normalized, raw: finish_reason, category: None, explanation: None };
        let usage = match &self.continued_usage {
            None => map_usage(self.usage.as_ref(), &self.provider_metadata_key),
            Some(continued) => {
                let mut requests = continued.clone();
                requests.push(self.usage.clone().unwrap_or_default());
                map_continued_usage(&requests, &self.provider_metadata_key)
            }
        };
        let metadata = self.prompt_feedback.as_ref().map(|feedback| {
            provider_metadata(&self.provider_metadata_key, {
                let mut value = Map::new();
                value.insert("promptFeedback".to_string(), feedback.clone());
                Value::Object(value)
            })
        });
        let state = std::mem::replace(&mut self.lifecycle, lifecycle::initial());
        self.lifecycle = lifecycle::finish(state, &mut events, reason, usage, metadata);
        Ok(events)
    }

    fn terminal(&self, _frame: &Frame) -> bool {
        false
    }

    fn continuation(&mut self) -> Option<Result<String, AiError>> {
        let token = self.continuation_token.clone();
        let continuing = match &token {
            Some(_) => self.finish_reason.is_none() || self.finish_reason.as_deref() == Some(CONTINUATION),
            None => false,
        };
        if continuing {
            self.continued_usage.get_or_insert_with(Vec::new).push(self.usage.clone().unwrap_or_default());
            self.finish_reason = None;
            self.prompt_feedback = None;
            self.usage = None;
            self.continuation_token = None;
            return token.map(Ok);
        }
        if self.finish_reason.as_deref() == Some(CONTINUATION) {
            return Some(Err(shared::event_error(&self.route, "Gemini continued the response without a token", None)));
        }
        None
    }
}

impl State {
    fn invalid_event(&self, event: &Value) -> AiError {
        shared::event_error(ID, format!("Invalid {} stream event", self.route), Some(crate::json::encode_body(event)))
    }
}

impl Protocol for Gemini {
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

    fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
        Some(format!("/models/{}:streamGenerateContent?alt=sse", model.id))
    }

    fn framing(&self) -> Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        let model = &request.model;
        Box::new(State {
            route: format!("{}/{}", model.provider, model.route.id),
            provider_metadata_key: model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone()),
            finish_reason: None,
            has_tool_calls: false,
            prompt_feedback: None,
            usage: None,
            continued_usage: None,
            lifecycle: lifecycle::initial(),
            reasoning_signature: None,
            text_signature: None,
            reasoning_id: None,
            text_id: None,
            next_reasoning_id: 0,
            next_text_id: 0,
            seen_call_ids: HashSet::new(),
            continuation_token: None,
            ids: context.ids.clone(),
        })
    }

    fn sanitizer(&self) -> Option<SanitizerKind> {
        Some(SanitizerKind::Gemini)
    }
}
