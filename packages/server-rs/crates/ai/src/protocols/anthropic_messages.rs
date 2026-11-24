use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, LazyLock};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use regex_lite::Regex;
use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::prepare::tool_updates::{PlacedGuard, ToolUpdateEncoding, ToolUpdateKind};
use crate::prepare::{
    adds_tools, effort_update, flatten_tools, fold_tools, resolve_effort_updates, tools_update, withdrawn_tools,
};
use crate::protocols::shared;
use crate::protocols::utils::cache::CacheControl;
use crate::protocols::utils::claude_model::{self, THINKING_BINDING_BETA};
use crate::protocols::utils::lifecycle;
use crate::protocols::utils::tool_stream;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::{self, Definition};
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, CacheHint, CompactionPart, ContentPart, FinishReason, FinishReasonDetails, LlmEvent, LlmRequest,
    MediaAsset, MediaPart, MediaSource, Message, ProviderMetadata, SystemPart, TextPart, ToolCallPart, ToolChanges,
    ToolChoice, ToolChoiceType, ToolContent, ToolDefinition, ToolEntry, ToolResultPart, ToolResultValue, Usage,
};

pub const ID: &str = "anthropic-messages";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
pub const PATH: &str = "/messages";
pub const DEFAULT_MAX_TOKENS: i64 = 32_000;

const ROUTE: &str = "Anthropic Messages";
const MIN_THINKING_BUDGET: i64 = 1_024;
const DEFAULT_EFFORT: &str = "high";

const SSE_EVENTS: [&str; 9] = [
    "message",
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "ping",
    "error",
];

const STREAM_BLOCK_TYPES: [&str; 6] =
    ["compaction", "text", "thinking", "redacted_thinking", "tool_use", "server_tool_use"];
const STREAM_DELTA_TYPES: [&str; 5] =
    ["compaction_delta", "text_delta", "thinking_delta", "signature_delta", "input_json_delta"];

#[derive(Default)]
pub struct AnthropicMessages {
    pub effort_updates_require_5_1: bool,
}

#[derive(Default)]
pub struct AnthropicMessagesPlain(pub AnthropicMessages);

impl Protocol for AnthropicMessagesPlain {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        self.0.build_body(request, model, context)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        self.0.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        self.0.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        self.0.new_state(request, context)
    }

    fn supports_effort_updates(&self, model: &Model, request: &LlmRequest) -> bool {
        self.0.supports_effort_updates(model, request)
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        self.0.tool_updates()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct BlockBinding {
    prefix_mismatch_behavior: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Thinking {
    Enabled { budget_tokens: Number, display: Option<String>, block_binding: Option<BlockBinding> },
    Adaptive { display: Option<String>, block_binding: Option<BlockBinding> },
    Off { kind: String },
}

impl Thinking {
    fn is_off(&self) -> bool {
        matches!(self, Thinking::Off { .. })
    }

    fn encode(&self) -> Map<String, Value> {
        let mut encoded = Map::new();
        match self {
            Thinking::Enabled { budget_tokens, display, block_binding } => {
                encoded.insert("type".to_string(), Value::String("enabled".to_string()));
                encoded.insert("budget_tokens".to_string(), Value::Number(budget_tokens.clone()));
                push_optional_string(&mut encoded, "display", display);
                push_block_binding(&mut encoded, block_binding);
            }
            Thinking::Adaptive { display, block_binding } => {
                encoded.insert("type".to_string(), Value::String("adaptive".to_string()));
                push_optional_string(&mut encoded, "display", display);
                push_block_binding(&mut encoded, block_binding);
            }
            Thinking::Off { kind } => {
                encoded.insert("type".to_string(), Value::String(kind.clone()));
            }
        }
        encoded
    }
}

#[derive(Debug, Clone, Default)]
struct OutputConfigInput {
    effort: Option<String>,
    format: Option<Value>,
}

#[derive(Debug, Clone)]
struct ContextManagementEdit {
    trigger: Option<Value>,
    pause_after_compaction: Option<bool>,
    instructions: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ContextManagement {
    edits: Vec<ContextManagementEdit>,
}

#[derive(Debug, Clone, Default)]
struct Options {
    context_management: Option<ContextManagement>,
    thinking: Option<Thinking>,
    effort: Option<String>,
    service_tier: Option<String>,
    speed: Option<String>,
    metadata: Option<Value>,
    container: Option<Value>,
    inference_geo: Option<String>,
    cache_control: Option<Value>,
    output_config: Option<OutputConfigInput>,
}

#[derive(Debug, Clone, Default)]
struct MediaOptions {
    file_id: Option<String>,
    title: Option<String>,
    context: Option<String>,
    citations: Option<Value>,
    transformations: Option<Value>,
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let options = decode_options(request.provider_options.as_ref())?;
    let management = options.context_management.as_ref();
    let format = options.output_config.as_ref().and_then(|config| config.format.clone());
    let current =
        options.effort.clone().or_else(|| options.output_config.as_ref().and_then(|config| config.effort.clone()));
    let (updated, effort) = resolve_effort_updates(request, current.as_deref());
    let mut breakpoints = CacheControl::new();
    let flattened = shared::flatten_tool_request(&updated);
    let tools = if flattened.0.is_empty() {
        None
    } else {
        Some(flattened.0.iter().map(|tool| lower_tool(&mut breakpoints, tool)).collect::<Vec<Value>>())
    };
    let declared = tools.is_some() || adds_tools(request);
    let tool_choice = match request.tool_choice.as_ref().filter(|_| declared) {
        Some(choice) => Some(lower_tool_choice(choice)?),
        None => None,
    };
    let system = lower_system(request, &mut breakpoints);
    let messages = lower_messages(&flattened.1, model, &mut breakpoints)?;
    let output_config = if effort.is_none() && format.is_none() {
        None
    } else {
        let mut config = Map::new();
        push_optional_string(&mut config, "effort", &effort);
        if let Some(format) = format {
            config.insert("format".to_string(), format);
        }
        Some(Value::Object(config))
    };
    let max_tokens = request.generation.max_tokens.clone().unwrap_or_else(|| Number::from(DEFAULT_MAX_TOKENS));
    let thinking = apply_thinking_binding_default(model, fit_thinking(&options.thinking, &max_tokens));
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    if let Some(system) = system {
        body.insert("system".to_string(), Value::Array(system));
    }
    body.insert("messages".to_string(), Value::Array(messages));
    if let Some(tools) = tools {
        body.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(tool_choice) = tool_choice {
        body.insert("tool_choice".to_string(), tool_choice);
    }
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert("max_tokens".to_string(), Value::Number(max_tokens));
    push_optional_number(&mut body, "temperature", request.generation.temperature.as_ref());
    push_optional_number(&mut body, "top_p", request.generation.top_p.as_ref());
    push_optional_number(&mut body, "top_k", request.generation.top_k.as_ref());
    if let Some(stop) = request.generation.stop.as_ref() {
        body.insert("stop_sequences".to_string(), Value::Array(stop.iter().cloned().map(Value::String).collect()));
    }
    if let Some(thinking) = thinking {
        body.insert("thinking".to_string(), Value::Object(thinking.encode()));
    }
    if let Some(output_config) = output_config {
        body.insert("output_config".to_string(), output_config);
    }
    if let Some(cache_control) = options.cache_control {
        body.insert("cache_control".to_string(), cache_control);
    }
    if let Some(container) = options.container {
        body.insert("container".to_string(), container);
    }
    if let Some(inference_geo) = options.inference_geo {
        body.insert("inference_geo".to_string(), Value::String(inference_geo));
    }
    if let Some(metadata) = options.metadata {
        body.insert("metadata".to_string(), metadata);
    }
    if let Some(service_tier) = options.service_tier {
        body.insert("service_tier".to_string(), Value::String(service_tier));
    }
    if let Some(speed) = options.speed {
        body.insert("speed".to_string(), Value::String(speed));
    }
    if let Some(management) = management {
        let edits = management.edits.iter().map(encode_context_management_edit).collect::<Vec<Value>>();
        let mut context_management = Map::new();
        context_management.insert("edits".to_string(), Value::Array(edits));
        body.insert("context_management".to_string(), Value::Object(context_management));
    }
    Ok(Value::Object(body))
}

fn lower_system(request: &LlmRequest, breakpoints: &mut CacheControl) -> Option<Vec<Value>> {
    let parts = request
        .system
        .iter()
        .map(|part| match part {
            SystemPart::Text { text, cache, .. } => (text, cache),
        })
        .filter(|(text, _)| !text.is_empty())
        .collect::<Vec<(&String, &Option<CacheHint>)>>();
    if parts.is_empty() {
        return None;
    }
    let system = parts
        .into_iter()
        .map(|(text, cache)| {
            let mut block = Map::new();
            block.insert("type".to_string(), Value::String("text".to_string()));
            block.insert("text".to_string(), Value::String(text.clone()));
            if let Some(cache_control) = cache_control(breakpoints, cache.as_ref()) {
                block.insert("cache_control".to_string(), cache_control);
            }
            Value::Object(block)
        })
        .collect();
    Some(system)
}

fn encode_context_management_edit(edit: &ContextManagementEdit) -> Value {
    let mut encoded = Map::new();
    encoded.insert("type".to_string(), Value::String("compact_20260112".to_string()));
    if let Some(trigger) = edit.trigger.as_ref() {
        encoded.insert("trigger".to_string(), trigger.clone());
    }
    if let Some(pause) = edit.pause_after_compaction {
        encoded.insert("pause_after_compaction".to_string(), Value::Bool(pause));
    }
    if let Some(instructions) = edit.instructions.as_ref() {
        encoded.insert("instructions".to_string(), Value::String(instructions.clone()));
    }
    Value::Object(encoded)
}

fn lower_tool(breakpoints: &mut CacheControl, tool: &ToolDefinition) -> Value {
    let mut encoded = Map::new();
    encoded.insert("name".to_string(), Value::String(tool.name.clone()));
    encoded.insert("description".to_string(), Value::String(tool.description.clone()));
    encoded.insert("input_schema".to_string(), tool.input_schema.clone());
    if let Some(cache_control) = cache_control(breakpoints, tool.cache.as_ref()) {
        encoded.insert("cache_control".to_string(), cache_control);
    }
    Value::Object(encoded)
}

fn lower_tool_choice(tool_choice: &ToolChoice) -> Result<Value, AiError> {
    let mut encoded = Map::new();
    match tool_choice.r#type {
        ToolChoiceType::Auto => {
            encoded.insert("type".to_string(), Value::String("auto".to_string()));
            push_disable_parallel(&mut encoded, tool_choice.disable_parallel_tool_use);
        }
        ToolChoiceType::None => {
            encoded.insert("type".to_string(), Value::String("none".to_string()));
        }
        ToolChoiceType::Required => {
            encoded.insert("type".to_string(), Value::String("any".to_string()));
            push_disable_parallel(&mut encoded, tool_choice.disable_parallel_tool_use);
        }
        ToolChoiceType::Tool => {
            let name = tool_choice
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| shared::invalid_request(format!("{ROUTE} tool choice requires a tool name")))?;
            encoded.insert("type".to_string(), Value::String("tool".to_string()));
            encoded.insert("name".to_string(), Value::String(name.to_string()));
            push_disable_parallel(&mut encoded, tool_choice.disable_parallel_tool_use);
        }
    }
    Ok(Value::Object(encoded))
}

fn push_disable_parallel(encoded: &mut Map<String, Value>, disabled: Option<bool>) {
    if let Some(disabled) = disabled {
        encoded.insert("disable_parallel_tool_use".to_string(), Value::Bool(disabled));
    }
}

fn cache_control(breakpoints: &mut CacheControl, cache: Option<&CacheHint>) -> Option<Value> {
    breakpoints.take(cache)
}

fn scrub_tool_call_id(id: &str) -> String {
    id.chars()
        .flat_map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                return vec![character];
            }
            vec!['_'; character.len_utf16()]
        })
        .collect()
}

fn lower_tool_call(part: &ToolCallPart, executed: bool) -> Value {
    let mut encoded = Map::new();
    encoded
        .insert("type".to_string(), Value::String(if executed { "server_tool_use" } else { "tool_use" }.to_string()));
    encoded.insert("id".to_string(), Value::String(scrub_tool_call_id(&part.id)));
    encoded.insert("name".to_string(), Value::String(part.name.clone()));
    encoded.insert("input".to_string(), part.input.clone());
    Value::Object(encoded)
}

fn server_tool_result_type(name: &str) -> Option<&'static str> {
    match name {
        "web_search" => Some("web_search_tool_result"),
        "code_execution" => Some("code_execution_tool_result"),
        "web_fetch" => Some("web_fetch_tool_result"),
        _ => None,
    }
}

fn server_tool_result_name(value: &str) -> Option<&'static str> {
    match value {
        "web_search_tool_result" => Some("web_search"),
        "code_execution_tool_result" => Some("code_execution"),
        "web_fetch_tool_result" => Some("web_fetch"),
        _ => None,
    }
}

fn lower_server_tool_result(part: &ToolResultPart, provider_metadata_key: &str) -> Result<Value, AiError> {
    let Some(wire_type) = server_tool_result_type(&part.name) else {
        return Err(shared::invalid_request(format!(
            "{ROUTE} does not know how to round-trip server tool result for {}",
            part.name
        )));
    };
    let payload = part
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(provider_metadata_key))
        .and_then(|metadata| metadata.get("result"))
        .filter(|result| !result.is_null())
        .cloned()
        .unwrap_or_else(|| result_value(&part.result));
    let mut encoded = Map::new();
    encoded.insert("type".to_string(), Value::String(wire_type.to_string()));
    encoded.insert("tool_use_id".to_string(), Value::String(scrub_tool_call_id(&part.id)));
    encoded.insert("content".to_string(), payload);
    Ok(Value::Object(encoded))
}

fn result_value(result: &ToolResultValue) -> Value {
    match result {
        ToolResultValue::Json { value } | ToolResultValue::Text { value } | ToolResultValue::Error { value } => {
            value.clone()
        }
        ToolResultValue::Content { value } => {
            Value::Array(value.iter().map(|item| serde_json::to_value(item).unwrap_or(Value::Null)).collect())
        }
    }
}

fn is_http_url(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("http://") || value.starts_with("https://")
}

fn lower_media(part: &MediaPart, model: &Model, mut breakpoints: Option<&mut CacheControl>) -> Result<Value, AiError> {
    let media_type = shared::media_type(&part.media);
    let mime = media_type.to_lowercase();
    let cache_control = breakpoints.as_mut().and_then(|breakpoints| cache_control(breakpoints, part.cache.as_ref()));
    if mime.starts_with("video/") {
        let source = match shared::inline_media(&part.media) {
            Some(inline) => base64_source(&mime, &inline.base64),
            None => video_reference(&part.media, &model.provider)?,
        };
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("video".to_string()));
        encoded.insert("source".to_string(), source);
        if let Some(cache_control) = cache_control {
            encoded.insert("cache_control".to_string(), cache_control);
        }
        return Ok(Value::Object(encoded));
    }
    let options = media_options(part, model)?;
    let transformations = options.transformations.clone();
    let image = |source: Value, cache_control: Option<Value>, transformations: Option<Value>| {
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("image".to_string()));
        encoded.insert("source".to_string(), source);
        if let Some(cache_control) = cache_control {
            encoded.insert("cache_control".to_string(), cache_control);
        }
        if let Some(transformations) = transformations {
            encoded.insert("transformations".to_string(), transformations);
        }
        Value::Object(encoded)
    };
    let document = |source: Value, cache_control: Option<Value>, options: &MediaOptions, part: &MediaPart| {
        let title = options.title.clone().or_else(|| part.filename.clone().filter(|filename| !filename.is_empty()));
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("document".to_string()));
        encoded.insert("source".to_string(), source);
        if let Some(cache_control) = cache_control {
            encoded.insert("cache_control".to_string(), cache_control);
        }
        if let Some(title) = title {
            encoded.insert("title".to_string(), Value::String(title));
        }
        if let Some(context) = options.context.clone() {
            encoded.insert("context".to_string(), Value::String(context));
        }
        if let Some(citations) = options.citations.clone() {
            encoded.insert("citations".to_string(), citations);
        }
        Value::Object(encoded)
    };
    if let Some(file_id) = options.file_id.clone().filter(|file_id| !file_id.is_empty()) {
        let mut source = Map::new();
        source.insert("type".to_string(), Value::String("file".to_string()));
        source.insert("file_id".to_string(), Value::String(file_id));
        let source = Value::Object(source);
        if mime.starts_with("image/") {
            return Ok(image(source, cache_control, transformations));
        }
        return Ok(document(source, cache_control, &options, part));
    }
    if let Some(url) = shared::media_url(&part.media).map(str::trim).filter(|url| !url.is_empty() && is_http_url(url)) {
        let mut source = Map::new();
        source.insert("type".to_string(), Value::String("url".to_string()));
        source.insert("url".to_string(), Value::String(url.to_string()));
        let source = Value::Object(source);
        if mime.starts_with("image/") {
            return Ok(image(source, cache_control, transformations));
        }
        if mime == "application/pdf" {
            return Ok(document(source, cache_control, &options, part));
        }
    }
    let inline = shared::require_inline_media(ROUTE, &part.media)?;
    if mime == "text/plain" {
        let data = BASE64_STANDARD
            .decode(&inline.base64)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        let mut source = Map::new();
        source.insert("type".to_string(), Value::String("text".to_string()));
        source.insert("media_type".to_string(), Value::String("text/plain".to_string()));
        source.insert("data".to_string(), Value::String(data));
        return Ok(document(Value::Object(source), cache_control, &options, part));
    }
    if inline.mime == "application/pdf" {
        return Ok(document(base64_source("application/pdf", &inline.base64), cache_control, &options, part));
    }
    if !inline.mime.starts_with("image/") {
        return Err(shared::invalid_request(format!("{ROUTE} does not support media type {media_type}")));
    }
    Ok(image(base64_source(&inline.mime, &inline.base64), cache_control, transformations))
}

fn base64_source(media_type: &str, data: &str) -> Value {
    let mut source = Map::new();
    source.insert("type".to_string(), Value::String("base64".to_string()));
    source.insert("media_type".to_string(), Value::String(media_type.to_string()));
    source.insert("data".to_string(), Value::String(data.to_string()));
    Value::Object(source)
}

fn video_reference(asset: &MediaAsset, provider: &str) -> Result<Value, AiError> {
    if let Some(url) = shared::media_url(asset) {
        let mut source = Map::new();
        source.insert("type".to_string(), Value::String("url".to_string()));
        source.insert("url".to_string(), Value::String(url.to_string()));
        return Ok(Value::Object(source));
    }
    if let MediaSource::Ref { provider: source_provider, id, .. } = &asset.source
        && source_provider == provider
    {
        if id.contains("://") {
            let mut source = Map::new();
            source.insert("type".to_string(), Value::String("url".to_string()));
            source.insert("url".to_string(), Value::String(id.clone()));
            return Ok(Value::Object(source));
        }
        let mut source = Map::new();
        source.insert("type".to_string(), Value::String("file".to_string()));
        source.insert("file_id".to_string(), Value::String(id.clone()));
        return Ok(Value::Object(source));
    }
    let got = match &asset.source {
        MediaSource::Ref { provider, id, .. } => format!("; got {provider}:{id}"),
        _ => String::new(),
    };
    Err(shared::invalid_request(format!(
        "Messages video accepts inline bytes, data URLs, http(s) URLs, and {provider} references{got}"
    )))
}

fn media_options(part: &MediaPart, model: &Model) -> Result<MediaOptions, AiError> {
    let key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let input = part.provider_metadata.as_ref().and_then(|metadata| metadata.get(&key)).cloned();
    decode_media_options(input.as_ref())
}

fn lower_tool_result_content_item(item: &ToolContent, model: &Model) -> Result<Value, AiError> {
    if let ToolContent::Text { text } = item {
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("text".to_string()));
        encoded.insert("text".to_string(), Value::String(text.clone()));
        return Ok(Value::Object(encoded));
    }
    let Some(media) = shared::tool_file_media(item) else {
        return Err(shared::invalid_request(format!("{ROUTE} does not support tool result content")));
    };
    lower_media(&media, model, None)
}

fn lower_tool_result_content(part: &ToolResultPart, model: &Model) -> Result<Value, AiError> {
    let ToolResultValue::Content { value } = &part.result else {
        return Ok(Value::String(shared::tool_result_text(part)));
    };
    let content = value
        .iter()
        .map(|item| lower_tool_result_content_item(item, model))
        .collect::<Result<Vec<Value>, AiError>>()?;
    Ok(Value::Array(content))
}

fn provider_metadata(key: &str, metadata: Map<String, Value>) -> ProviderMetadata {
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(key.to_string(), Value::Object(metadata));
    provider_metadata
}

fn signature_from_metadata(metadata: Option<&ProviderMetadata>, key: &str) -> Option<String> {
    let provider = metadata?.get(key).filter(|value| !value.is_null())?;
    provider.get("signature").and_then(Value::as_str).map(str::to_string)
}

fn redacted_data_from_metadata(metadata: Option<&ProviderMetadata>, key: &str) -> Option<String> {
    let provider = metadata?.get(key).filter(|value| !value.is_null())?;
    provider.get("redactedData").and_then(Value::as_str).map(str::to_string)
}

fn require_thinking_signature(model: &Model) -> bool {
    if let Some(value) = model
        .compatibility
        .as_ref()
        .and_then(|compatibility| compatibility.get("requireSignature"))
        .and_then(Value::as_bool)
    {
        return value;
    }
    let provider = model.provider.to_lowercase();
    let model_id = model.id.to_lowercase();
    let base_url = model.route.endpoint.base_url.as_deref().unwrap_or_default().to_lowercase();
    if provider == "kimi-for-coding"
        || provider == "moonshotai"
        || provider == "moonshotai-cn"
        || model_id.starts_with("kimi-")
        || base_url.contains("api.kimi.com/coding")
        || base_url.contains("api.moonshot.ai/anthropic")
        || base_url.contains("api.moonshot.cn/anthropic")
    {
        return false;
    }
    if provider.contains("xiaomi") || model_id.contains("mimo") || base_url.contains("xiaomimimo.com") {
        return false;
    }
    if model_id.starts_with("qwen") || model_id.starts_with("minimax-") {
        return false;
    }
    true
}

fn supports_native_system_updates(model: &Model) -> bool {
    if let Some(value) =
        model.capabilities.as_ref().and_then(|capabilities| capabilities.get("systemUpdates")).and_then(Value::as_bool)
    {
        return value;
    }
    let Some(version) = claude_model::claude_version(&model.id) else {
        return false;
    };
    if version.family == "opus" && version.major == 4 {
        return version.minor >= 8;
    }
    version.major >= 5
}

fn model_supports_effort_updates(model: &Model, thinking_off: bool) -> bool {
    if thinking_off {
        return false;
    }
    if let Some(value) = model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.get("reasoning"))
        .and_then(|reasoning| reasoning.get("updates"))
        .and_then(Value::as_bool)
    {
        return value;
    }
    let Some(version) = claude_model::claude_version(&model.id) else {
        return false;
    };
    if version.family == "opus" && version.major >= 5 {
        return true;
    }
    version.major > 5 || (version.major == 5 && version.minor >= 1)
}

fn is_thinking_off(thinking: Option<&Thinking>) -> bool {
    thinking.is_some_and(Thinking::is_off)
}

fn apply_thinking_binding_default(model: &Model, thinking: Option<Thinking>) -> Option<Thinking> {
    if is_thinking_off(thinking.as_ref()) {
        return thinking;
    }
    if !claude_model::supports_thinking_block_binding(model) {
        return thinking;
    }
    match thinking {
        Some(Thinking::Enabled { budget_tokens, display, block_binding }) => {
            Some(Thinking::Enabled { budget_tokens, display, block_binding: Some(binding(block_binding)) })
        }
        Some(Thinking::Adaptive { display, block_binding }) => {
            Some(Thinking::Adaptive { display, block_binding: Some(binding(block_binding)) })
        }
        Some(Thinking::Off { kind }) => Some(Thinking::Off { kind }),
        None => Some(Thinking::Adaptive { display: None, block_binding: Some(binding(None)) }),
    }
}

fn binding(existing: Option<BlockBinding>) -> BlockBinding {
    let prefix_mismatch_behavior =
        existing.and_then(|binding| binding.prefix_mismatch_behavior).unwrap_or_else(|| "drop_block".to_string());
    BlockBinding { prefix_mismatch_behavior: Some(prefix_mismatch_behavior) }
}

fn fit_thinking(thinking: &Option<Thinking>, max_tokens: &Number) -> Option<Thinking> {
    let Some(Thinking::Enabled { budget_tokens, display, block_binding }) = thinking else {
        return thinking.clone();
    };
    Some(Thinking::Enabled {
        budget_tokens: shared::fit_thinking_budget(budget_tokens, Some(max_tokens), MIN_THINKING_BUDGET),
        display: display.clone(),
        block_binding: block_binding.clone(),
    })
}

fn lower_messages(request: &LlmRequest, model: &Model, breakpoints: &mut CacheControl) -> Result<Vec<Value>, AiError> {
    let mut messages: Vec<Value> = Vec::new();
    let provider_metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let hold_updates = supports_native_system_updates(model);
    let mut held: Vec<&Message> = Vec::new();
    let mut declared = request.tools.clone();
    for (index, message) in request.messages.iter().enumerate() {
        if let Message::System(_) = message {
            if let Some(update) = effort_update(message) {
                let mut output_config = Map::new();
                output_config.insert(
                    "effort".to_string(),
                    Value::String(update.effort.clone().unwrap_or_else(|| DEFAULT_EFFORT.to_string())),
                );
                let mut encoded = Map::new();
                encoded.insert("role".to_string(), Value::String("system".to_string()));
                encoded.insert("content".to_string(), Value::Array(Vec::new()));
                encoded.insert("output_config".to_string(), Value::Object(output_config));
                messages.push(Value::Object(encoded));
                continue;
            }
            if tools_update(message).is_some() {
                held.push(message);
                continue;
            }
            if splits_local_tool_results(&request.messages, index) {
                return Err(shared::invalid_request(format!(
                    "{ROUTE} system updates cannot split a local tool call from its tool result"
                )));
            }
            if hold_updates {
                held.push(message);
                continue;
            }
            let block = lower_wrapped_system_update(message, breakpoints)?;
            append_to_user_turn(&mut messages, block);
            continue;
        }
        if let Message::User(user) = message {
            let mut content: Vec<Value> = Vec::new();
            for part in &user.content {
                match part {
                    ContentPart::Text(part) => {
                        if part.text.trim().is_empty() {
                            continue;
                        }
                        let mut encoded = Map::new();
                        encoded.insert("type".to_string(), Value::String("text".to_string()));
                        encoded.insert("text".to_string(), Value::String(part.text.clone()));
                        if let Some(cache_control) = cache_control(breakpoints, part.cache.as_ref()) {
                            encoded.insert("cache_control".to_string(), cache_control);
                        }
                        content.push(Value::Object(encoded));
                    }
                    ContentPart::Media(part) => {
                        content.push(lower_media(part, model, Some(&mut *breakpoints))?);
                    }
                    _ => {}
                }
            }
            if !content.is_empty() {
                let mut encoded = Map::new();
                encoded.insert("role".to_string(), Value::String("user".to_string()));
                encoded.insert("content".to_string(), Value::Array(content));
                messages.push(Value::Object(encoded));
            }
            continue;
        }
        if let Message::Assistant(assistant) = message {
            let mut content: Vec<Value> = Vec::new();
            for part in &assistant.content {
                match part {
                    ContentPart::Compaction(part) => {
                        let Some(text) = part.text.as_ref() else {
                            return Err(shared::invalid_request(
                                "Compaction state must be replayed to its originating provider and API",
                            ));
                        };
                        if part.provider != model.provider {
                            return Err(shared::invalid_request(
                                "Compaction state must be replayed to its originating provider and API",
                            ));
                        }
                        let mut encoded = Map::new();
                        encoded.insert("type".to_string(), Value::String("compaction".to_string()));
                        encoded.insert("content".to_string(), text.clone().map(Value::String).unwrap_or(Value::Null));
                        content.push(Value::Object(encoded));
                    }
                    ContentPart::Text(part) => {
                        if part.text.trim().is_empty() {
                            continue;
                        }
                        let mut encoded = Map::new();
                        encoded.insert("type".to_string(), Value::String("text".to_string()));
                        encoded.insert("text".to_string(), Value::String(part.text.clone()));
                        if let Some(cache_control) = cache_control(breakpoints, part.cache.as_ref()) {
                            encoded.insert("cache_control".to_string(), cache_control);
                        }
                        content.push(Value::Object(encoded));
                    }
                    ContentPart::Reasoning(part) => {
                        let signature = part.encrypted.clone().or_else(|| {
                            signature_from_metadata(part.provider_metadata.as_ref(), &provider_metadata_key)
                        });
                        let redacted_data =
                            redacted_data_from_metadata(part.provider_metadata.as_ref(), &provider_metadata_key);
                        if signature.is_none()
                            && let Some(redacted_data) = redacted_data
                        {
                            let mut encoded = Map::new();
                            encoded.insert("type".to_string(), Value::String("redacted_thinking".to_string()));
                            encoded.insert("data".to_string(), Value::String(redacted_data));
                            content.push(Value::Object(encoded));
                            continue;
                        }
                        let Some(signature) = signature.filter(|signature| !signature.trim().is_empty()) else {
                            if !part.text.trim().is_empty() && !require_thinking_signature(model) {
                                let mut encoded = Map::new();
                                encoded.insert("type".to_string(), Value::String("thinking".to_string()));
                                encoded.insert("thinking".to_string(), Value::String(part.text.clone()));
                                encoded.insert("signature".to_string(), Value::String(String::new()));
                                content.push(Value::Object(encoded));
                            }
                            continue;
                        };
                        let mut encoded = Map::new();
                        encoded.insert("type".to_string(), Value::String("thinking".to_string()));
                        encoded.insert("thinking".to_string(), Value::String(part.text.clone()));
                        encoded.insert("signature".to_string(), Value::String(signature));
                        content.push(Value::Object(encoded));
                    }
                    ContentPart::ToolCall(part) => {
                        content.push(lower_tool_call(part, part.provider_executed == Some(true)));
                    }
                    ContentPart::ToolResult(part) if part.provider_executed == Some(true) => {
                        content.push(lower_server_tool_result(part, &provider_metadata_key)?);
                    }
                    _ => {
                        return Err(shared::invalid_request(format!(
                            "{ROUTE} assistant messages only support text, reasoning, and tool-call content for now"
                        )));
                    }
                }
            }
            if content.is_empty() {
                continue;
            }
            release_held(&mut messages, &mut held, &mut declared, breakpoints)?;
            let mut encoded = Map::new();
            encoded.insert("role".to_string(), Value::String("assistant".to_string()));
            encoded.insert("content".to_string(), Value::Array(content));
            messages.push(Value::Object(encoded));
            continue;
        }
        let Message::Tool(tool) = message else {
            continue;
        };
        let mut content: Vec<Value> = Vec::new();
        for part in &tool.content {
            let ContentPart::ToolResult(part) = part else {
                continue;
            };
            let mut encoded = Map::new();
            encoded.insert("type".to_string(), Value::String("tool_result".to_string()));
            encoded.insert("tool_use_id".to_string(), Value::String(scrub_tool_call_id(&part.id)));
            encoded.insert("content".to_string(), lower_tool_result_content(part, model)?);
            if matches!(part.result, ToolResultValue::Error { .. }) {
                encoded.insert("is_error".to_string(), Value::Bool(true));
            }
            if let Some(cache_control) = cache_control(breakpoints, part.cache.as_ref()) {
                encoded.insert("cache_control".to_string(), cache_control);
            }
            content.push(Value::Object(encoded));
        }
        let previous_is_tool_result = messages.last().is_some_and(|previous| {
            previous.get("role").and_then(Value::as_str) == Some("user")
                && previous.get("content").and_then(Value::as_array).is_some_and(|blocks| {
                    blocks.iter().all(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
                })
        });
        if previous_is_tool_result {
            if let Some(previous) = messages.last_mut()
                && let Some(blocks) = previous.get_mut("content").and_then(Value::as_array_mut)
            {
                blocks.extend(content);
            }
            continue;
        }
        let mut encoded = Map::new();
        encoded.insert("role".to_string(), Value::String("user".to_string()));
        encoded.insert("content".to_string(), Value::Array(content));
        messages.push(Value::Object(encoded));
    }
    release_held(&mut messages, &mut held, &mut declared, breakpoints)?;
    Ok(messages)
}

fn release_held(
    messages: &mut Vec<Value>,
    held: &mut Vec<&Message>,
    declared: &mut Vec<ToolEntry>,
    breakpoints: &mut CacheControl,
) -> Result<(), AiError> {
    let native = accepts_native_system_after(messages);
    for update in held.drain(..) {
        if let Some(changes) = tools_update(update) {
            if !native {
                return Err(shared::invalid_request(format!(
                    "{ROUTE} tool updates must follow a user message or tool result"
                )));
            }
            let next = fold_tools(declared.clone(), &changes.add, &changes.remove);
            let withdrawn = withdrawn_tools(declared, &next);
            let lowered = lower_tools_update(update, changes, &withdrawn, breakpoints)?;
            *declared = next;
            if lowered.get("content").and_then(Value::as_array).is_some_and(|content| !content.is_empty()) {
                messages.push(lowered);
            }
            continue;
        }
        if native {
            messages.push(lower_native_system_update(update, breakpoints)?);
            continue;
        }
        let block = lower_wrapped_system_update(update, breakpoints)?;
        append_to_user_turn(messages, block);
    }
    Ok(())
}

fn accepts_native_system_after(messages: &[Value]) -> bool {
    let Some(last) =
        messages.iter().rev().find(|message| message.get("role").and_then(Value::as_str) != Some("system"))
    else {
        return false;
    };
    match last.get("role").and_then(Value::as_str) {
        Some("user") => true,
        Some("assistant") => {
            last.get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.last())
                .and_then(|block| block.get("type"))
                .and_then(Value::as_str)
                == Some("server_tool_use")
        }
        _ => false,
    }
}

fn splits_local_tool_results(messages: &[Message], index: usize) -> bool {
    let mut pending: HashSet<String> = HashSet::new();
    for message in messages.iter().take(index) {
        if let Message::Assistant(assistant) = message {
            for part in &assistant.content {
                if let ContentPart::ToolCall(part) = part
                    && part.provider_executed != Some(true)
                {
                    pending.insert(part.id.clone());
                }
            }
        }
        if let Message::Tool(tool) = message {
            for part in &tool.content {
                if let ContentPart::ToolResult(part) = part {
                    pending.remove(&part.id);
                }
            }
        }
    }
    !pending.is_empty()
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn system_update_parts(message: &Message) -> Result<Vec<&TextPart>, AiError> {
    message_content(message)
        .iter()
        .map(|part| match part {
            ContentPart::Text(part) => Ok(part),
            _ => Err(shared::invalid_request(format!("{ROUTE} system messages only support text content for now"))),
        })
        .collect()
}

fn lower_native_system_content(message: &Message, breakpoints: &mut CacheControl) -> Result<Vec<Value>, AiError> {
    let parts = system_update_parts(message)?;
    Ok(parts
        .into_iter()
        .map(|part| {
            let mut encoded = Map::new();
            encoded.insert("type".to_string(), Value::String("text".to_string()));
            encoded.insert("text".to_string(), Value::String(part.text.clone()));
            if let Some(cache_control) = cache_control(breakpoints, part.cache.as_ref()) {
                encoded.insert("cache_control".to_string(), cache_control);
            }
            Value::Object(encoded)
        })
        .collect())
}

fn lower_native_system_update(message: &Message, breakpoints: &mut CacheControl) -> Result<Value, AiError> {
    let content = lower_native_system_content(message, breakpoints)?;
    let mut encoded = Map::new();
    encoded.insert("role".to_string(), Value::String("system".to_string()));
    encoded.insert("content".to_string(), Value::Array(content));
    Ok(Value::Object(encoded))
}

fn lower_wrapped_system_update(message: &Message, breakpoints: &mut CacheControl) -> Result<Value, AiError> {
    let part = shared::wrapped_system_update(ROUTE, message)?;
    Ok(text_block(&part, breakpoints))
}

fn text_block(part: &TextPart, breakpoints: &mut CacheControl) -> Value {
    let mut encoded = Map::new();
    encoded.insert("type".to_string(), Value::String("text".to_string()));
    encoded.insert("text".to_string(), Value::String(part.text.clone()));
    if let Some(cache_control) = cache_control(breakpoints, part.cache.as_ref()) {
        encoded.insert("cache_control".to_string(), cache_control);
    }
    Value::Object(encoded)
}

fn lower_tools_update(
    message: &Message,
    changes: &ToolChanges,
    withdrawn: &[String],
    breakpoints: &mut CacheControl,
) -> Result<Value, AiError> {
    let text = if message_content(message).is_empty() {
        Vec::new()
    } else {
        lower_native_system_content(message, breakpoints)?
    };
    let mut content: Vec<Value> = Vec::new();
    for tool in flatten_tools(&changes.add) {
        let mut definition = Map::new();
        definition.insert("type".to_string(), Value::String("tool_definition".to_string()));
        definition.insert("definition".to_string(), lower_tool(breakpoints, &tool));
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("tool_addition".to_string()));
        encoded.insert("tool".to_string(), Value::Object(definition));
        content.push(Value::Object(encoded));
    }
    for name in withdrawn {
        let mut reference = Map::new();
        reference.insert("type".to_string(), Value::String("tool_reference".to_string()));
        reference.insert("name".to_string(), Value::String(name.clone()));
        let mut encoded = Map::new();
        encoded.insert("type".to_string(), Value::String("tool_removal".to_string()));
        encoded.insert("tool".to_string(), Value::Object(reference));
        content.push(Value::Object(encoded));
    }
    content.extend(text);
    let mut encoded = Map::new();
    encoded.insert("role".to_string(), Value::String("system".to_string()));
    encoded.insert("content".to_string(), Value::Array(content));
    Ok(Value::Object(encoded))
}

fn append_to_user_turn(messages: &mut Vec<Value>, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.push(block);
        return;
    }
    let mut encoded = Map::new();
    encoded.insert("role".to_string(), Value::String("user".to_string()));
    encoded.insert("content".to_string(), Value::Array(vec![block]));
    messages.push(Value::Object(encoded));
}

fn push_optional_string(encoded: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        encoded.insert(key.to_string(), Value::String(value.clone()));
    }
}

fn push_optional_number(encoded: &mut Map<String, Value>, key: &str, value: Option<&Number>) {
    if let Some(value) = value {
        encoded.insert(key.to_string(), Value::Number(value.clone()));
    }
}

fn push_block_binding(encoded: &mut Map<String, Value>, block_binding: &Option<BlockBinding>) {
    let Some(block_binding) = block_binding else {
        return;
    };
    let mut binding = Map::new();
    push_optional_string(&mut binding, "prefix_mismatch_behavior", &block_binding.prefix_mismatch_behavior);
    encoded.insert("block_binding".to_string(), Value::Object(binding));
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("{ROUTE} invalid provider option {field}"))
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    let context_management = input.get("contextManagement").map(decode_context_management).transpose()?;
    let thinking = input.get("thinking").map(decode_thinking).transpose()?;
    let effort = present_string(input, "effort")?;
    let service_tier = match present_string(input, "service_tier")? {
        Some(service_tier) => Some(service_tier),
        None => present_string(input, "serviceTier")?,
    };
    let speed = present_string(input, "speed")?;
    let metadata = input.get("metadata").map(decode_metadata).transpose()?;
    let container = input.get("container").map(decode_container).transpose()?;
    let inference_geo = first_present(
        present_nullable_string(input, "inference_geo")?,
        present_nullable_string(input, "inferenceGeo")?,
    );
    let cache_control = match input.get("cache_control") {
        Some(value) => Some(decode_cache_control(value)?),
        None => input.get("cacheControl").map(decode_cache_control).transpose()?,
    };
    let output_config = match input.get("output_config") {
        Some(value) => Some(decode_output_config(value)?),
        None => input.get("outputConfig").map(decode_output_config).transpose()?,
    };
    Ok(Options {
        context_management,
        thinking,
        effort,
        service_tier,
        speed,
        metadata,
        container,
        inference_geo,
        cache_control,
        output_config,
    })
}

fn first_present(first: Option<Option<String>>, second: Option<Option<String>>) -> Option<String> {
    match first {
        Some(Some(value)) => Some(value),
        _ => second.flatten(),
    }
}

fn present_string(input: &Map<String, Value>, key: &str) -> Result<Option<String>, AiError> {
    match input.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(key)),
    }
}

fn present_nullable_string(input: &Map<String, Value>, key: &str) -> Result<Option<Option<String>>, AiError> {
    match input.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => Ok(Some(Some(value.clone()))),
        Some(_) => Err(option_error(key)),
    }
}

fn decode_context_management(value: &Value) -> Result<ContextManagement, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("contextManagement"))?;
    let edits = object
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(|| option_error("contextManagement"))?
        .iter()
        .map(decode_context_management_edit)
        .collect::<Result<Vec<ContextManagementEdit>, AiError>>()?;
    Ok(ContextManagement { edits })
}

fn decode_context_management_edit(value: &Value) -> Result<ContextManagementEdit, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("contextManagement"))?;
    if object.get("type").and_then(Value::as_str) != Some("compact_20260112") {
        return Err(option_error("contextManagement"));
    }
    let trigger = match object.get("trigger") {
        None => None,
        Some(trigger) => Some(decode_trigger(trigger)?),
    };
    let pause_after_compaction = match object.get("pauseAfterCompaction") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(option_error("contextManagement")),
    };
    let instructions = match object.get("instructions") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(option_error("contextManagement")),
    };
    Ok(ContextManagementEdit { trigger, pause_after_compaction, instructions })
}

fn decode_trigger(value: &Value) -> Result<Value, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("contextManagement"))?;
    if object.get("type").and_then(Value::as_str) != Some("input_tokens") {
        return Err(option_error("contextManagement"));
    }
    let Some(value) = object.get("value").and_then(Value::as_i64) else {
        return Err(option_error("contextManagement"));
    };
    if value < 50_000 {
        return Err(option_error("contextManagement"));
    }
    let mut trigger = Map::new();
    trigger.insert("type".to_string(), Value::String("input_tokens".to_string()));
    trigger.insert("value".to_string(), Value::Number(Number::from(value)));
    Ok(Value::Object(trigger))
}

fn decode_thinking(value: &Value) -> Result<Thinking, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Err(option_error("thinking"));
    };
    let display = present_string(object, "display")?;
    let block_binding = match object.get("block_binding") {
        None => None,
        Some(value) => Some(decode_block_binding(value)?),
    };
    match kind {
        "enabled" => {
            let budget = match object.get("budgetTokens").or_else(|| object.get("budget_tokens")) {
                Some(Value::Number(budget)) => budget.clone(),
                _ => return Err(option_error("thinking")),
            };
            Ok(Thinking::Enabled { budget_tokens: budget, display, block_binding })
        }
        "adaptive" => Ok(Thinking::Adaptive { display, block_binding }),
        "disabled" | "between_tools" => Ok(Thinking::Off { kind: kind.to_string() }),
        _ => Err(option_error("thinking")),
    }
}

fn decode_block_binding(value: &Value) -> Result<BlockBinding, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let prefix_mismatch_behavior = present_string(object, "prefix_mismatch_behavior")?;
    Ok(BlockBinding { prefix_mismatch_behavior })
}

fn decode_metadata(value: &Value) -> Result<Value, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("metadata"))?;
    let mut metadata = Map::new();
    match object.get("user_id") {
        None => {}
        Some(Value::Null) => {
            metadata.insert("user_id".to_string(), Value::Null);
        }
        Some(Value::String(value)) => {
            metadata.insert("user_id".to_string(), Value::String(value.clone()));
        }
        Some(_) => return Err(option_error("metadata")),
    }
    Ok(Value::Object(metadata))
}

fn decode_container(value: &Value) -> Result<Value, AiError> {
    if let Value::String(value) = value {
        return Ok(Value::String(value.clone()));
    }
    let object = value.as_object().ok_or_else(|| option_error("container"))?;
    let mut container = Map::new();
    match object.get("id") {
        None => {}
        Some(Value::Null) => {
            container.insert("id".to_string(), Value::Null);
        }
        Some(Value::String(value)) => {
            container.insert("id".to_string(), Value::String(value.clone()));
        }
        Some(_) => return Err(option_error("container")),
    }
    match object.get("skills") {
        None => {}
        Some(Value::Null) => {
            container.insert("skills".to_string(), Value::Null);
        }
        Some(Value::Array(skills)) => {
            if skills.iter().any(|skill| !skill.is_object()) {
                return Err(option_error("container"));
            }
            container.insert("skills".to_string(), Value::Array(skills.clone()));
        }
        Some(_) => return Err(option_error("container")),
    }
    Ok(Value::Object(container))
}

static CACHE_TTL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+(?:ms|s|m|h|d)$").expect("cache ttl pattern compiles"));

fn valid_ttl(value: &str) -> bool {
    CACHE_TTL.is_match(value)
}

fn decode_cache_control(value: &Value) -> Result<Value, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("cache_control"))?;
    if object.get("type").and_then(Value::as_str) != Some("ephemeral") {
        return Err(option_error("cache_control"));
    }
    let mut cache_control = Map::new();
    cache_control.insert("type".to_string(), Value::String("ephemeral".to_string()));
    match object.get("ttl") {
        None => {}
        Some(Value::String(ttl)) if valid_ttl(ttl) => {
            cache_control.insert("ttl".to_string(), Value::String(ttl.clone()));
        }
        Some(_) => return Err(option_error("cache_control")),
    }
    Ok(Value::Object(cache_control))
}

fn decode_output_config(value: &Value) -> Result<OutputConfigInput, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("output_config"))?;
    let effort = present_nullable_string(object, "effort")?.flatten();
    let format = match object.get("format") {
        None | Some(Value::Null) => None,
        Some(value) => Some(decode_output_format(value)?),
    };
    Ok(OutputConfigInput { effort, format })
}

fn decode_output_format(value: &Value) -> Result<Value, AiError> {
    let object = value.as_object().ok_or_else(|| option_error("output_config"))?;
    if object.get("type").and_then(Value::as_str) != Some("json_schema") {
        return Err(option_error("output_config"));
    }
    let Some(schema) = object.get("schema").filter(|schema| schema.is_object()) else {
        return Err(option_error("output_config"));
    };
    let mut format = Map::new();
    format.insert("type".to_string(), Value::String("json_schema".to_string()));
    format.insert("schema".to_string(), schema.clone());
    Ok(Value::Object(format))
}

fn decode_media_options(value: Option<&Value>) -> Result<MediaOptions, AiError> {
    let empty = Map::new();
    let object = match value {
        None | Some(Value::Null) => &empty,
        Some(value) => value.as_object().ok_or_else(|| option_error("providerMetadata"))?,
    };
    let file_id = present_string(object, "file_id")?;
    let title = present_string(object, "title")?;
    let context = present_string(object, "context")?;
    let citations = match object.get("citations") {
        None => None,
        Some(value) => {
            let citations = value.as_object().ok_or_else(|| option_error("citations"))?;
            let Some(enabled) = citations.get("enabled").and_then(Value::as_bool) else {
                return Err(option_error("citations"));
            };
            let mut encoded = Map::new();
            encoded.insert("enabled".to_string(), Value::Bool(enabled));
            Some(Value::Object(encoded))
        }
    };
    let transformations = match object.get("transformations") {
        None => None,
        Some(value) => {
            let transformations = value.as_object().ok_or_else(|| option_error("transformations"))?;
            let mut encoded = Map::new();
            match transformations.get("oversized_image") {
                None => {}
                Some(Value::String(value)) if value == "downsize" || value == "error" => {
                    encoded.insert("oversized_image".to_string(), Value::String(value.clone()));
                }
                Some(_) => return Err(option_error("transformations")),
            }
            Some(Value::Object(encoded))
        }
    };
    Ok(MediaOptions { file_id, title, context, citations, transformations })
}

#[derive(Debug, Clone)]
struct PendingFinish {
    reason: FinishReasonDetails,
    provider_metadata: Option<ProviderMetadata>,
}

struct State {
    provider: String,
    compactions: BTreeMap<i64, Option<String>>,
    provider_metadata_key: String,
    tools: tool_stream::State,
    reasoning_signatures: BTreeMap<i64, String>,
    usage: Option<Usage>,
    pending_finish: Option<PendingFinish>,
    lifecycle: lifecycle::State,
}

#[derive(Debug, Clone)]
struct StreamBlock {
    r#type: String,
    id: Option<String>,
    name: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    data: Option<String>,
    input: Option<Value>,
    tool_use_id: Option<String>,
    content: Option<Value>,
}

#[derive(Debug, Clone)]
struct StopDetails {
    category: Option<Option<String>>,
    explanation: Option<Option<String>>,
}

#[derive(Debug, Clone)]
struct StreamDelta {
    r#type: Option<String>,
    content: Option<Option<String>>,
    text: Option<String>,
    thinking: Option<String>,
    partial_json: Option<String>,
    signature: Option<String>,
    stop_reason: Option<Option<String>>,
    stop_sequence: Option<Option<String>>,
    stop_details: Option<Option<StopDetails>>,
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn optional_nullable_string(object: &Map<String, Value>, key: &str) -> Option<Option<Option<String>>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::Null) => Some(Some(None)),
        Some(Value::String(value)) => Some(Some(Some(value.clone()))),
        Some(_) => None,
    }
}

fn decode_stream_block(value: &Value) -> Option<StreamBlock> {
    let object = value.as_object()?;
    Some(StreamBlock {
        r#type: object.get("type")?.as_str()?.to_string(),
        id: optional_string(object, "id")?,
        name: optional_string(object, "name")?,
        text: optional_string(object, "text")?,
        thinking: optional_string(object, "thinking")?,
        signature: optional_string(object, "signature")?,
        data: optional_string(object, "data")?,
        input: object.get("input").cloned(),
        tool_use_id: optional_string(object, "tool_use_id")?,
        content: object.get("content").cloned(),
    })
}

fn decode_stream_delta(value: &Value) -> Option<StreamDelta> {
    let object = value.as_object()?;
    let stop_details = match object.get("stop_details") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(value) => Some(Some(decode_stop_details(value)?)),
    };
    Some(StreamDelta {
        r#type: optional_string(object, "type")?,
        content: optional_nullable_string(object, "content")?,
        text: optional_string(object, "text")?,
        thinking: optional_string(object, "thinking")?,
        partial_json: optional_string(object, "partial_json")?,
        signature: optional_string(object, "signature")?,
        stop_reason: optional_nullable_string(object, "stop_reason")?,
        stop_sequence: optional_nullable_string(object, "stop_sequence")?,
        stop_details,
    })
}

fn decode_stop_details(value: &Value) -> Option<StopDetails> {
    let object = value.as_object()?;
    Some(StopDetails {
        category: optional_nullable_string(object, "category")?,
        explanation: optional_nullable_string(object, "explanation")?,
    })
}

fn decode_compaction(value: &Value) -> Option<Option<String>> {
    let object = value.as_object()?;
    if object.get("type")?.as_str()? != "compaction" {
        return None;
    }
    match object.get("content")? {
        Value::Null => Some(None),
        Value::String(content) => Some(Some(content.clone())),
        _ => None,
    }
}

fn invalid_stream_event(body: Option<String>) -> AiError {
    shared::event_error(ID, "Invalid anthropic/anthropic-messages stream event", body)
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

fn map_usage(usage: Option<&Value>, provider_metadata_key: &str, frame: Option<&Value>) -> Option<Usage> {
    let usage = usage.filter(|usage| usage.is_object())?;
    let iterations = usage
        .get("iterations")
        .and_then(Value::as_array)
        .filter(|iterations| !iterations.is_empty())
        .map(|iterations| iterations.iter().collect::<Vec<&Value>>())
        .unwrap_or_else(|| vec![usage]);
    let token = |item: &Value, field: &str| item.get(field).and_then(Value::as_u64);
    let non_cached = shared::sum_tokens(&iterations.iter().map(|item| token(item, "input_tokens")).collect::<Vec<_>>());
    let cache_read =
        shared::sum_tokens(&iterations.iter().map(|item| token(item, "cache_read_input_tokens")).collect::<Vec<_>>());
    let cache_write = shared::sum_tokens(
        &iterations.iter().map(|item| token(item, "cache_creation_input_tokens")).collect::<Vec<_>>(),
    );
    let input_tokens = shared::sum_tokens(&[non_cached, cache_read, cache_write]);
    let output_tokens =
        shared::sum_tokens(&iterations.iter().map(|item| token(item, "output_tokens")).collect::<Vec<_>>());
    let reasoning_tokens = shared::sum_tokens(
        &iterations
            .iter()
            .map(|item| {
                item.get("output_tokens_details")
                    .and_then(|details| details.get("thinking_tokens"))
                    .and_then(Value::as_u64)
            })
            .collect::<Vec<_>>(),
    );
    let context_tokens =
        match usage.get("iterations").and_then(Value::as_array).and_then(|iterations| iterations.last()) {
            Some(last) if last.get("type").and_then(Value::as_str) == Some("message") => shared::sum_tokens(&[
                token(last, "input_tokens"),
                token(last, "cache_read_input_tokens"),
                token(last, "cache_creation_input_tokens"),
            ]),
            _ => None,
        };
    let frames = frame.map(|frame| vec![frame]).unwrap_or_default();
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(provider_metadata_key.to_string(), usage.clone());
    Some(Usage {
        context_tokens,
        input_tokens,
        output_tokens,
        non_cached_input_tokens: non_cached,
        cache_read_input_tokens: cache_read,
        cache_write_input_tokens: cache_write,
        reasoning_tokens,
        total_tokens: shared::total_tokens(input_tokens, output_tokens, None),
        cost: shared::reported_billing(usage, &frames),
        provider_metadata: Some(provider_metadata),
    })
}

fn merge_usage(left: Option<Usage>, right: Option<Usage>, provider_metadata_key: &str) -> Option<Usage> {
    let (left, right) = match (left, right) {
        (Some(left), Some(right)) => (left, right),
        (left, right) => return left.or(right),
    };
    let non_cached_input_tokens = right.non_cached_input_tokens.or(left.non_cached_input_tokens);
    let cache_read_input_tokens = right.cache_read_input_tokens.or(left.cache_read_input_tokens);
    let cache_write_input_tokens = right.cache_write_input_tokens.or(left.cache_write_input_tokens);
    let input_tokens =
        shared::sum_tokens(&[non_cached_input_tokens, cache_read_input_tokens, cache_write_input_tokens]);
    let output_tokens = right.output_tokens.or(left.output_tokens);
    let reasoning_tokens = right.reasoning_tokens.or(left.reasoning_tokens);
    let empty = Value::Object(Map::new());
    let left_metadata = left
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(provider_metadata_key))
        .cloned()
        .unwrap_or(empty.clone());
    let right_metadata = right
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(provider_metadata_key))
        .cloned()
        .unwrap_or(empty);
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(
        provider_metadata_key.to_string(),
        crate::json::deep_merge(&left_metadata, &right_metadata).unwrap_or_else(|| Value::Object(Map::new())),
    );
    Some(Usage {
        context_tokens: right.context_tokens.or(left.context_tokens),
        input_tokens,
        output_tokens,
        non_cached_input_tokens,
        cache_read_input_tokens,
        cache_write_input_tokens,
        reasoning_tokens,
        total_tokens: shared::total_tokens(input_tokens, output_tokens, None),
        cost: right.cost.or(left.cost),
        provider_metadata: Some(provider_metadata),
    })
}

fn provider_error_message(event: &Value) -> String {
    let error = event.get("error").filter(|error| error.is_object());
    let kind = error.and_then(|error| error.get("type")).and_then(Value::as_str).filter(|kind| !kind.is_empty());
    let message =
        error.and_then(|error| error.get("message")).and_then(Value::as_str).filter(|message| !message.is_empty());
    match (kind, message) {
        (Some(kind), Some(message)) => format!("{kind}: {message}"),
        (Some(kind), None) => kind.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => "Anthropic Messages stream error".to_string(),
    }
}

fn string_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) => "[object Array]".to_string(),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn server_tool_result_event(block: &StreamBlock, provider_metadata_key: &str) -> Option<LlmEvent> {
    let name = server_tool_result_name(&block.r#type)?;
    let content = block.content.clone().unwrap_or(Value::Null);
    let error_payload = content
        .as_object()
        .filter(|content| content.contains_key("type"))
        .map(|content| string_value(content.get("type").unwrap_or(&Value::Null)))
        .unwrap_or_default();
    let result = if error_payload.ends_with("_tool_result_error") {
        ToolResultValue::Error { value: content.clone() }
    } else {
        ToolResultValue::Json { value: content.clone() }
    };
    let mut metadata = Map::new();
    metadata.insert("blockType".to_string(), Value::String(block.r#type.clone()));
    metadata.insert("result".to_string(), content);
    Some(LlmEvent::ToolResult {
        id: block.tool_use_id.clone().unwrap_or_default(),
        name: name.to_string(),
        namespace: None,
        result,
        output: None,
        provider_executed: Some(true),
        provider_metadata: Some(provider_metadata(provider_metadata_key, metadata)),
    })
}

impl State {
    fn on_message_start(&mut self, event: &Value) {
        let usage =
            map_usage(event.get("message").and_then(|message| message.get("usage")), &self.provider_metadata_key, None);
        self.usage = merge_usage(self.usage.clone(), usage, &self.provider_metadata_key);
    }

    fn on_content_block_start(&mut self, event: &Value, block: &StreamBlock) -> Vec<LlmEvent> {
        let index = event.get("index").and_then(Value::as_i64);
        if block.r#type == "tool_use" || block.r#type == "server_tool_use" {
            let Some(index) = index else {
                return Vec::new();
            };
            let Some(id) = block.id.clone().filter(|id| !id.is_empty()) else {
                return Vec::new();
            };
            let name = block.name.clone().unwrap_or_default();
            let executed = block.r#type == "server_tool_use";
            let input = match block.input.as_ref() {
                Some(input) if !input.is_object() || input.as_object().is_some_and(|input| !input.is_empty()) => {
                    Some(crate::json::encode_body(input))
                }
                _ => None,
            };
            let mut events = Vec::new();
            let lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
            self.lifecycle = lifecycle;
            self.tools.insert(
                index,
                tool_stream::PendingTool {
                    id: id.clone(),
                    name: name.clone(),
                    namespace: None,
                    input: input.unwrap_or_default(),
                    provider_executed: Some(executed),
                    provider_metadata: None,
                },
            );
            events.push(LlmEvent::ToolInputStart {
                id,
                name,
                namespace: None,
                provider_executed: executed.then_some(true),
                provider_metadata: None,
            });
            return events;
        }
        if block.r#type == "text"
            && let Some(text) = block.text.clone()
        {
            let mut events = Vec::new();
            let id = format!("text-{}", index.unwrap_or(0));
            let mut lifecycle = lifecycle::text_start(self.lifecycle.clone(), &mut events, &id, None);
            if !text.is_empty() {
                lifecycle = lifecycle::text_delta(lifecycle, &mut events, &id, &text, None);
            }
            self.lifecycle = lifecycle;
            return events;
        }
        if block.r#type == "thinking"
            && let Some(text) = block.thinking.clone()
        {
            let mut events = Vec::new();
            let id = format!("reasoning-{}", index.unwrap_or(0));
            let metadata = block.signature.clone().map(|signature| {
                let mut inner = Map::new();
                inner.insert("signature".to_string(), Value::String(signature));
                provider_metadata(&self.provider_metadata_key, inner)
            });
            let mut lifecycle = lifecycle::reasoning_start(self.lifecycle.clone(), &mut events, &id, metadata.clone());
            if !text.is_empty() {
                lifecycle = lifecycle::reasoning_delta(lifecycle, &mut events, &id, &text, metadata);
            }
            if let (Some(index), Some(signature)) = (index, block.signature.clone()) {
                self.reasoning_signatures.insert(index, signature);
            }
            self.lifecycle = lifecycle;
            return events;
        }
        if block.r#type == "redacted_thinking"
            && let Some(data) = block.data.clone()
        {
            let mut events = Vec::new();
            let id = format!("reasoning-{}", index.unwrap_or(0));
            let mut inner = Map::new();
            inner.insert("redactedData".to_string(), Value::String(data));
            let metadata = provider_metadata(&self.provider_metadata_key, inner);
            self.lifecycle = lifecycle::reasoning_start(self.lifecycle.clone(), &mut events, &id, Some(metadata));
            return events;
        }
        let Some(result) = server_tool_result_event(block, &self.provider_metadata_key) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
        events.push(result);
        events
    }

    fn on_content_block_delta(&mut self, event: &Value, delta: &StreamDelta) -> Result<Vec<LlmEvent>, AiError> {
        let index = event.get("index").and_then(Value::as_i64);
        let Some(kind) = delta.r#type.as_deref() else {
            return Ok(Vec::new());
        };
        if kind == "compaction_delta" {
            let Some(index) = index.filter(|index| self.compactions.contains_key(index)) else {
                return Err(shared::event_error(ID, "Compaction delta is missing its block or content", None));
            };
            let Some(content) = delta.content.clone() else {
                return Err(shared::event_error(ID, "Compaction delta is missing its block or content", None));
            };
            self.compactions.insert(index, content);
            return Ok(Vec::new());
        }
        if kind == "text_delta"
            && let Some(text) = delta.text.clone().filter(|text| !text.is_empty())
        {
            let id = format!("text-{}", index.unwrap_or(0));
            if !self.lifecycle.text.iter().any(|open| open == &id) {
                return Ok(Vec::new());
            }
            let mut events = Vec::new();
            self.lifecycle = lifecycle::text_delta(self.lifecycle.clone(), &mut events, &id, &text, None);
            return Ok(events);
        }
        if kind == "thinking_delta"
            && let Some(text) = delta.thinking.clone().filter(|text| !text.is_empty())
        {
            let id = format!("reasoning-{}", index.unwrap_or(0));
            if !self.lifecycle.reasoning.iter().any(|open| open == &id) {
                return Ok(Vec::new());
            }
            let mut events = Vec::new();
            self.lifecycle = lifecycle::reasoning_delta(self.lifecycle.clone(), &mut events, &id, &text, None);
            return Ok(events);
        }
        if kind == "signature_delta"
            && let Some(signature) = delta.signature.clone().filter(|signature| !signature.is_empty())
        {
            let index = index.unwrap_or(0);
            let id = format!("reasoning-{index}");
            if !self.lifecycle.reasoning.iter().any(|open| open == &id) {
                return Ok(Vec::new());
            }
            self.reasoning_signatures.insert(index, signature);
            return Ok(Vec::new());
        }
        if kind == "input_json_delta"
            && let Some(index) = index
        {
            let Some(text) = delta.partial_json.clone().filter(|text| !text.is_empty()) else {
                return Ok(Vec::new());
            };
            if !self.tools.contains_key(&index) {
                return Ok(Vec::new());
            }
            let Some(result) = tool_stream::append(&mut self.tools, index, &text) else {
                return Err(shared::event_error(
                    ID,
                    "Anthropic Messages tool argument delta is missing its tool call",
                    None,
                ));
            };
            let mut events = Vec::new();
            let lifecycle = if result.events.is_empty() {
                self.lifecycle.clone()
            } else {
                lifecycle::step_start(self.lifecycle.clone(), &mut events)
            };
            events.extend(result.events);
            self.lifecycle = lifecycle;
            return Ok(events);
        }
        Ok(Vec::new())
    }

    fn on_content_block_stop(&mut self, event: &Value) -> Result<Vec<LlmEvent>, AiError> {
        let Some(index) = event.get("index").and_then(Value::as_i64) else {
            return Ok(Vec::new());
        };
        if let Some(content) = self.compactions.remove(&index) {
            let mut events = Vec::new();
            self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
            events.push(LlmEvent::Compaction(CompactionPart {
                provider: self.provider.clone(),
                id: None,
                encrypted: None,
                text: Some(content),
            }));
            return Ok(events);
        }
        let result_events = tool_stream::finish(ID, &mut self.tools, index, None)?.events;
        let signature = self.reasoning_signatures.get(&index).cloned();
        let mut events = Vec::new();
        let lifecycle = if result_events.is_empty() {
            let text_id = format!("text-{index}");
            let reasoning_id = format!("reasoning-{index}");
            let metadata = signature.map(|signature| {
                let mut inner = Map::new();
                inner.insert("signature".to_string(), Value::String(signature));
                provider_metadata(&self.provider_metadata_key, inner)
            });
            lifecycle::reasoning_end(
                lifecycle::text_end(self.lifecycle.clone(), &mut events, &text_id, None, None),
                &mut events,
                &reasoning_id,
                metadata,
                None,
            )
        } else {
            lifecycle::step_start(self.lifecycle.clone(), &mut events)
        };
        events.extend(result_events);
        self.reasoning_signatures.remove(&index);
        self.lifecycle = lifecycle;
        Ok(events)
    }

    fn on_message_delta(&mut self, event: &Value, delta: &StreamDelta) -> Result<(), AiError> {
        let usage = map_usage(event.get("usage"), &self.provider_metadata_key, Some(event));
        self.usage = merge_usage(self.usage.clone(), usage, &self.provider_metadata_key);
        let stop_reason = match delta.stop_reason.clone() {
            None | Some(None) => return Ok(()),
            Some(Some(stop_reason)) => stop_reason,
        };
        let provider_metadata = match delta.stop_sequence.clone() {
            Some(Some(stop_sequence)) => {
                let mut inner = Map::new();
                inner.insert("stopSequence".to_string(), Value::String(stop_sequence));
                Some(provider_metadata(&self.provider_metadata_key, inner))
            }
            _ => self.pending_finish.as_ref().and_then(|pending| pending.provider_metadata.clone()),
        };
        let stop_details = delta.stop_details.clone().flatten();
        let category = stop_details
            .as_ref()
            .and_then(|details| details.category.clone().flatten())
            .filter(|category| !category.is_empty());
        let explanation = stop_details
            .as_ref()
            .and_then(|details| details.explanation.clone().flatten())
            .filter(|explanation| !explanation.is_empty());
        self.pending_finish = Some(PendingFinish {
            reason: FinishReasonDetails {
                normalized: map_finish_reason(&stop_reason),
                raw: Some(stop_reason),
                category,
                explanation,
            },
            provider_metadata,
        });
        Ok(())
    }

    fn on_message_stop(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        if !self.compactions.is_empty() {
            return Err(shared::event_error(ID, "Response ended with an incomplete compaction block", None));
        }
        let finished = tool_stream::finish_all(ID, &mut self.tools)?;
        let mut events = Vec::new();
        let lifecycle = if finished.events.is_empty() {
            self.lifecycle.clone()
        } else {
            lifecycle::step_start(self.lifecycle.clone(), &mut events)
        };
        events.extend(finished.events);
        let mut closed = lifecycle;
        for (index, signature) in std::mem::take(&mut self.reasoning_signatures) {
            let mut inner = Map::new();
            inner.insert("signature".to_string(), Value::String(signature));
            let metadata = provider_metadata(&self.provider_metadata_key, inner);
            closed = lifecycle::reasoning_end(closed, &mut events, &format!("reasoning-{index}"), Some(metadata), None);
        }
        let reason =
            self.pending_finish.as_ref().map(|pending| pending.reason.clone()).unwrap_or(FinishReasonDetails {
                normalized: FinishReason::Unknown,
                raw: None,
                category: None,
                explanation: None,
            });
        let provider_metadata = self.pending_finish.as_ref().and_then(|pending| pending.provider_metadata.clone());
        self.lifecycle = lifecycle::finish(closed, &mut events, reason, self.usage.clone(), provider_metadata);
        Ok(events)
    }
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event: Value = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(_) => return Err(invalid_stream_event(Some(frame))),
        };
        let Some(object) = event.as_object() else {
            return Err(invalid_stream_event(Some(frame)));
        };
        let event_type = match object.get("type") {
            Some(Value::String(event_type)) => event_type.clone(),
            Some(_) => return Err(invalid_stream_event(Some(frame))),
            None => {
                if object.get("error").is_some_and(|error| !error.is_null()) {
                    "error".to_string()
                } else {
                    return Ok(Vec::new());
                }
            }
        };
        if !SSE_EVENTS.contains(&event_type.as_str()) {
            return Ok(Vec::new());
        }
        if event_type != "content_block_start"
            && let Some(content_block) = event.get("content_block")
            && decode_stream_block(content_block).is_none()
        {
            return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
        }
        if event_type != "content_block_delta"
            && let Some(delta) = event.get("delta")
            && decode_stream_delta(delta).is_none()
        {
            return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
        }
        match event_type.as_str() {
            "message_start" => {
                self.on_message_start(&event);
                Ok(Vec::new())
            }
            "content_block_start" => self.handle_content_block_start(&event),
            "content_block_delta" => {
                let Some(delta_value) = event.get("delta").filter(|delta| delta.is_object()) else {
                    return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
                };
                if let Some(kind) = delta_value.get("type").and_then(Value::as_str)
                    && !STREAM_DELTA_TYPES.contains(&kind)
                {
                    return Ok(Vec::new());
                }
                let Some(delta) = decode_stream_delta(delta_value) else {
                    return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
                };
                self.on_content_block_delta(&event, &delta)
            }
            "content_block_stop" => self.on_content_block_stop(&event),
            "message_delta" => {
                let Some(delta_value) = event.get("delta") else {
                    return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
                };
                let Some(delta) = decode_stream_delta(delta_value) else {
                    return Err(invalid_stream_event(Some(crate::json::encode_body(&event))));
                };
                self.on_message_delta(&event, &delta)?;
                Ok(Vec::new())
            }
            "message_stop" => self.on_message_stop(),
            "error" => {
                let message = provider_error_message(&event);
                let body = crate::json::encode_body(&event);
                Err(AiError {
                    reason: classify_provider_failure(ProviderFailure {
                        message,
                        raw_body: Some(body),
                        ..ProviderFailure::default()
                    }),
                })
            }
            _ => Ok(Vec::new()),
        }
    }

    fn terminal(&self, _frame: &Frame) -> bool {
        false
    }
}

impl State {
    fn handle_content_block_start(&mut self, event: &Value) -> Result<Vec<LlmEvent>, AiError> {
        let Some(content_block) = event.get("content_block").filter(|block| block.is_object()) else {
            return Err(invalid_stream_event(Some(crate::json::encode_body(event))));
        };
        let Some(kind) = content_block.get("type").and_then(Value::as_str) else {
            return Err(invalid_stream_event(Some(crate::json::encode_body(event))));
        };
        if kind == "compaction" {
            let Some(index) = event.get("index").and_then(Value::as_i64) else {
                return Err(invalid_stream_event(Some(crate::json::encode_body(event))));
            };
            let Some(content) = decode_compaction(content_block) else {
                return Err(invalid_stream_event(Some(crate::json::encode_body(event))));
            };
            self.compactions.insert(index, content);
            return Ok(Vec::new());
        }
        if !STREAM_BLOCK_TYPES.contains(&kind) && server_tool_result_name(kind).is_none() {
            return Ok(Vec::new());
        }
        let Some(block) = decode_stream_block(content_block) else {
            return Err(invalid_stream_event(Some(crate::json::encode_body(event))));
        };
        if block.r#type == "tool_use" || block.r#type == "server_tool_use" {
            let Some(index) = event.get("index").and_then(Value::as_i64) else {
                return Err(shared::event_error(ID, format!("Anthropic {} missing index", block.r#type), None));
            };
            if block.id.as_deref().is_none_or(str::is_empty) {
                return Err(shared::event_error(ID, format!("Anthropic tool_use missing id at index {index}"), None));
            }
        }
        Ok(self.on_content_block_start(event, &block))
    }
}

impl Protocol for AnthropicMessages {
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
        Some(if model.provider == "anthropic" { format!("{PATH}?beta=true") } else { PATH.to_string() })
    }

    fn framing(&self) -> Definition {
        framing::sse_events(SSE_EVENTS.iter().map(|event| event.to_string()).collect::<Vec<String>>())
    }

    fn new_state(&self, request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        let model = &request.model;
        Box::new(State {
            provider: model.provider.clone(),
            compactions: BTreeMap::new(),
            provider_metadata_key: model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone()),
            tools: tool_stream::empty(),
            reasoning_signatures: BTreeMap::new(),
            usage: None,
            pending_finish: None,
            lifecycle: lifecycle::initial(),
        })
    }

    fn required_headers(&self, _request: &LlmRequest) -> Vec<(String, String)> {
        vec![("anthropic-version".to_string(), "2023-06-01".to_string())]
    }

    fn body_headers(&self, _model: &Model, request: &LlmRequest, body: &Value) -> Vec<(String, String)> {
        let mut betas: Vec<String> = vec!["interleaved-thinking-2025-05-14".to_string()];
        let requests_compaction = body
            .get("context_management")
            .and_then(|management| management.get("edits"))
            .and_then(Value::as_array)
            .is_some_and(|edits| !edits.is_empty());
        let replays_compaction = body.get("messages").and_then(Value::as_array).is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("content").and_then(Value::as_array).is_some_and(|content| {
                    content.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("compaction"))
                })
            })
        });
        if requests_compaction || replays_compaction {
            betas.push("compact-2026-01-12".to_string());
        }
        let messages = body.get("messages").and_then(Value::as_array);
        let has_system = |predicate: &dyn Fn(&Value) -> bool| {
            messages.is_some_and(|messages| {
                messages
                    .iter()
                    .any(|message| message.get("role").and_then(Value::as_str) == Some("system") && predicate(message))
            })
        };
        if has_system(&|message| message.get("output_config").is_some()) {
            betas.push("mid-conversation-output-config-2026-07-01".to_string());
        }
        if has_system(&|message| {
            message.get("content").and_then(Value::as_array).is_some_and(|content| {
                content.iter().any(|block| {
                    matches!(block.get("type").and_then(Value::as_str), Some("tool_addition") | Some("tool_removal"))
                })
            })
        }) {
            betas.push("inline-tools-2026-09-15".to_string());
        }
        if body.get("speed").is_some() {
            betas.push("fast-mode-2026-02-01".to_string());
        }
        if let Some(thinking) = body.get("thinking").filter(|thinking| thinking.is_object())
            && !matches!(thinking.get("type").and_then(Value::as_str), Some("disabled") | Some("between_tools"))
            && thinking.get("block_binding").is_some()
        {
            betas.push(THINKING_BINDING_BETA.to_string());
        }
        let existing = request
            .headers
            .as_ref()
            .and_then(|headers| {
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
                    .map(|(_, value)| value.to_string())
            })
            .unwrap_or_default();
        let mut values: Vec<String> = Vec::new();
        for value in existing.split(',').map(str::trim).filter(|value| !value.is_empty()) {
            if !values.iter().any(|existing| existing == value) {
                values.push(value.to_string());
            }
        }
        for beta in betas {
            if !values.iter().any(|value| value == &beta) {
                values.push(beta);
            }
        }
        if values.is_empty() {
            return Vec::new();
        }
        vec![("anthropic-beta".to_string(), values.join(","))]
    }

    fn supports_effort_updates(&self, model: &Model, request: &LlmRequest) -> bool {
        let thinking_off = request
            .provider_options
            .as_ref()
            .and_then(|options| options.get("thinking"))
            .and_then(Value::as_object)
            .and_then(|thinking| thinking.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "disabled" || kind == "between_tools");
        if !model_supports_effort_updates(model, thinking_off) {
            return false;
        }
        if !self.effort_updates_require_5_1 {
            return true;
        }
        if model
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.get("reasoning"))
            .and_then(|reasoning| reasoning.get("updates"))
            .is_some()
        {
            return true;
        }
        claude_model::claude_version(&model.id)
            .is_some_and(|version| version.major > 5 || (version.major == 5 && version.minor >= 1))
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        let placeable: PlacedGuard = Arc::new(placeable);
        Some(ToolUpdateEncoding {
            kinds: vec![ToolUpdateKind::Add, ToolUpdateKind::Remove, ToolUpdateKind::Replace],
            placeable: Some(placeable),
            declarable: None,
        })
    }
}

fn placeable(model: &Model, request: &LlmRequest, index: usize) -> bool {
    if !supports_native_system_updates(model) {
        return false;
    }
    let end = request
        .messages
        .iter()
        .enumerate()
        .find(|(at, message)| *at > index && matches!(message, Message::Assistant(_)))
        .map(|(at, _)| at);
    let limit = end.unwrap_or(request.messages.len());
    request.messages[..limit]
        .iter()
        .rev()
        .find(|message| !matches!(message, Message::System(_)))
        .is_some_and(|message| matches!(message, Message::User(_) | Message::Tool(_)))
}
