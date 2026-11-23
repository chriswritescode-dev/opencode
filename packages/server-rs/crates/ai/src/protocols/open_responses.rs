use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::id::IdSource;
use crate::prepare::effort_updates::effort_update;
use crate::prepare::tool_updates::tools_update;
use crate::protocols::openai_freeform::{self, Custom};
use crate::protocols::shared;
use crate::protocols::utils::{lifecycle, open_responses_options, tool_stream};
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, AiErrorReason, CompactionPart, ContentPart, FinishReason, FinishReasonDetails, LlmEvent, LlmRequest,
    MediaPart, Message, ProviderMetadata, SystemPart, TextPart, ToolChanges, ToolChoiceType, ToolContent,
    ToolDefinition, ToolResultPart, ToolResultValue, Usage,
};

pub const ID: &str = "open-responses";
pub const NAME: &str = "Open Responses";
pub const PATH: &str = "/responses";

const DEFAULT_EFFORT: &str = "medium";
const DEFAULT_METADATA_KEY: &str = "openresponses";
const COMPACTION_REPLAY_ERROR: &str = "Compaction state must be replayed to its originating provider and API";

pub type Event = Value;
pub type ToolChangesLowering<'a> = &'a mut dyn FnMut(&ToolChanges) -> Result<Vec<Value>, AiError>;
pub type NativeToolLowering = fn(&Map<String, Value>) -> Result<Value, AiError>;
pub type MediaLowering = fn(&MediaPart, &LlmRequest) -> Option<Value>;
pub type HostedToolRestore = fn(&Value) -> Option<Value>;

#[derive(Clone, Copy)]
pub struct ProviderAdapter {
    pub id: &'static str,
    pub name: &'static str,
    pub native_tool: Option<NativeToolLowering>,
    pub lower_media: Option<MediaLowering>,
    pub restore_hosted_tool_item: Option<HostedToolRestore>,
}

pub const BASE_ADAPTER: ProviderAdapter =
    ProviderAdapter { id: ID, name: NAME, native_tool: None, lower_media: None, restore_hosted_tool_item: None };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningSummaryStatus {
    Active,
    CanConclude,
    Concluded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReasoningStreamItem {
    pub encrypted_content: Option<String>,
    pub summary_parts: BTreeMap<usize, ReasoningSummaryStatus>,
    pub delta_indexes: BTreeSet<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageState {
    pub id: String,
    pub phase: Option<Value>,
}

pub struct State {
    pub provider: String,
    pub completed_compactions: BTreeSet<String>,
    pub id: String,
    pub name: String,
    pub provider_metadata_key: String,
    pub tools: tool_stream::State,
    pub tool_keys: BTreeMap<String, i64>,
    pub next_tool_key: i64,
    pub custom: BTreeMap<String, Custom>,
    pub has_function_call: bool,
    pub lifecycle: lifecycle::State,
    pub output_items: BTreeMap<i64, String>,
    pub message: Option<MessageState>,
    pub reasoning_items: BTreeMap<String, ReasoningStreamItem>,
    pub ids: Arc<dyn IdSource>,
}

impl State {
    pub fn tool_key(&mut self, item_id: &str) -> i64 {
        if let Some(key) = self.tool_keys.get(item_id) {
            return *key;
        }
        let key = self.next_tool_key;
        self.next_tool_key += 1;
        self.tool_keys.insert(item_id.to_string(), key);
        key
    }

    pub fn pending_tool_key(&self, item_id: &str) -> Option<i64> {
        self.tool_keys.get(item_id).copied().filter(|key| self.tools.contains_key(key))
    }

    pub fn has_pending_tool(&self, item_id: &str) -> bool {
        self.pending_tool_key(item_id).is_some()
    }
}

pub fn metadata_key(model: &Model) -> String {
    model.route.provider_metadata_key.clone().unwrap_or_else(|| DEFAULT_METADATA_KEY.to_string())
}

pub fn provider_metadata(provider_metadata_key: &str, metadata: Map<String, Value>) -> ProviderMetadata {
    let mut result = ProviderMetadata::new();
    result.insert(provider_metadata_key.to_string(), Value::Object(metadata));
    result
}

pub fn initial(request: &ResolvedRequest, context: &ExecutionContext, adapter: &ProviderAdapter) -> State {
    State {
        provider: request.model.provider.clone(),
        completed_compactions: BTreeSet::new(),
        id: adapter.id.to_string(),
        name: adapter.name.to_string(),
        provider_metadata_key: metadata_key(&request.model),
        tools: tool_stream::empty(),
        tool_keys: BTreeMap::new(),
        next_tool_key: 0,
        custom: openai_freeform::tools(&request.request),
        has_function_call: false,
        lifecycle: lifecycle::initial(),
        output_items: BTreeMap::new(),
        message: None,
        reasoning_items: BTreeMap::new(),
        ids: context.ids.clone(),
    }
}

pub fn lower_tool(protocol_name: &str, tool: &ToolDefinition) -> Result<Value, AiError> {
    if tool.native.is_some() {
        return Err(shared::invalid_request(format!(
            "{protocol_name} does not support provider-native tool {}",
            tool.name
        )));
    }
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    object.insert("name".to_string(), Value::String(tool.name.clone()));
    object.insert("description".to_string(), Value::String(tool.description.clone()));
    object.insert("parameters".to_string(), tool.input_schema.clone());
    object.insert("strict".to_string(), Value::Bool(false));
    Ok(Value::Object(object))
}

pub fn lower_custom_tool(tool: &ToolDefinition, custom: &Custom) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("custom".to_string()));
    object.insert("name".to_string(), Value::String(tool.name.clone()));
    object.insert("description".to_string(), Value::String(tool.description.clone()));
    let format = match openai_freeform::grammar(&custom.format) {
        Some(Value::Object(grammar)) => {
            let mut format = Map::new();
            format.insert("type".to_string(), Value::String("grammar".to_string()));
            for (key, value) in grammar {
                format.insert(key, value);
            }
            Value::Object(format)
        }
        _ => {
            let mut format = Map::new();
            format.insert("type".to_string(), Value::String("text".to_string()));
            Value::Object(format)
        }
    };
    object.insert("format".to_string(), format);
    Value::Object(object)
}

pub fn lower_tools(
    tools: &[ToolDefinition],
    adapter: &ProviderAdapter,
    custom: &BTreeMap<String, Custom>,
) -> Result<Vec<Value>, AiError> {
    tools
        .iter()
        .map(|tool| {
            if let Some(native) = tool.native.as_ref()
                && let Some(hook) = adapter.native_tool
            {
                return hook(native);
            }
            match custom.get(&tool.name) {
                Some(definition) => Ok(lower_custom_tool(tool, definition)),
                None => lower_tool(adapter.name, tool),
            }
        })
        .collect()
}

pub fn named_tool(name: &str, custom: &BTreeMap<String, Custom>) -> Value {
    let mut object = Map::new();
    object.insert(
        "type".to_string(),
        Value::String(if custom.contains_key(name) { "custom" } else { "function" }.to_string()),
    );
    object.insert("name".to_string(), Value::String(name.to_string()));
    Value::Object(object)
}

pub fn lower_tool_choice(
    protocol_name: &str,
    tool_choice: &crate::schema::ToolChoice,
    custom: &BTreeMap<String, Custom>,
) -> Result<Value, AiError> {
    match tool_choice.r#type {
        ToolChoiceType::Auto => return Ok(Value::String("auto".to_string())),
        ToolChoiceType::None => return Ok(Value::String("none".to_string())),
        ToolChoiceType::Required => return Ok(Value::String("required".to_string())),
        ToolChoiceType::Tool => {}
    }
    let name = tool_choice
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| shared::invalid_request(format!("{protocol_name} tool choice requires a tool name")))?;
    Ok(named_tool(name, custom))
}

pub fn allowed_tool_choice(
    options: &open_responses_options::Options,
    custom: &BTreeMap<String, Custom>,
) -> Option<Value> {
    let allowed = options.allowed_tools.as_ref()?;
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("allowed_tools".to_string()));
    object.insert(
        "mode".to_string(),
        Value::String(
            match allowed.mode {
                open_responses_options::AllowedToolsMode::Auto => "auto",
                open_responses_options::AllowedToolsMode::None => "none",
                open_responses_options::AllowedToolsMode::Required => "required",
            }
            .to_string(),
        ),
    );
    object.insert(
        "tools".to_string(),
        Value::Array(allowed.tool_names.iter().map(|name| named_tool(name, custom)).collect()),
    );
    Some(Value::Object(object))
}

pub fn lower_generation(
    request: &LlmRequest,
    model: &Model,
    options: &open_responses_options::Options,
) -> Map<String, Value> {
    let cache_key = shared::prompt_cache_key(request, model);
    let parallel_tool_calls = open_responses_options::resolve_parallel_tool_calls(request, options);
    let mut object = Map::new();
    object.insert("stream".to_string(), Value::Bool(true));
    insert_number(&mut object, "max_output_tokens", request.generation.max_tokens.as_ref());
    insert_number(&mut object, "temperature", request.generation.temperature.as_ref());
    insert_number(&mut object, "top_p", request.generation.top_p.as_ref());
    insert_number(&mut object, "presence_penalty", request.generation.presence_penalty.as_ref());
    insert_number(&mut object, "frequency_penalty", request.generation.frequency_penalty.as_ref());
    if let Some(store) = options.store {
        object.insert("store".to_string(), Value::Bool(store));
    }
    if let Some(metadata) = options.metadata.as_ref() {
        object.insert("metadata".to_string(), Value::Object(metadata.clone()));
    }
    if let Some(safety_identifier) = options.safety_identifier.as_deref().filter(|value| !value.is_empty()) {
        object.insert("safety_identifier".to_string(), Value::String(safety_identifier.to_string()));
    }
    if let Some(include_obfuscation) =
        options.stream_options.as_ref().and_then(|stream_options| stream_options.include_obfuscation)
    {
        let mut stream_options = Map::new();
        stream_options.insert("include_obfuscation".to_string(), Value::Bool(include_obfuscation));
        object.insert("stream_options".to_string(), Value::Object(stream_options));
    }
    if let Some(top_logprobs) = options.top_logprobs {
        object.insert("top_logprobs".to_string(), Value::Number(top_logprobs.into()));
    }
    if let Some(cache_key) = cache_key.filter(|cache_key| !cache_key.is_empty()) {
        object.insert("prompt_cache_key".to_string(), Value::String(cache_key));
    }
    if let Some(include) = options.include.as_ref().filter(|include| !include.is_empty()) {
        object.insert(
            "include".to_string(),
            Value::Array(include.iter().map(|value| Value::String(value.clone())).collect()),
        );
    }
    let reasoning_effort = options.reasoning_effort.as_deref().filter(|value| !value.is_empty());
    let reasoning_summary = options.reasoning_summary.as_deref().filter(|value| !value.is_empty());
    if reasoning_effort.is_some() || reasoning_summary.is_some() {
        let mut reasoning = Map::new();
        if let Some(effort) = reasoning_effort {
            reasoning.insert("effort".to_string(), Value::String(effort.to_string()));
        }
        if let Some(summary) = reasoning_summary {
            reasoning.insert("summary".to_string(), Value::String(summary.to_string()));
        }
        object.insert("reasoning".to_string(), Value::Object(reasoning));
    }
    if let Some(verbosity) = options.text_verbosity.as_deref().filter(|value| !value.is_empty()) {
        let mut text = Map::new();
        text.insert("verbosity".to_string(), Value::String(verbosity.to_string()));
        object.insert("text".to_string(), Value::Object(text));
    }
    if let Some(service_tier) = options.service_tier.as_deref().filter(|value| !value.is_empty()) {
        object.insert("service_tier".to_string(), Value::String(service_tier.to_string()));
    }
    if let Some(max_tool_calls) = options.max_tool_calls {
        object.insert("max_tool_calls".to_string(), Value::Number(max_tool_calls.into()));
    }
    if let Some(parallel_tool_calls) = parallel_tool_calls {
        object.insert("parallel_tool_calls".to_string(), Value::Bool(parallel_tool_calls));
    }
    if let Some(truncation) = options.truncation.as_deref().filter(|value| !value.is_empty()) {
        object.insert("truncation".to_string(), Value::String(truncation.to_string()));
    }
    object
}

fn insert_number(object: &mut Map<String, Value>, key: &str, value: Option<&serde_json::Number>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::Number(value.clone()));
    }
}

pub fn lower_conversation(
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
    lower_tool_changes: Option<ToolChangesLowering<'_>>,
) -> Result<Map<String, Value>, AiError> {
    let system = shared::join_text(request.system.iter().map(|part| match part {
        SystemPart::Text { text, .. } => text.clone(),
    }));
    let mut input = Vec::new();
    if !system.is_empty() {
        let mut message = Map::new();
        message.insert("type".to_string(), Value::String("message".to_string()));
        message.insert("role".to_string(), Value::String("developer".to_string()));
        message.insert("content".to_string(), Value::String(system));
        input.push(Value::Object(message));
    }
    input.extend(lower_messages(request, model, adapter, lower_tool_changes)?);
    let mut object = Map::new();
    object.insert("model".to_string(), Value::String(model.id.clone()));
    object.insert("input".to_string(), Value::Array(input));
    Ok(object)
}

pub fn from_request_with_adapter(
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
    lower_tool_changes: Option<ToolChangesLowering<'_>>,
) -> Result<Value, AiError> {
    let (tools, projected) = shared::flatten_tool_request(request);
    let options = open_responses_options::resolve(request);
    let custom = openai_freeform::tools(request);
    let mut body = lower_conversation(&projected, model, adapter, lower_tool_changes)?;
    for (key, value) in lower_generation(request, model, &options) {
        body.insert(key, value);
    }
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(lower_tools(&tools, adapter, &custom)?));
    }
    if let Some(choice) = allowed_tool_choice(&options, &custom) {
        body.insert("tool_choice".to_string(), choice);
    } else if let Some(choice) = request.tool_choice.as_ref() {
        body.insert("tool_choice".to_string(), lower_tool_choice(adapter.name, choice, &custom)?);
    }
    Ok(Value::Object(body))
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    from_request_with_adapter(request, model, &BASE_ADAPTER, None)
}

#[derive(Default)]
struct MessageMetadata {
    item_id: Option<String>,
    status: Option<String>,
    phase: Option<Value>,
}

struct ReasoningInput {
    index: usize,
    object: Map<String, Value>,
}

struct TextGroup {
    id: Option<String>,
    phase: Option<Value>,
    parts: Vec<String>,
}

pub fn lower_messages(
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
    mut lower_tool_changes: Option<ToolChangesLowering<'_>>,
) -> Result<Vec<Value>, AiError> {
    let mut input: Vec<Value> = Vec::new();
    let provider_metadata_key = metadata_key(model);
    let custom = openai_freeform::tools(request);
    for message in &request.messages {
        let effort = effort_update(message);
        let changes = tools_update(message);
        let raw_metadata = match message {
            Message::User(message) => message.provider_metadata.as_ref(),
            Message::Assistant(message) => message.provider_metadata.as_ref(),
            Message::System(_) | Message::Tool(_) => None,
        };
        let metadata = raw_metadata
            .and_then(|metadata| metadata.get(&provider_metadata_key))
            .map(decode_message_metadata)
            .unwrap_or_default();
        match message {
            Message::System(message) => {
                if let Some(update) = effort {
                    if input.last().and_then(|last| last.get("type")).and_then(Value::as_str)
                        == Some("configuration_update")
                    {
                        input.pop();
                    }
                    let effort = update.effort.clone().unwrap_or_else(|| DEFAULT_EFFORT.to_string());
                    let mut reasoning = Map::new();
                    reasoning.insert("effort".to_string(), Value::String(effort));
                    let mut object = Map::new();
                    object.insert("type".to_string(), Value::String("configuration_update".to_string()));
                    object.insert("reasoning".to_string(), Value::Object(reasoning));
                    input.push(Value::Object(object));
                    continue;
                }
                if let Some(changes) = changes
                    && let Some(lower) = lower_tool_changes.as_deref_mut()
                {
                    input.extend(lower(changes)?);
                    if message.content.is_empty() {
                        continue;
                    }
                }
                let text = system_update_text(adapter.name, &message.content)?;
                let mut object = Map::new();
                object.insert("type".to_string(), Value::String("message".to_string()));
                object.insert("role".to_string(), Value::String("developer".to_string()));
                object.insert("content".to_string(), Value::String(text));
                input.push(Value::Object(object));
                continue;
            }
            Message::User(message) => {
                let mut content = Vec::new();
                for part in &message.content {
                    content.push(lower_user_content(part, request, model, adapter)?);
                }
                if !content.is_empty() {
                    let mut object = Map::new();
                    object.insert("type".to_string(), Value::String("message".to_string()));
                    object.insert("role".to_string(), Value::String("user".to_string()));
                    object.insert("content".to_string(), Value::Array(content));
                    if let Some(item_id) = metadata.item_id.as_ref() {
                        object.insert("id".to_string(), Value::String(item_id.clone()));
                    }
                    if let Some(status) = metadata.status.as_ref() {
                        object.insert("status".to_string(), Value::String(status.clone()));
                    }
                    input.push(Value::Object(object));
                }
                continue;
            }
            Message::Assistant(message) => {
                let mut content: Vec<TextPart> = Vec::new();
                let mut reasoning_items: BTreeMap<String, ReasoningInput> = BTreeMap::new();
                let mut hosted_tool_items: BTreeSet<String> = BTreeSet::new();
                for part in &message.content {
                    match part {
                        ContentPart::Compaction(part) => {
                            flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                            if part.provider != model.provider || part.encrypted.is_none() {
                                return Err(shared::invalid_request(COMPACTION_REPLAY_ERROR));
                            }
                            let mut object = Map::new();
                            object.insert("type".to_string(), Value::String("compaction".to_string()));
                            if let Some(id) = part.id.as_ref() {
                                object.insert("id".to_string(), Value::String(id.clone()));
                            }
                            object.insert(
                                "encrypted_content".to_string(),
                                Value::String(part.encrypted.clone().unwrap_or_default()),
                            );
                            input.push(Value::Object(object));
                        }
                        ContentPart::Text(part) => content.push(part.clone()),
                        ContentPart::Reasoning(part) => {
                            flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                            let Some(reasoning) = lower_reasoning(part, &provider_metadata_key) else {
                                continue;
                            };
                            let id = reasoning.get("id").and_then(Value::as_str).map(str::to_string);
                            if let Some(existing) = id.as_ref().and_then(|id| reasoning_items.get_mut(id)) {
                                if let Some(summary) = reasoning.get("summary").and_then(Value::as_array)
                                    && let Some(target) =
                                        existing.object.get_mut("summary").and_then(Value::as_array_mut)
                                {
                                    target.extend(summary.iter().cloned());
                                }
                                if let Some(encrypted) = reasoning.get("encrypted_content").and_then(Value::as_str) {
                                    existing
                                        .object
                                        .insert("encrypted_content".to_string(), Value::String(encrypted.to_string()));
                                }
                                input[existing.index] = Value::Object(existing.object.clone());
                                continue;
                            }
                            if let Some(id) = id {
                                reasoning_items
                                    .insert(id, ReasoningInput { index: input.len(), object: reasoning.clone() });
                            }
                            input.push(Value::Object(reasoning));
                        }
                        ContentPart::ToolCall(part) => {
                            flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                            if part.provider_executed == Some(true) {
                                continue;
                            }
                            let defined = custom_tool(&custom, part.name.as_str(), part.namespace.as_deref());
                            input.push(lower_tool_call(part, &provider_metadata_key, defined));
                        }
                        ContentPart::ToolResult(part) if part.provider_executed == Some(true) => {
                            flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                            let id = item_id(part.provider_metadata.as_ref(), &provider_metadata_key);
                            let hosted = match &part.result {
                                ToolResultValue::Json { value } if is_hosted_tool_item(value) => Some(value.clone()),
                                ToolResultValue::Json { value } => {
                                    adapter.restore_hosted_tool_item.and_then(|restore| restore(value))
                                }
                                ToolResultValue::Text { .. }
                                | ToolResultValue::Error { .. }
                                | ToolResultValue::Content { .. } => None,
                            };
                            if let (Some(id), Some(hosted)) = (id.as_ref(), hosted.as_ref())
                                && hosted.get("id").and_then(Value::as_str) == Some(id.as_str())
                            {
                                if !hosted_tool_items.contains(id) {
                                    input.push(hosted.clone());
                                    hosted_tool_items.insert(id.clone());
                                }
                                continue;
                            }
                            let content = match &part.result {
                                ToolResultValue::Content { value } => value
                                    .iter()
                                    .map(|item| lower_tool_result_content_item(item, request, model, adapter))
                                    .collect::<Result<Vec<Value>, AiError>>()?,
                                _ => vec![input_text(&shared::tool_result_text(part))],
                            };
                            input.push(user_message(content));
                        }
                        ContentPart::Media(part) => {
                            flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                            let lowered = lower_media(part, request, model, adapter)?;
                            input.push(user_message(vec![lowered]));
                        }
                        _ => return Err(unsupported_assistant(adapter.name)),
                    }
                }
                flush_text(&mut input, &mut content, &metadata, &provider_metadata_key);
                continue;
            }
            Message::Tool(message) => {
                for part in &message.content {
                    let ContentPart::ToolResult(part) = part else {
                        continue;
                    };
                    let output = lower_tool_result_output(part, request, model, adapter)?;
                    let defined = custom_tool(&custom, part.name.as_str(), part.namespace.as_deref());
                    let mut object = Map::new();
                    object.insert(
                        "type".to_string(),
                        Value::String(
                            if defined.is_some() { "custom_tool_call_output" } else { "function_call_output" }
                                .to_string(),
                        ),
                    );
                    object.insert("call_id".to_string(), Value::String(part.id.clone()));
                    object.insert("output".to_string(), output);
                    input.push(Value::Object(object));
                }
            }
        }
    }
    Ok(input)
}

fn flush_text(
    input: &mut Vec<Value>,
    content: &mut Vec<TextPart>,
    metadata: &MessageMetadata,
    provider_metadata_key: &str,
) {
    if content.is_empty() {
        return;
    }
    let mut groups: Vec<TextGroup> = Vec::new();
    for part in std::mem::take(content) {
        let part_metadata = part.provider_metadata.as_ref().and_then(|metadata| metadata.get(provider_metadata_key));
        let id = item_id(part.provider_metadata.as_ref(), provider_metadata_key).or_else(|| metadata.item_id.clone());
        let part_phase = message_phase(part_metadata.and_then(|metadata| metadata.get("phase")));
        let phase = part_phase.or_else(|| metadata.phase.clone());
        match groups.last_mut() {
            Some(group) if group.id == id && group.phase == phase => group.parts.push(part.text),
            _ => groups.push(TextGroup { id, phase, parts: vec![part.text] }),
        }
    }
    for group in groups {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("message".to_string()));
        if let Some(id) = group.id {
            object.insert("id".to_string(), Value::String(id));
        }
        object.insert("role".to_string(), Value::String("assistant".to_string()));
        object.insert("status".to_string(), Value::String("completed".to_string()));
        object.insert(
            "content".to_string(),
            Value::Array(group.parts.into_iter().map(|text| output_text(&text)).collect()),
        );
        if let Some(phase) = group.phase {
            object.insert("phase".to_string(), phase);
        }
        input.push(Value::Object(object));
    }
}

fn lower_user_content(
    part: &ContentPart,
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
) -> Result<Value, AiError> {
    match part {
        ContentPart::Text(part) => Ok(input_text(&part.text)),
        ContentPart::Media(part) => lower_media(part, request, model, adapter),
        _ => Err(shared::invalid_request(format!(
            "{} user messages only support text and media content for now",
            adapter.name
        ))),
    }
}

fn lower_tool_result_content_item(
    item: &ToolContent,
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
) -> Result<Value, AiError> {
    match item {
        ToolContent::Text { text } => Ok(input_text(text)),
        ToolContent::File { .. } => {
            let media = shared::tool_file_media(item).ok_or_else(|| {
                shared::invalid_request(format!("{} requires a tool result file with media", adapter.name))
            })?;
            lower_media(&media, request, model, adapter)
        }
    }
}

fn lower_tool_result_output(
    part: &ToolResultPart,
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
) -> Result<Value, AiError> {
    let ToolResultValue::Content { value } = &part.result else {
        return Ok(Value::String(shared::tool_result_text(part)));
    };
    Ok(Value::Array(
        value
            .iter()
            .map(|item| lower_tool_result_content_item(item, request, model, adapter))
            .collect::<Result<Vec<Value>, AiError>>()?,
    ))
}

fn lower_media(
    part: &MediaPart,
    request: &LlmRequest,
    model: &Model,
    adapter: &ProviderAdapter,
) -> Result<Value, AiError> {
    if let Some(hook) = adapter.lower_media
        && let Some(value) = hook(part, request)
    {
        return Ok(value);
    }
    let detail = part
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(&metadata_key(model)))
        .and_then(|metadata| metadata.get("detail"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mime = shared::media_type(&part.media).to_lowercase();
    let url = shared::media_url(&part.media).map(str::to_string);
    let location = match url.as_ref() {
        Some(url) => url.clone(),
        None => shared::require_inline_media(adapter.name, &part.media)?.data_url,
    };
    let kind = shared::media_kind(&mime);
    let mut object = Map::new();
    if kind != "image" {
        if kind == "video" {
            object.insert("type".to_string(), Value::String("input_video".to_string()));
            object.insert("video_url".to_string(), Value::String(location));
            return Ok(Value::Object(object));
        }
        if let Some(url) = url {
            object.insert("type".to_string(), Value::String("input_file".to_string()));
            object.insert("file_url".to_string(), Value::String(url));
            insert_detail(&mut object, detail);
            return Ok(Value::Object(object));
        }
        let filename = part
            .filename
            .clone()
            .unwrap_or_else(|| if mime == "application/pdf" { "document.pdf" } else { "file" }.to_string());
        object.insert("type".to_string(), Value::String("input_file".to_string()));
        object.insert("filename".to_string(), Value::String(filename));
        insert_detail(&mut object, detail);
        object.insert("file_data".to_string(), Value::String(location));
        return Ok(Value::Object(object));
    }
    object.insert("type".to_string(), Value::String("input_image".to_string()));
    object.insert("image_url".to_string(), Value::String(location));
    insert_detail(&mut object, detail);
    Ok(Value::Object(object))
}

fn insert_detail(object: &mut Map<String, Value>, detail: Option<String>) {
    if let Some(detail) = detail {
        object.insert("detail".to_string(), Value::String(detail));
    }
}

fn lower_tool_call(part: &crate::schema::ToolCallPart, provider_metadata_key: &str, custom: Option<&Custom>) -> Value {
    let id = if openai_freeform::keeps_item_id(part.provider_metadata.as_ref(), provider_metadata_key, custom.is_some())
    {
        item_id(part.provider_metadata.as_ref(), provider_metadata_key)
    } else {
        None
    };
    let mut object = Map::new();
    if let Some(custom) = custom {
        object.insert("type".to_string(), Value::String("custom_tool_call".to_string()));
        if let Some(id) = id {
            object.insert("id".to_string(), Value::String(id));
        }
        object.insert("call_id".to_string(), Value::String(part.id.clone()));
        object.insert("name".to_string(), Value::String(part.name.clone()));
        object.insert("input".to_string(), Value::String(openai_freeform::to_raw(custom, &part.input)));
        return Value::Object(object);
    }
    object.insert("type".to_string(), Value::String("function_call".to_string()));
    if let Some(id) = id {
        object.insert("id".to_string(), Value::String(id));
    }
    object.insert("call_id".to_string(), Value::String(part.id.clone()));
    object.insert("name".to_string(), Value::String(part.name.clone()));
    if let Some(namespace) = part.namespace.as_ref() {
        object.insert("namespace".to_string(), Value::String(namespace.clone()));
    }
    let arguments = if part.input.is_null() { Value::Object(Map::new()) } else { part.input.clone() };
    object.insert("arguments".to_string(), Value::String(crate::json::encode_body(&arguments)));
    Value::Object(object)
}

fn lower_reasoning(part: &crate::schema::ReasoningPart, provider_metadata_key: &str) -> Option<Map<String, Value>> {
    let metadata = part.provider_metadata.as_ref()?.get(provider_metadata_key)?;
    if metadata.is_null() {
        return None;
    }
    let id = item_id(part.provider_metadata.as_ref(), provider_metadata_key);
    let encrypted =
        metadata.get("reasoningEncryptedContent").filter(|value| value.is_string() || value.is_null()).cloned();
    let has_encrypted = encrypted.as_ref().is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty()));
    if part.text.is_empty() && !has_encrypted {
        return None;
    }
    let mut summary = Vec::new();
    if !part.text.is_empty() {
        let mut entry = Map::new();
        entry.insert("type".to_string(), Value::String("summary_text".to_string()));
        entry.insert("text".to_string(), Value::String(part.text.clone()));
        summary.push(Value::Object(entry));
    }
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("reasoning".to_string()));
    if let Some(id) = id {
        object.insert("id".to_string(), Value::String(id));
    }
    object.insert("summary".to_string(), Value::Array(summary));
    if let Some(encrypted) = encrypted {
        object.insert("encrypted_content".to_string(), encrypted);
    }
    Some(object)
}

fn custom_tool<'a>(custom: &'a BTreeMap<String, Custom>, name: &str, namespace: Option<&str>) -> Option<&'a Custom> {
    if namespace.is_some() {
        return None;
    }
    custom.get(name)
}

fn system_update_text(route: &str, content: &[ContentPart]) -> Result<String, AiError> {
    let mut texts = Vec::new();
    for part in content {
        let ContentPart::Text(part) = part else {
            return Err(shared::invalid_request(format!("{route} system messages only support text content for now")));
        };
        texts.push(part.text.clone());
    }
    Ok(shared::join_text(texts))
}

fn unsupported_assistant(route: &str) -> AiError {
    shared::invalid_request(format!(
        "{route} assistant messages only support text, reasoning, tool-call, tool-result, and media content for now"
    ))
}

fn decode_message_metadata(value: &Value) -> MessageMetadata {
    MessageMetadata {
        item_id: value.get("itemId").and_then(Value::as_str).map(str::to_string),
        status: value.get("status").and_then(Value::as_str).map(str::to_string),
        phase: message_phase(value.get("phase")),
    }
}

fn item_id(provider_metadata: Option<&ProviderMetadata>, provider_metadata_key: &str) -> Option<String> {
    let metadata = provider_metadata?.get(provider_metadata_key)?;
    let value = metadata.get("itemId")?.as_str()?;
    let separator = value.find('_')?;
    (separator > 0 && separator < value.len() - 1).then(|| value.to_string())
}

fn input_text(text: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("input_text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(object)
}

fn output_text(text: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("output_text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(object)
}

fn user_message(content: Vec<Value>) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("message".to_string()));
    object.insert("role".to_string(), Value::String("user".to_string()));
    object.insert("content".to_string(), Value::Array(content));
    Value::Object(object)
}

pub fn map_usage(usage: Option<&Value>, provider_metadata_key: &str, frames: &[&Value]) -> Option<Usage> {
    let usage = usage.filter(|usage| usage.is_object())?;
    let input = number_at(usage, &["input_tokens"]);
    let output = number_at(usage, &["output_tokens"]);
    let cached = number_at(usage, &["input_tokens_details", "cached_tokens"]);
    let cache_write = number_at(usage, &["input_tokens_details", "cache_write_tokens"]);
    let reasoning = number_at(usage, &["output_tokens_details", "reasoning_tokens"]);
    let total = number_at(usage, &["total_tokens"]);
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(provider_metadata_key.to_string(), usage.clone());
    Some(Usage {
        context_tokens: None,
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: shared::subtract_tokens(input, shared::sum_tokens(&[cached, cache_write])),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: cache_write,
        reasoning_tokens: reasoning,
        total_tokens: shared::total_tokens(input, output, total),
        cost: shared::reported_billing(usage, frames),
        provider_metadata: Some(provider_metadata),
    })
}

fn number_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut value = value;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_u64()
}

fn content_filter_category(raw: Option<&Value>) -> Option<String> {
    let filters = raw?.as_array()?;
    let blocked = filters
        .iter()
        .filter(|filter| filter.get("blocked").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<&Value>>();
    let filter = blocked
        .iter()
        .copied()
        .find(|filter| filter.get("source_type").and_then(Value::as_str) == Some("completion"))
        .or_else(|| blocked.first().copied())?;
    if let Some(rules) = filter.get("content_filter_raw").and_then(Value::as_array)
        && let Some(label) = rules
            .iter()
            .find(|rule| {
                rule.get("action").and_then(Value::as_str) == Some("BLOCK")
                    && rule.get("result") == Some(&Value::Bool(true))
            })
            .and_then(|rule| rule.get("label"))
            .and_then(Value::as_str)
    {
        return Some(label.to_string());
    }
    let results = filter.get("content_filter_results").unwrap_or(&Value::Null);
    shared::content_filter_category(&[results])
}

fn map_finish_reason(event: &Value, has_function_call: bool) -> FinishReason {
    let reason = event
        .get("response")
        .and_then(|response| response.get("incomplete_details"))
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    let Some(reason) = reason else {
        if has_function_call {
            return FinishReason::ToolCalls;
        }
        if event.get("type").and_then(Value::as_str) == Some("response.incomplete") {
            return FinishReason::Unknown;
        }
        return FinishReason::Stop;
    };
    match reason {
        "max_output_tokens" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        _ if has_function_call => FinishReason::ToolCalls,
        _ => FinishReason::Unknown,
    }
}

pub fn parse_event(route: &str, frame: &str) -> Result<Event, AiError> {
    let value: Value = serde_json::from_str(frame)
        .map_err(|_| shared::event_error(route, format!("Invalid {route} stream event"), Some(frame.to_string())))?;
    let Some(object) = value.as_object() else {
        return Err(shared::event_error(route, format!("Invalid {route} stream event"), Some(frame.to_string())));
    };
    if object.contains_key("type") {
        return Ok(value);
    }
    let mut object = object.clone();
    let kind = if object.get("error").is_some_and(|error| !error.is_null()) { "error" } else { "ignored" };
    object.insert("type".to_string(), Value::String(kind.to_string()));
    Ok(Value::Object(object))
}

pub fn terminal(event: &Event) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("error" | "response.completed" | "response.incomplete" | "response.failed")
    )
}

fn item_id_prefix(kind: &str) -> &'static str {
    match kind {
        "message" => "msg",
        "reasoning" => "rs",
        "function_call" => "fc",
        "custom_tool_call" => "ctc",
        "compaction" => "cmp",
        _ => "item",
    }
}

fn resolve_item(state: &State, item: &Value, index: Option<i64>) -> Value {
    let mut resolved = item.as_object().cloned().unwrap_or_default();
    let id = index
        .and_then(|index| state.output_items.get(&index))
        .cloned()
        .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| {
            let prefix = item.get("type").and_then(Value::as_str).map(item_id_prefix).unwrap_or("item");
            format!("{prefix}_{}", state.ids.uuid().replace('-', ""))
        });
    resolved.insert("id".to_string(), Value::String(id));
    Value::Object(resolved)
}

pub fn normalize(state: &State, input: &Event) -> Event {
    let mut normalized = input.as_object().cloned().unwrap_or_default();
    if let Some(item_id) = input.get("item_id").and_then(Value::as_str) {
        let output_index = input.get("output_index").and_then(Value::as_i64);
        let mapped = output_index
            .and_then(|index| state.output_items.get(&index))
            .cloned()
            .unwrap_or_else(|| item_id.to_string());
        normalized.insert("item_id".to_string(), Value::String(mapped));
    }
    if let Some(item) = input.get("item").filter(|item| item.is_object()) {
        let output_index = input.get("output_index").and_then(Value::as_i64);
        normalized.insert("item".to_string(), resolve_item(state, item, output_index));
    }
    Value::Object(normalized)
}

fn field<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

fn summary_index(event: &Value) -> usize {
    event.get("summary_index").and_then(Value::as_u64).unwrap_or(0) as usize
}

fn message_phase(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    match value {
        Value::Null => Some(Value::Null),
        Value::String(phase) if phase == "commentary" || phase == "final_answer" => Some(value.clone()),
        _ => None,
    }
}

fn text_metadata(state: &State, id: &str, phase: Option<&Value>) -> ProviderMetadata {
    let mut metadata = Map::new();
    metadata.insert("itemId".to_string(), Value::String(id.to_string()));
    if let Some(phase) = phase {
        metadata.insert("phase".to_string(), phase.clone());
    }
    provider_metadata(&state.provider_metadata_key, metadata)
}

fn reasoning_metadata(state: &State, item: &Value) -> ProviderMetadata {
    let mut metadata = Map::new();
    if let Some(id) = item.get("id").and_then(Value::as_str) {
        metadata.insert("itemId".to_string(), Value::String(id.to_string()));
    }
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null);
    metadata.insert("reasoningEncryptedContent".to_string(), encrypted);
    provider_metadata(&state.provider_metadata_key, metadata)
}

fn on_output_text_delta(state: &mut State, event: &Event, id: &str) -> Vec<LlmEvent> {
    let Some(delta) = field(event, "delta").filter(|delta| !delta.is_empty()) else {
        return Vec::new();
    };
    let Some(phase) = state.message.as_ref().filter(|message| message.id == id).map(|message| message.phase.clone())
    else {
        return Vec::new();
    };
    let metadata = text_metadata(state, id, phase.as_ref());
    let mut events = Vec::new();
    let lifecycle = lifecycle::text_start(state.lifecycle.clone(), &mut events, id, Some(metadata));
    state.lifecycle = lifecycle::text_delta(lifecycle, &mut events, id, delta, None);
    events
}

fn on_output_text_done(state: &mut State, event: &Event, id: &str) -> Vec<LlmEvent> {
    if state.message.as_ref().is_some_and(|message| message.id == id) {
        if state.lifecycle.text.iter().any(|open| open == id) {
            return Vec::new();
        }
        let Some(text) = field(event, "text") else {
            return Vec::new();
        };
        let mut patched = event.clone();
        patched["delta"] = Value::String(text.to_string());
        return on_output_text_delta(state, &patched, id);
    }
    let mut events = Vec::new();
    state.lifecycle = lifecycle::text_end(state.lifecycle.clone(), &mut events, id, None, None);
    events
}

fn start_reasoning_summary_part(state: &mut State, item_id: &str, index: usize) -> Vec<LlmEvent> {
    let Some(item) = state.reasoning_items.get(item_id).cloned() else {
        return Vec::new();
    };
    if index == 0 || item.summary_parts.contains_key(&index) {
        return Vec::new();
    }
    let mut events = Vec::new();
    let mut lifecycle = state.lifecycle.clone();
    let mut part_metadata = Map::new();
    part_metadata.insert("itemId".to_string(), Value::String(item_id.to_string()));
    let part_metadata = provider_metadata(&state.provider_metadata_key, part_metadata);
    for (key, status) in &item.summary_parts {
        if *status == ReasoningSummaryStatus::Concluded {
            continue;
        }
        lifecycle = lifecycle::reasoning_end(
            lifecycle,
            &mut events,
            &format!("{item_id}:{key}"),
            Some(part_metadata.clone()),
            None,
        );
    }
    let mut start_metadata = Map::new();
    start_metadata.insert("itemId".to_string(), Value::String(item_id.to_string()));
    start_metadata.insert(
        "reasoningEncryptedContent".to_string(),
        item.encrypted_content.clone().map(Value::String).unwrap_or(Value::Null),
    );
    lifecycle = lifecycle::reasoning_start(
        lifecycle,
        &mut events,
        &format!("{item_id}:{index}"),
        Some(provider_metadata(&state.provider_metadata_key, start_metadata)),
    );
    state.lifecycle = lifecycle;
    if let Some(entry) = state.reasoning_items.get_mut(item_id) {
        for status in entry.summary_parts.values_mut() {
            if *status != ReasoningSummaryStatus::Concluded {
                *status = ReasoningSummaryStatus::Concluded;
            }
        }
        entry.summary_parts.insert(index, ReasoningSummaryStatus::Active);
    }
    events
}

pub fn on_reasoning_delta(state: &mut State, event: &Event, item_id: &str) -> Vec<LlmEvent> {
    let Some(delta) = field(event, "delta").filter(|delta| !delta.is_empty()) else {
        return Vec::new();
    };
    let Some(item) = state.reasoning_items.get(item_id) else {
        return Vec::new();
    };
    let index = summary_index(event);
    if item.summary_parts.get(&index) == Some(&ReasoningSummaryStatus::Concluded) {
        return Vec::new();
    }
    let mut events = start_reasoning_summary_part(state, item_id, index);
    if !state.reasoning_items.contains_key(item_id) {
        return events;
    }
    state.lifecycle =
        lifecycle::reasoning_delta(state.lifecycle.clone(), &mut events, &format!("{item_id}:{index}"), delta, None);
    if let Some(item) = state.reasoning_items.get_mut(item_id) {
        item.delta_indexes.insert(index);
    }
    events
}

fn on_reasoning_done(state: &mut State, event: &Event, item_id: &str) -> Vec<LlmEvent> {
    let Some(text) = field(event, "text") else {
        return Vec::new();
    };
    let Some(item) = state.reasoning_items.get(item_id) else {
        return Vec::new();
    };
    let index = summary_index(event);
    if item.delta_indexes.contains(&index) {
        return Vec::new();
    }
    let mut patched = event.clone();
    patched["delta"] = Value::String(text.to_string());
    on_reasoning_delta(state, &patched, item_id)
}

fn on_reasoning_summary_part_done(state: &mut State, event: &Event) -> Vec<LlmEvent> {
    let (Some(item_id), Some(index)) = (field(event, "item_id"), event.get("summary_index").and_then(Value::as_u64))
    else {
        return Vec::new();
    };
    let index = index as usize;
    let Some(item) = state.reasoning_items.get(item_id) else {
        return Vec::new();
    };
    if item.summary_parts.get(&index) != Some(&ReasoningSummaryStatus::Active) {
        return Vec::new();
    }
    if let Some(item) = state.reasoning_items.get_mut(item_id) {
        item.summary_parts.insert(index, ReasoningSummaryStatus::CanConclude);
    }
    Vec::new()
}

fn raw_input(item: &Value) -> &str {
    field(item, "input").unwrap_or_default()
}

fn on_output_item_added(state: &mut State, item: &Value) -> Vec<LlmEvent> {
    let kind = field(item, "type").unwrap_or_default();
    let item_id = field(item, "id").unwrap_or_default().to_string();
    match kind {
        "message" => {
            let phase = message_phase(item.get("phase"));
            let mut events = Vec::new();
            let mut lifecycle = state.lifecycle.clone();
            let open =
                state.lifecycle.text.iter().filter(|id| id.as_str() != item_id).cloned().collect::<Vec<String>>();
            for id in open {
                let open_phase =
                    state.message.as_ref().filter(|message| message.id == id).and_then(|message| message.phase.clone());
                lifecycle = lifecycle::text_end(
                    lifecycle,
                    &mut events,
                    &id,
                    Some(text_metadata(state, &id, open_phase.as_ref())),
                    None,
                );
            }
            state.lifecycle = lifecycle;
            let phase = match phase {
                Some(phase) => Some(phase),
                None => state.message.as_ref().filter(|message| message.id == item_id).and_then(|m| m.phase.clone()),
            };
            state.message = Some(MessageState { id: item_id, phase });
            events
        }
        "reasoning" => {
            if state.reasoning_items.contains_key(&item_id) {
                return Vec::new();
            }
            let mut events = Vec::new();
            let metadata = reasoning_metadata(state, item);
            state.lifecycle = lifecycle::reasoning_start(
                state.lifecycle.clone(),
                &mut events,
                &format!("{item_id}:0"),
                Some(metadata),
            );
            let mut summary_parts = BTreeMap::new();
            summary_parts.insert(0, ReasoningSummaryStatus::Active);
            state.reasoning_items.insert(
                item_id,
                ReasoningStreamItem {
                    encrypted_content: field(item, "encrypted_content").map(str::to_string),
                    summary_parts,
                    delta_indexes: BTreeSet::new(),
                },
            );
            events
        }
        "function_call" | "custom_tool_call" => {
            let Some(call_id) = field(item, "call_id").filter(|value| !value.is_empty()) else {
                return Vec::new();
            };
            let name = field(item, "name").unwrap_or_default();
            if state.has_pending_tool(&item_id) {
                return Vec::new();
            }
            let custom = kind == "custom_tool_call";
            let metadata = provider_metadata(&state.provider_metadata_key, openai_freeform::metadata(&item_id, custom));
            let mut events = Vec::new();
            let lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
            let key = state.tool_key(&item_id);
            let tool = tool_stream::PendingTool {
                id: call_id.to_string(),
                name: name.to_string(),
                namespace: field(item, "namespace").map(str::to_string),
                input: if custom { String::new() } else { field(item, "arguments").unwrap_or_default().to_string() },
                provider_executed: None,
                provider_metadata: Some(metadata.clone()),
            };
            tool_stream::start(&mut state.tools, key, tool);
            let mut opened_events = Vec::new();
            if custom {
                let raw = raw_input(item);
                let starts = openai_freeform::stream_start(state.custom.get(name), raw);
                if let Some(opened) = tool_stream::append(&mut state.tools, key, &starts) {
                    opened_events = opened.events;
                }
            }
            state.lifecycle = lifecycle;
            events.push(LlmEvent::ToolInputStart {
                id: call_id.to_string(),
                name: name.to_string(),
                namespace: field(item, "namespace").map(str::to_string),
                provider_executed: None,
                provider_metadata: Some(metadata),
            });
            events.extend(opened_events);
            events
        }
        _ => Vec::new(),
    }
}

fn on_function_call_arguments_delta(state: &mut State, event: &Event) -> Result<Vec<LlmEvent>, AiError> {
    let Some(item_id) = field(event, "item_id") else {
        return Ok(Vec::new());
    };
    let Some(key) = state.pending_tool_key(item_id) else {
        return Ok(Vec::new());
    };
    let Some(tool) = state.tools.get(&key).cloned() else {
        return Ok(Vec::new());
    };
    let done = field(event, "type") == Some("response.function_call_arguments.done");
    let final_arguments = if done { field(event, "arguments") } else { None };
    if done && final_arguments.is_none() {
        return Ok(Vec::new());
    }
    if let Some(final_arguments) = final_arguments
        && !final_arguments.starts_with(&tool.input)
    {
        let mut replacement = tool;
        replacement.input = final_arguments.to_string();
        state.tools.insert(key, replacement);
        return Ok(Vec::new());
    }
    let delta = match final_arguments {
        Some(final_arguments) => final_arguments[tool.input.len()..].to_string(),
        None => field(event, "delta").unwrap_or_default().to_string(),
    };
    if delta.is_empty() {
        return Ok(Vec::new());
    }
    let missing = format!("{} tool argument delta is missing its tool call", state.name);
    let result = tool_stream::append_existing(&state.id, &mut state.tools, key, &delta, &missing)?;
    let mut events = Vec::new();
    if !result.events.is_empty() {
        state.lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
    }
    events.extend(result.events);
    Ok(events)
}

fn on_output_item_done(state: &mut State, item: &Value) -> Result<Vec<LlmEvent>, AiError> {
    let kind = field(item, "type").unwrap_or_default();
    let item_id = field(item, "id").unwrap_or_default().to_string();
    if kind == "compaction" {
        let Some(encrypted) = field(item, "encrypted_content") else {
            return Err(shared::event_error(&state.id, "Compaction output is missing its encrypted content", None));
        };
        if state.completed_compactions.contains(&item_id) {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        state.lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
        events.push(LlmEvent::Compaction(CompactionPart {
            provider: state.provider.clone(),
            id: Some(item_id.clone()),
            encrypted: Some(encrypted.to_string()),
            text: None,
        }));
        state.completed_compactions.insert(item_id);
        return Ok(events);
    }
    if kind == "message" {
        let active = state.message.as_ref().is_some_and(|message| message.id == item_id);
        let item_phase = message_phase(item.get("phase"));
        let phase = if item_phase.is_none() && active {
            state.message.as_ref().and_then(|message| message.phase.clone())
        } else {
            item_phase
        };
        let parts = item.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        let mut content = Vec::new();
        for part in parts {
            match field(part, "type") {
                Some("output_text") => {
                    if let Some(text) = field(part, "text") {
                        content.push(text.to_string());
                    }
                }
                Some("refusal") => {
                    if let Some(refusal) = field(part, "refusal") {
                        content.push(refusal.to_string());
                    }
                }
                _ => {}
            }
        }
        let text = (!content.is_empty()).then(|| content.concat());
        let metadata = text_metadata(state, &item_id, phase.as_ref());
        let mut events = Vec::new();
        let lifecycle = if text.as_deref().is_some_and(|text| !text.is_empty()) {
            lifecycle::text_start(state.lifecycle.clone(), &mut events, &item_id, Some(metadata.clone()))
        } else {
            state.lifecycle.clone()
        };
        state.lifecycle = lifecycle::text_end(lifecycle, &mut events, &item_id, Some(metadata), text);
        if active {
            state.message = None;
        }
        return Ok(events);
    }
    if kind == "function_call" || kind == "custom_tool_call" {
        let (Some(call_id), Some(name)) = (
            field(item, "call_id").filter(|value| !value.is_empty()),
            field(item, "name").filter(|value| !value.is_empty()),
        ) else {
            return Ok(Vec::new());
        };
        let custom = kind == "custom_tool_call";
        let metadata = provider_metadata(&state.provider_metadata_key, openai_freeform::metadata(&item_id, custom));
        let final_custom = if custom { state.custom.get(name).cloned() } else { None };
        let raw = raw_input(item);
        let input = if custom {
            Some(openai_freeform::from_raw(final_custom.as_ref(), raw))
        } else {
            field(item, "arguments").map(str::to_string)
        };
        let key = state.tool_key(&item_id);
        let registered = state.tools.contains_key(&key);
        let mut closed_events = Vec::new();
        let mut tools = std::mem::take(&mut state.tools);
        if custom {
            let closed = openai_freeform::close(
                tools,
                [key],
                Some(openai_freeform::Final { custom: final_custom.as_ref(), raw }),
            );
            tools = closed.tools;
            closed_events = closed.events;
        }
        if !registered {
            tool_stream::start(
                &mut tools,
                key,
                tool_stream::PendingTool {
                    id: call_id.to_string(),
                    name: name.to_string(),
                    namespace: field(item, "namespace").map(str::to_string),
                    input: String::new(),
                    provider_executed: None,
                    provider_metadata: Some(metadata.clone()),
                },
            );
        }
        let mut finished = closed_events;
        match tool_stream::finish(&state.id, &mut tools, key, input.as_deref()) {
            Ok(result) => {
                finished.extend(result.events);
                state.tools = tools;
            }
            Err(error) => {
                state.tools = tool_stream::empty();
                return Err(error);
            }
        }
        let result_events = if registered || finished.is_empty() {
            finished
        } else {
            let mut events = vec![LlmEvent::ToolInputStart {
                id: call_id.to_string(),
                name: name.to_string(),
                namespace: field(item, "namespace").map(str::to_string),
                provider_executed: None,
                provider_metadata: Some(metadata),
            }];
            events.extend(finished);
            events
        };
        state.has_function_call = result_events
            .iter()
            .any(|event| matches!(event, LlmEvent::ToolCall { .. } | LlmEvent::ToolInputError { .. }))
            || state.has_function_call;
        let mut events = Vec::new();
        if !result_events.is_empty() {
            state.lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
        }
        events.extend(result_events);
        return Ok(events);
    }
    if kind == "reasoning" {
        let metadata = reasoning_metadata(state, item);
        let summary = item
            .get("summary")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().map(decode_summary_part).collect::<Vec<Option<String>>>())
            .unwrap_or_default();
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().filter_map(decode_reasoning_text).collect::<Vec<String>>())
            .unwrap_or_default();
        let item_text = join_reasoning_text(&summary).or_else(|| join_reasoning_text_strings(&content));
        let mut events = Vec::new();
        if let Some(reasoning_item) = state.reasoning_items.get(&item_id).cloned() {
            let fragments =
                reasoning_item.summary_parts.iter().map(|(index, status)| (*index, *status)).collect::<Vec<_>>();
            let mut lifecycle = state.lifecycle.clone();
            for (index, status) in &fragments {
                if *status == ReasoningSummaryStatus::Concluded {
                    continue;
                }
                let final_text =
                    if fragments.len() == 1 { item_text.clone() } else { summary.get(*index).cloned().flatten() };
                lifecycle = lifecycle::reasoning_end(
                    lifecycle,
                    &mut events,
                    &format!("{item_id}:{index}"),
                    Some(metadata.clone()),
                    final_text.filter(|text| !text.is_empty()),
                );
            }
            state.lifecycle = lifecycle;
            state.reasoning_items.remove(&item_id);
            return Ok(events);
        }
        state.lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
        events.push(LlmEvent::ReasoningStart { id: item_id.clone(), provider_metadata: Some(metadata.clone()) });
        events.push(LlmEvent::ReasoningEnd { id: item_id, text: item_text, provider_metadata: Some(metadata) });
        return Ok(events);
    }
    Ok(Vec::new())
}

fn decode_summary_part(part: &Value) -> Option<String> {
    if field(part, "type") != Some("summary_text") {
        return None;
    }
    field(part, "text").map(str::to_string)
}

fn decode_reasoning_text(part: &Value) -> Option<String> {
    if field(part, "type") != Some("reasoning_text") {
        return None;
    }
    field(part, "text").map(str::to_string)
}

fn join_reasoning_text(parts: &[Option<String>]) -> Option<String> {
    if !parts.iter().any(|part| part.as_deref().is_some_and(|text| !text.is_empty())) {
        return None;
    }
    Some(parts.iter().flatten().cloned().collect::<Vec<String>>().join("\n\n"))
}

fn join_reasoning_text_strings(parts: &[String]) -> Option<String> {
    if !parts.iter().any(|part| !part.is_empty()) {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn on_response_finish(state: &mut State, event: &Event) -> Result<Vec<LlmEvent>, AiError> {
    let mut events = Vec::new();
    if event.get("type").and_then(Value::as_str) == Some("response.completed") {
        let output = event
            .get("response")
            .and_then(|response| response.get("output"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let step_started = state.lifecycle.step_started;
        for (index, item) in output.iter().enumerate() {
            let resolved = resolve_item(state, item, Some(index as i64));
            let kind = field(&resolved, "type").unwrap_or_default();
            let item_id = field(&resolved, "id").unwrap_or_default();
            if kind == "compaction" && step_started && !state.completed_compactions.contains(item_id) {
                return Err(shared::event_error(
                    &state.id,
                    "Cannot recover a compaction checkpoint after output has been emitted",
                    None,
                ));
            }
            let recoverable = kind == "compaction"
                || ((kind == "function_call" || kind == "custom_tool_call") && state.has_pending_tool(item_id));
            if !recoverable {
                continue;
            }
            events.extend(on_output_item_done(state, &resolved)?);
        }
        let mut tools = std::mem::take(&mut state.tools);
        let pending = tool_stream::finish_all(&state.id, &mut tools)?;
        state.tools = tools;
        state.has_function_call = state.has_function_call
            || pending
                .events
                .iter()
                .any(|event| matches!(event, LlmEvent::ToolCall { .. } | LlmEvent::ToolInputError { .. }));
        events.extend(pending.events);
    }
    let normalized = map_finish_reason(event, state.has_function_call);
    let response = event.get("response").filter(|response| response.is_object());
    let raw = response
        .and_then(|response| response.get("incomplete_details"))
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let category = if normalized == FinishReason::ContentFilter {
        content_filter_category(response.and_then(|response| response.get("content_filters")))
    } else {
        None
    };
    let usage = {
        let usage = response.and_then(|response| response.get("usage"));
        let mut frames = vec![event];
        if let Some(response) = response {
            frames.push(response);
        }
        map_usage(usage, &state.provider_metadata_key, &frames)
    };
    let finish_metadata = {
        let response_id = response.and_then(|response| response.get("id"));
        let service_tier = response.and_then(|response| response.get("service_tier"));
        let truthy = |value: Option<&Value>| value.and_then(Value::as_str).is_some_and(|value| !value.is_empty());
        if truthy(response_id) || truthy(service_tier) {
            let mut metadata = Map::new();
            if let Some(value) = response_id {
                metadata.insert("responseId".to_string(), value.clone());
            }
            if let Some(value) = service_tier {
                metadata.insert("serviceTier".to_string(), value.clone());
            }
            Some(provider_metadata(&state.provider_metadata_key, metadata))
        } else {
            None
        }
    };
    let reason = FinishReasonDetails { normalized, raw, category, explanation: None };
    state.lifecycle = lifecycle::finish(state.lifecycle.clone(), &mut events, reason, usage, finish_metadata);
    Ok(events)
}

pub struct ErrorDetail {
    pub message: Option<String>,
    pub code: Option<String>,
}

pub fn error_detail(event: &Event) -> ErrorDetail {
    let raw = event
        .get("error")
        .filter(|value| !value.is_null())
        .or_else(|| event.get("response").and_then(|response| response.get("error")).filter(|value| !value.is_null()));
    let nested = match raw {
        Some(Value::String(message)) => {
            let mut value = Map::new();
            value.insert("message".to_string(), Value::String(message.clone()));
            Some(Value::Object(value))
        }
        Some(value) if value.is_object() => Some(value.clone()),
        _ => None,
    };
    ErrorDetail {
        message: as_text(event.get("message"))
            .or_else(|| nested.as_ref().and_then(|value| as_text(value.get("message")))),
        code: as_text(event.get("code")).or_else(|| nested.as_ref().and_then(|value| as_text(value.get("code")))),
    }
}

fn as_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub fn provider_failure(event: &Event, fallback: &str, body: Option<&str>) -> AiError {
    let detail = error_detail(event);
    let summary = match (detail.message.clone(), detail.code.clone()) {
        (Some(message), Some(code)) => Some(format!("{code}: {message}")),
        (Some(message), None) => Some(message),
        (None, Some(code)) => Some(code),
        (None, None) => None,
    };
    let body = body.map(str::to_string).unwrap_or_else(|| crate::json::encode_body(event));
    let message = summary.unwrap_or_else(|| if body == "{}" { fallback.to_string() } else { body.clone() });
    let status = event
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .or_else(|| event.get("status_code").and_then(Value::as_u64).and_then(|status| u16::try_from(status).ok()));
    let special = event.get("type").and_then(Value::as_str) == Some("error")
        && event.get("error").is_none()
        && event.get("response").is_none()
        && detail.message.is_none()
        && detail.code.is_none()
        && status.is_none();
    if special {
        return AiError {
            reason: AiErrorReason::ProviderInternal { message, body: Some(body), http: None, retry_after_ms: None },
        };
    }
    let reason = classify_provider_failure(ProviderFailure {
        message,
        status,
        raw_body: Some(body),
        ..ProviderFailure::default()
    });
    AiError { reason }
}

pub fn step(state: &mut State, event: &Event) -> Result<Vec<LlmEvent>, AiError> {
    let Some(kind) = event.get("type").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    if kind == "response.output_text.delta" || kind == "response.output_text.done" {
        let Some(item_id) = field(event, "item_id") else {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        };
        return Ok(if kind == "response.output_text.delta" {
            on_output_text_delta(state, event, item_id)
        } else {
            on_output_text_done(state, event, item_id)
        });
    }
    if kind == "response.refusal.delta" || kind == "response.refusal.done" {
        let value = if kind == "response.refusal.delta" { field(event, "delta") } else { field(event, "refusal") };
        let (Some(item_id), Some(value)) = (field(event, "item_id"), value) else {
            return Err(shared::event_error(&state.id, format!("{kind} is malformed"), None));
        };
        return Ok(if kind == "response.refusal.delta" {
            on_output_text_delta(state, event, item_id)
        } else {
            let mut patched = event.clone();
            patched["text"] = Value::String(value.to_string());
            on_output_text_done(state, &patched, item_id)
        });
    }
    if kind == "response.reasoning.delta" || kind == "response.reasoning_summary_text.delta" {
        let Some(item_id) = field(event, "item_id") else {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        };
        return Ok(on_reasoning_delta(state, event, item_id));
    }
    if kind == "response.reasoning.done"
        || kind == "response.reasoning_summary_text.done"
        || kind == "response.reasoning_text.done"
    {
        let Some(item_id) = field(event, "item_id") else {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        };
        return Ok(on_reasoning_done(state, event, item_id));
    }
    if kind == "response.reasoning_summary_part.added" {
        if field(event, "item_id").is_none() {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        }
        let (Some(item_id), Some(index)) =
            (field(event, "item_id"), event.get("summary_index").and_then(Value::as_u64))
        else {
            return Ok(Vec::new());
        };
        return Ok(start_reasoning_summary_part(state, item_id, index as usize));
    }
    if kind == "response.reasoning_summary_part.done" {
        if field(event, "item_id").is_none() {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        }
        return Ok(on_reasoning_summary_part_done(state, event));
    }
    if kind == "response.output_item.added" {
        let item = event.get("item").filter(|item| item.is_object());
        if let Some(item) = item
            && field(item, "type") == Some("reasoning")
            && let Some(item_id) = field(item, "id")
            && !state.reasoning_items.contains_key(item_id)
            && !state.lifecycle.reasoning.is_empty()
        {
            return Err(shared::event_error(
                &state.id,
                format!("{kind} started reasoning before the previous item ended"),
                None,
            ));
        }
        if let (Some(index), Some(item)) = (event.get("output_index").and_then(Value::as_i64), item)
            && let Some(item_id) = field(item, "id")
        {
            state.output_items.insert(index, item_id.to_string());
        }
        return Ok(item.map(|item| on_output_item_added(state, item)).unwrap_or_default());
    }
    if kind == "response.function_call_arguments.delta" || kind == "response.function_call_arguments.done" {
        if field(event, "item_id").is_none() {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        }
        return on_function_call_arguments_delta(state, event);
    }
    if kind == "response.custom_tool_call_input.delta" {
        let Some(item_id) = field(event, "item_id") else {
            return Err(shared::event_error(&state.id, format!("{kind} is missing item_id"), None));
        };
        let mut patched = event.clone();
        patched["type"] = Value::String("response.function_call_arguments.delta".to_string());
        patched["delta"] = Value::String(openai_freeform::stream_delta(field(event, "delta").unwrap_or_default()));
        patched["item_id"] = Value::String(item_id.to_string());
        return on_function_call_arguments_delta(state, &patched);
    }
    if kind == "response.output_item.done" {
        return match event.get("item").filter(|item| item.is_object()) {
            Some(item) => on_output_item_done(state, item),
            None => Ok(Vec::new()),
        };
    }
    if kind == "response.completed" || kind == "response.incomplete" {
        return on_response_finish(state, event);
    }
    if kind == "response.failed" {
        return Err(provider_failure(event, &format!("{} response failed", state.name), None));
    }
    if kind == "error" {
        return Err(provider_failure(event, &format!("{} stream error", state.name), None));
    }
    Ok(Vec::new())
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = parse_event(&self.id, &frame)?;
        let normalized = normalize(self, &event);
        step(self, &normalized)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        parse_event(&self.id, frame).map(|event| terminal(&event)).unwrap_or(false)
    }
}

pub struct OpenResponses;

impl Protocol for OpenResponses {
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

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(initial(request, context, &BASE_ADAPTER))
    }
}

fn is_hosted_tool_item(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return false;
    };
    let id = object.get("id").and_then(Value::as_str).is_some();
    let status = shared::optional_string(object, "status");
    match kind {
        "web_search_call" => id && status && shared::optional_null_object(object, "action"),
        "file_search_call" => {
            id && status && optional_string_array(object, "queries") && optional_null_object_array(object, "results")
        }
        "code_interpreter_call" => {
            id && status
                && shared::optional_null_string(object, "code")
                && shared::optional_null_string(object, "container_id")
                && optional_null_object_array(object, "outputs")
        }
        "mcp_call" => {
            id && status
                && shared::optional_string(object, "server_label")
                && shared::optional_string(object, "name")
                && shared::optional_string(object, "arguments")
                && shared::optional_null_string(object, "output")
        }
        _ => false,
    }
}

fn optional_string_array(object: &Map<String, Value>, key: &str) -> bool {
    match object.get(key) {
        None => true,
        Some(Value::Array(values)) => values.iter().all(Value::is_string),
        Some(_) => false,
    }
}

fn optional_null_object_array(object: &Map<String, Value>, key: &str) -> bool {
    match object.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::Array(values)) => values.iter().all(Value::is_object),
        Some(_) => false,
    }
}
