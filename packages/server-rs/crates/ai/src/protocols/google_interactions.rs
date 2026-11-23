use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::protocols::shared;
use crate::protocols::utils::{gemini_generate_content, lifecycle, media_input, tool_stream};
use crate::provider_error::{ProviderFailure, classify_provider_failure, provider_error_message};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, AiErrorReason, ContentPart, FinishReason, FinishReasonDetails, InvalidProviderOutputClassification,
    LlmEvent, LlmRequest, MediaAsset, MediaSource, Message, ProviderMetadata, SanitizerKind, SystemPart, ToolChoice,
    ToolChoiceType, ToolContent, ToolDefinition, ToolResultPart, ToolResultValue, Usage,
};

pub const ID: &str = "google-interactions";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const PATH: &str = "/interactions";

const LABEL: &str = "Google Interactions";
const CONTINUATION: &str = "requires_continuation";
const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

const OFFICE_DOCUMENT_MIME_TYPES: [&str; 10] = [
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.oasis.opendocument.spreadsheet",
];

const USAGE_FIELDS: [&str; 6] = [
    "total_input_tokens",
    "total_cached_tokens",
    "total_output_tokens",
    "total_thought_tokens",
    "total_tokens",
    "raw_prompt_token",
];

const RECOVERABLE_CODES: [&str; 4] =
    ["MALFORMED_FUNCTION_CALL", "MALFORMED_TOOL_CALL", "UNEXPECTED_TOOL_CALL", "MALFORMED_RESPONSE"];
const INVALID_OUTPUT_CODES: [&str; 3] = ["MISSING_THOUGHT_SIGNATURE", "TOO_MANY_TOOL_CALLS", "NO_IMAGE"];

pub struct GoogleInteractions;

struct Options {
    previous_interaction_id: Option<String>,
    store: Option<bool>,
    thinking_level: Option<String>,
    thinking_summaries: Option<String>,
    service_tier: Option<String>,
    safety_settings: Option<Vec<SafetySetting>>,
}

struct SafetySetting {
    category: String,
    threshold: String,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    Ok(Options {
        previous_interaction_id: optional_string(input.get("previousInteractionId"), "previousInteractionId")?,
        store: optional_bool(input.get("store"), "store")?,
        thinking_level: optional_string(input.get("thinkingLevel"), "thinkingLevel")?,
        thinking_summaries: optional_string(input.get("thinkingSummaries"), "thinkingSummaries")?,
        service_tier: optional_string(input.get("serviceTier"), "serviceTier")?,
        safety_settings: optional_safety_settings(input.get("safetySettings"))?,
    })
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

fn optional_safety_settings(value: Option<&Value>) -> Result<Option<Vec<SafetySetting>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(items) = value.as_array() else {
        return Err(option_error("safetySettings"));
    };
    let mut settings = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            return Err(option_error("safetySettings"));
        };
        let Some(category) = object.get("category").and_then(Value::as_str) else {
            return Err(option_error("safetySettings"));
        };
        let Some(threshold) = object.get("threshold").and_then(Value::as_str) else {
            return Err(option_error("safetySettings"));
        };
        settings.push(SafetySetting { category: category.to_string(), threshold: threshold.to_string() });
    }
    Ok(Some(settings))
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("{ID} providerOptions {field} is invalid"))
}

fn signature(metadata: Option<&ProviderMetadata>, key: &str) -> Option<String> {
    shared::provider_metadata_value(metadata, key)?.get("interactionSignature")?.as_str().map(str::to_string)
}

fn signature_metadata(key: &str, signature: Option<&str>) -> ProviderMetadata {
    let mut inner = Map::new();
    if let Some(signature) = signature {
        inner.insert("interactionSignature".to_string(), Value::String(signature.to_string()));
    }
    let mut metadata = ProviderMetadata::new();
    metadata.insert(key.to_string(), Value::Object(inner));
    metadata
}

fn text_content(text: &str) -> Value {
    let mut content = Map::new();
    content.insert("type".to_string(), Value::String("text".to_string()));
    content.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(content)
}

fn is_http_url(url: &str) -> bool {
    url.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || url.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn media_content(asset: &MediaAsset) -> Result<Value, AiError> {
    let media_type = shared::media_type(asset);
    let kind = shared::media_kind(&media_type);
    if !matches!(kind, "image" | "audio" | "video")
        && media_type != "application/pdf"
        && media_type != "text/csv"
        && !OFFICE_DOCUMENT_MIME_TYPES.contains(&media_type.as_str())
    {
        return Err(shared::invalid_request(format!("{LABEL} does not support {media_type} document input")));
    }
    let content_type = match kind {
        "image" | "audio" | "video" => kind,
        _ => "document",
    };
    let uri = match &asset.source {
        MediaSource::Url { url, .. } if kind == "video" && is_http_url(url) => Some(url.clone()),
        _ => media_input::ref_id(asset, "google"),
    };
    if let Some(uri) = uri {
        let mut content = Map::new();
        content.insert("type".to_string(), Value::String(content_type.to_string()));
        content.insert("uri".to_string(), Value::String(uri));
        content.insert("mime_type".to_string(), Value::String(media_type));
        return Ok(Value::Object(content));
    }
    let inline = shared::require_inline_media(LABEL, asset)?;
    let mut content = Map::new();
    content.insert("type".to_string(), Value::String(content_type.to_string()));
    content.insert("data".to_string(), Value::String(inline.base64));
    content.insert("mime_type".to_string(), Value::String(inline.mime));
    Ok(Value::Object(content))
}

fn lower_tool_result(part: &ToolResultPart) -> Result<Value, AiError> {
    match &part.result {
        ToolResultValue::Json { value } if value.is_object() => Ok(value.clone()),
        ToolResultValue::Content { value } => {
            let mut items = Vec::new();
            for item in value {
                match item {
                    ToolContent::Text { text } => items.push(text_content(text)),
                    ToolContent::File { .. } => {
                        let media = gemini_generate_content::tool_media(item)
                            .ok_or_else(|| shared::invalid_request(format!("{LABEL} cannot lower tool file media")))?;
                        items.push(media_content(&media.media)?);
                    }
                }
            }
            Ok(Value::Array(items))
        }
        _ => Ok(Value::String(shared::tool_result_text(part))),
    }
}

fn append_content(steps: &mut Vec<Value>, start: usize, kind: &str, content: Value) {
    let merge =
        steps.len() > start && steps.last().and_then(|step| step.get("type")).and_then(Value::as_str) == Some(kind);
    if merge {
        if let Some(contents) = steps.last_mut().and_then(|step| step.get_mut("content")).and_then(Value::as_array_mut)
        {
            contents.push(content);
        }
        return;
    }
    let mut step = Map::new();
    step.insert("type".to_string(), Value::String(kind.to_string()));
    step.insert("content".to_string(), Value::Array(vec![content]));
    steps.push(Value::Object(step));
}

fn lower_messages(request: &LlmRequest, metadata_key: &str) -> Result<Vec<Value>, AiError> {
    let mut steps = Vec::new();
    for message in &request.messages {
        if matches!(message, Message::System(_)) {
            let part = shared::wrapped_system_update(ID, message)?;
            let mut step = Map::new();
            step.insert("type".to_string(), Value::String("user_input".to_string()));
            step.insert("content".to_string(), Value::Array(vec![text_content(&part.text)]));
            steps.push(Value::Object(step));
            continue;
        }
        let assistant = matches!(message, Message::Assistant(_));
        let content_kind = if assistant { "model_output" } else { "user_input" };
        let start = steps.len();
        let mut has_signed_step = false;
        for part in message_content(message) {
            if matches!(message, Message::Tool(_)) {
                let ContentPart::ToolResult(part) = part else {
                    return Err(unsupported_content("tool"));
                };
                let mut step = Map::new();
                step.insert("type".to_string(), Value::String("function_result".to_string()));
                step.insert("call_id".to_string(), Value::String(part.id.clone()));
                step.insert("name".to_string(), Value::String(part.name.clone()));
                step.insert("result".to_string(), lower_tool_result(part)?);
                if matches!(part.result, ToolResultValue::Error { .. }) {
                    step.insert("is_error".to_string(), Value::Bool(true));
                }
                steps.push(Value::Object(step));
                continue;
            }
            match part {
                ContentPart::Text(part) => {
                    append_content(&mut steps, start, content_kind, text_content(&part.text));
                }
                ContentPart::Media(part) => {
                    append_content(&mut steps, start, content_kind, media_content(&part.media)?);
                }
                ContentPart::Reasoning(part) if assistant => {
                    let thought_signature = signature(part.provider_metadata.as_ref(), metadata_key);
                    let mut step = Map::new();
                    step.insert("type".to_string(), Value::String("thought".to_string()));
                    if let Some(thought_signature) = &thought_signature {
                        step.insert("signature".to_string(), Value::String(thought_signature.clone()));
                    }
                    if !part.text.is_empty() {
                        step.insert("summary".to_string(), Value::Array(vec![text_content(&part.text)]));
                    }
                    if thought_signature.is_some() {
                        has_signed_step = true;
                    }
                    steps.push(Value::Object(step));
                }
                ContentPart::ToolCall(part) if assistant => {
                    let tool_signature = signature(part.provider_metadata.as_ref(), metadata_key);
                    let mut step = Map::new();
                    step.insert("type".to_string(), Value::String("function_call".to_string()));
                    step.insert("id".to_string(), Value::String(part.id.clone()));
                    step.insert("name".to_string(), Value::String(part.name.clone()));
                    step.insert("arguments".to_string(), part.input.clone());
                    let emitted = match &tool_signature {
                        Some(signature) => Some(signature.clone()),
                        None if !has_signed_step => Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_string()),
                        None => None,
                    };
                    if let Some(signature) = emitted {
                        step.insert("signature".to_string(), Value::String(signature));
                    }
                    if tool_signature.is_some() {
                        has_signed_step = true;
                    }
                    steps.push(Value::Object(step));
                }
                _ => return Err(unsupported_content(if assistant { "assistant" } else { "user" })),
            }
        }
    }
    Ok(steps)
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn unsupported_content(role: &str) -> AiError {
    let types = match role {
        "user" => "text and media",
        "tool" => "tool-result",
        _ => "text, media, reasoning, and tool-call",
    };
    shared::invalid_request(format!("{ID} {role} messages only support {types} content for now"))
}

fn lower_tool(tool: &ToolDefinition) -> Value {
    let mut lowered = Map::new();
    lowered.insert("type".to_string(), Value::String("function".to_string()));
    lowered.insert("name".to_string(), Value::String(tool.name.clone()));
    lowered.insert("description".to_string(), Value::String(tool.description.clone()));
    lowered.insert("parameters".to_string(), tool.input_schema.clone());
    Value::Object(lowered)
}

fn lower_tool_choice(tool_choice: &ToolChoice) -> Result<Value, AiError> {
    match tool_choice.r#type {
        ToolChoiceType::Auto => Ok(Value::String("auto".to_string())),
        ToolChoiceType::None => Ok(Value::String("none".to_string())),
        ToolChoiceType::Required => Ok(Value::String("any".to_string())),
        ToolChoiceType::Tool => {
            let name = tool_choice
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| shared::invalid_request(format!("{ID} tool choice requires a tool name")))?;
            let mut allowed = Map::new();
            allowed.insert("mode".to_string(), Value::String("any".to_string()));
            allowed.insert("tools".to_string(), Value::Array(vec![Value::String(name.to_string())]));
            let mut choice = Map::new();
            choice.insert("allowed_tools".to_string(), Value::Object(allowed));
            Ok(Value::Object(choice))
        }
    }
}

fn lower_safety_settings(settings: &[SafetySetting]) -> Value {
    Value::Array(
        settings
            .iter()
            .map(|setting| {
                let category = setting.category.strip_prefix("HARM_CATEGORY_").unwrap_or(&setting.category);
                let mut lowered = Map::new();
                lowered.insert("type".to_string(), Value::String(category.to_lowercase()));
                lowered.insert("threshold".to_string(), Value::String(setting.threshold.to_lowercase()));
                Value::Object(lowered)
            })
            .collect(),
    )
}

fn generation_config(request: &LlmRequest, options: &Options, tool_choice: Option<Value>) -> Value {
    let generation = &request.generation;
    let mut config = Map::new();
    if let Some(max_tokens) = generation.max_tokens.clone() {
        config.insert("max_output_tokens".to_string(), Value::Number(max_tokens));
    }
    if let Some(temperature) = generation.temperature.clone() {
        config.insert("temperature".to_string(), Value::Number(temperature));
    }
    if let Some(top_p) = generation.top_p.clone() {
        config.insert("top_p".to_string(), Value::Number(top_p));
    }
    if let Some(seed) = generation.seed.clone() {
        config.insert("seed".to_string(), Value::Number(seed));
    }
    if let Some(stop) = generation.stop.as_ref() {
        config.insert(
            "stop_sequences".to_string(),
            Value::Array(stop.iter().map(|value| Value::String(value.clone())).collect()),
        );
    }
    if let Some(thinking_level) = &options.thinking_level {
        config.insert("thinking_level".to_string(), Value::String(thinking_level.clone()));
    }
    if let Some(thinking_summaries) = &options.thinking_summaries {
        config.insert("thinking_summaries".to_string(), Value::String(thinking_summaries.clone()));
    }
    if let Some(tool_choice) = tool_choice {
        config.insert("tool_choice".to_string(), tool_choice);
    }
    Value::Object(config)
}

fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let options = decode_options(request.provider_options.as_ref())?;
    let (tools, flattened) = shared::flatten_tool_request(request);
    if tools.iter().any(|tool| tool.native.is_some()) {
        return Err(shared::invalid_request("Google Interactions hosted tools are not supported"));
    }
    if request.generation.top_k.is_some()
        || request.generation.frequency_penalty.is_some()
        || request.generation.presence_penalty.is_some()
    {
        return Err(shared::invalid_request(
            "Google Interactions does not support topK, frequencyPenalty, or presencePenalty",
        ));
    }
    let tool_choice = match request.tool_choice.as_ref() {
        Some(tool_choice) => Some(lower_tool_choice(tool_choice)?),
        None => None,
    };
    let metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("input".to_string(), Value::Array(lower_messages(&flattened, &metadata_key)?));
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert("store".to_string(), Value::Bool(options.store.unwrap_or(false)));
    if let Some(previous_interaction_id) = &options.previous_interaction_id {
        body.insert("previous_interaction_id".to_string(), Value::String(previous_interaction_id.clone()));
    }
    if !request.system.is_empty() {
        let text = shared::join_text(request.system.iter().map(|part| {
            let SystemPart::Text { text, .. } = part;
            text.clone()
        }));
        body.insert("system_instruction".to_string(), Value::String(text));
    }
    if let Some(service_tier) = &options.service_tier {
        body.insert("service_tier".to_string(), Value::String(service_tier.clone()));
    }
    if let Some(safety_settings) = options.safety_settings.as_ref() {
        body.insert("safety_settings".to_string(), lower_safety_settings(safety_settings));
    }
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools.iter().map(lower_tool).collect()));
    }
    body.insert("generation_config".to_string(), generation_config(request, &options, tool_choice));
    Ok(Value::Object(body))
}

#[derive(Clone)]
struct RawUsage(Value);

impl RawUsage {
    fn number(&self, key: &str) -> Option<u64> {
        self.0.get(key).and_then(Value::as_u64)
    }

    fn value(&self) -> Value {
        self.0.clone()
    }
}

fn decode_usage(value: &Value) -> Option<RawUsage> {
    let object = value.as_object()?;
    for key in USAGE_FIELDS {
        if !matches!(object.get(key), None | Some(Value::Null) | Some(Value::Number(_))) {
            return None;
        }
    }
    Some(RawUsage(value.clone()))
}

fn decode_usage_field(object: &Map<String, Value>, key: &str) -> Option<Option<RawUsage>> {
    match object.get(key) {
        None => Some(None),
        Some(value) => Some(Some(decode_usage(value)?)),
    }
}

fn sum_usage(hops: &[Option<&RawUsage>], field: fn(&RawUsage) -> Option<u64>) -> Option<u64> {
    shared::sum_tokens(&hops.iter().map(|hop| hop.and_then(field)).collect::<Vec<Option<u64>>>())
}

fn map_usage(usage: Option<&RawUsage>, key: &str, continued: &[Option<RawUsage>]) -> Option<Usage> {
    if usage.is_none() && continued.iter().all(Option::is_none) {
        return None;
    }
    let first = if continued.is_empty() { usage } else { continued.first().and_then(|hop| hop.as_ref()) };
    let hops = continued.iter().map(|hop| hop.as_ref()).chain(std::iter::once(usage)).collect::<Vec<_>>();
    let input = first.and_then(|usage| usage.number("total_input_tokens"));
    let cached = first.and_then(|usage| usage.number("total_cached_tokens"));
    let reasoning = sum_usage(&hops, |usage| usage.number("total_thought_tokens"));
    let text = sum_usage(&hops, |usage| usage.number("total_output_tokens"));
    let output = if continued.is_empty() {
        text.map(|text| text + reasoning.unwrap_or(0))
    } else {
        shared::sum_tokens(&[text, reasoning])
    };
    let total = if continued.is_empty() {
        usage.and_then(|usage| usage.number("total_tokens"))
    } else {
        shared::total_tokens(input, output, None)
    };
    let usage_metadata = if continued.is_empty() {
        usage.map(RawUsage::value).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        let mut metadata = Map::new();
        if let Some(value) = sum_usage(&hops, |usage| usage.number("total_input_tokens")) {
            metadata.insert("total_input_tokens".to_string(), Value::Number(Number::from(value)));
        }
        if let Some(value) = sum_usage(&hops, |usage| usage.number("total_cached_tokens")) {
            metadata.insert("total_cached_tokens".to_string(), Value::Number(Number::from(value)));
        }
        if let Some(value) = text {
            metadata.insert("total_output_tokens".to_string(), Value::Number(Number::from(value)));
        }
        if let Some(value) = reasoning {
            metadata.insert("total_thought_tokens".to_string(), Value::Number(Number::from(value)));
        }
        if let Some(value) = sum_usage(&hops, |usage| usage.number("total_tokens")) {
            metadata.insert("total_tokens".to_string(), Value::Number(Number::from(value)));
        }
        metadata.insert("requests".to_string(), Value::Number(Number::from(hops.len() as u64)));
        Value::Object(metadata)
    };
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(key.to_string(), usage_metadata);
    Some(Usage {
        context_tokens: first.and_then(|usage| usage.number("raw_prompt_token")).or(input),
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: shared::subtract_tokens(input, cached),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: None,
        reasoning_tokens: reasoning,
        total_tokens: total,
        cost: None,
        provider_metadata: Some(provider_metadata),
    })
}

#[derive(Clone)]
struct OutputContent {
    kind: String,
    text: Option<String>,
}

#[derive(Clone)]
struct OutputStep {
    kind: String,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<Value>,
    signature: Option<String>,
    summary: Option<Vec<String>>,
    content: Option<Vec<OutputContent>>,
}

enum Delta {
    Text(String),
    Arguments(String),
    ThoughtSignature(String),
    ThoughtSummary(String),
    Unknown(String),
}

struct InteractionError {
    code: Option<String>,
    message: Option<String>,
}

struct Interaction {
    id: Option<String>,
    status: String,
    usage: Option<RawUsage>,
    errors: Option<Vec<InteractionError>>,
}

enum Event {
    StepStart { index: Number, step: OutputStep },
    StepDelta { index: Number, delta: Delta },
    StepStop { index: Number },
    InteractionCreated,
    StatusUpdate { usage: Option<RawUsage> },
    Completed { interaction: Interaction },
    Error { error: Option<Value> },
    Unknown,
}

struct ErrorDetail {
    code: Option<String>,
    error_type: Option<String>,
    message: Option<String>,
}

fn field_string(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn field_null_string(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn decode_output_step(object: &Map<String, Value>) -> Option<OutputStep> {
    let kind = object.get("type")?.as_str()?.to_string();
    let id = field_string(object, "id")?;
    let name = field_string(object, "name")?;
    let arguments = match object.get("arguments") {
        None => None,
        Some(value) if value.is_object() => Some(value.clone()),
        Some(_) => return None,
    };
    let signature = field_string(object, "signature")?;
    let summary = match object.get("summary") {
        None => None,
        Some(Value::Array(items)) => {
            let mut texts = Vec::new();
            for item in items {
                let item = item.as_object()?;
                if item.get("type")?.as_str()? != "text" {
                    return None;
                }
                texts.push(item.get("text")?.as_str()?.to_string());
            }
            Some(texts)
        }
        Some(_) => return None,
    };
    let content = match object.get("content") {
        None => None,
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                let item = item.as_object()?;
                parts.push(OutputContent {
                    kind: item.get("type")?.as_str()?.to_string(),
                    text: field_string(item, "text")?,
                });
            }
            Some(parts)
        }
        Some(_) => return None,
    };
    Some(OutputStep { kind, id, name, arguments, signature, summary, content })
}

fn decode_delta(value: &Value) -> Option<Delta> {
    let object = value.as_object()?;
    match object.get("type")?.as_str()? {
        "text" => Some(Delta::Text(object.get("text")?.as_str()?.to_string())),
        "arguments_delta" => Some(Delta::Arguments(object.get("arguments")?.as_str()?.to_string())),
        "thought_signature" => Some(Delta::ThoughtSignature(object.get("signature")?.as_str()?.to_string())),
        "thought_summary" => {
            let content = object.get("content")?.as_object()?;
            if content.get("type")?.as_str()? != "text" {
                return None;
            }
            Some(Delta::ThoughtSummary(content.get("text")?.as_str()?.to_string()))
        }
        other => Some(Delta::Unknown(other.to_string())),
    }
}

fn decode_interaction(value: &Value) -> Option<Interaction> {
    let object = value.as_object()?;
    let id = field_string(object, "id")?;
    let status = object.get("status")?.as_str()?.to_string();
    field_null_string(object, "continuation_token")?;
    let usage = decode_usage_field(object, "usage")?;
    let errors = match object.get("errors") {
        None => None,
        Some(Value::Array(items)) => {
            let mut errors = Vec::new();
            for item in items {
                let item = item.as_object()?;
                errors.push(InteractionError {
                    code: field_string(item, "code")?,
                    message: field_string(item, "message")?,
                });
            }
            Some(errors)
        }
        Some(_) => return None,
    };
    Some(Interaction { id, status, usage, errors })
}

fn invalid_event(label: &str, frame: &str) -> AiError {
    shared::event_error(ID, format!("Invalid {label} stream event"), Some(frame.to_string()))
}

fn decode_event(frame: &str, label: &str) -> Result<Event, AiError> {
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid_event(label, frame))?;
    let object = value.as_object().ok_or_else(|| invalid_event(label, frame))?;
    let event_type = object.get("event_type").and_then(Value::as_str).ok_or_else(|| invalid_event(label, frame))?;
    match event_type {
        "step.start" => {
            let index =
                object.get("index").and_then(Value::as_number).cloned().ok_or_else(|| invalid_event(label, frame))?;
            let step = object
                .get("step")
                .and_then(Value::as_object)
                .and_then(decode_output_step)
                .ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::StepStart { index, step })
        }
        "step.delta" => {
            let index =
                object.get("index").and_then(Value::as_number).cloned().ok_or_else(|| invalid_event(label, frame))?;
            let delta = object.get("delta").and_then(decode_delta).ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::StepDelta { index, delta })
        }
        "step.stop" => {
            let index =
                object.get("index").and_then(Value::as_number).cloned().ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::StepStop { index })
        }
        "interaction.created" => {
            let interaction =
                object.get("interaction").and_then(Value::as_object).ok_or_else(|| invalid_event(label, frame))?;
            field_string(interaction, "id").ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::InteractionCreated)
        }
        "interaction.status_update" => {
            field_string(object, "interaction_id").ok_or_else(|| invalid_event(label, frame))?;
            object.get("status").and_then(Value::as_str).ok_or_else(|| invalid_event(label, frame))?;
            field_null_string(object, "continuation_token").ok_or_else(|| invalid_event(label, frame))?;
            let usage = decode_usage_field(object, "usage").ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::StatusUpdate { usage })
        }
        "interaction.completed" => {
            let interaction =
                object.get("interaction").and_then(decode_interaction).ok_or_else(|| invalid_event(label, frame))?;
            Ok(Event::Completed { interaction })
        }
        "error" => Ok(Event::Error { error: object.get("error").cloned() }),
        _ => Ok(Event::Unknown),
    }
}

fn decode_error_detail(value: &Value) -> Option<ErrorDetail> {
    let object = value.as_object()?;
    Some(ErrorDetail {
        code: object.get("code").and_then(Value::as_str).map(str::to_string),
        error_type: object.get("error_type").and_then(Value::as_str).map(str::to_string),
        message: object.get("message").and_then(Value::as_str).map(str::to_string),
    })
}

fn step_value(step: &OutputStep) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String(step.kind.clone()));
    if let Some(id) = &step.id {
        object.insert("id".to_string(), Value::String(id.clone()));
    }
    if let Some(name) = &step.name {
        object.insert("name".to_string(), Value::String(name.clone()));
    }
    if let Some(arguments) = &step.arguments {
        object.insert("arguments".to_string(), arguments.clone());
    }
    if let Some(signature) = &step.signature {
        object.insert("signature".to_string(), Value::String(signature.clone()));
    }
    if let Some(summary) = &step.summary {
        object.insert("summary".to_string(), Value::Array(summary.iter().map(|text| text_content(text)).collect()));
    }
    if let Some(content) = &step.content {
        object.insert(
            "content".to_string(),
            Value::Array(
                content
                    .iter()
                    .map(|part| {
                        let mut object = Map::new();
                        object.insert("type".to_string(), Value::String(part.kind.clone()));
                        if let Some(text) = &part.text {
                            object.insert("text".to_string(), Value::String(text.clone()));
                        }
                        Value::Object(object)
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(object)
}

fn delta_value(delta: &Delta) -> Value {
    let mut object = Map::new();
    match delta {
        Delta::Text(text) => {
            object.insert("type".to_string(), Value::String("text".to_string()));
            object.insert("text".to_string(), Value::String(text.clone()));
        }
        Delta::Arguments(arguments) => {
            object.insert("type".to_string(), Value::String("arguments_delta".to_string()));
            object.insert("arguments".to_string(), Value::String(arguments.clone()));
        }
        Delta::ThoughtSignature(signature) => {
            object.insert("type".to_string(), Value::String("thought_signature".to_string()));
            object.insert("signature".to_string(), Value::String(signature.clone()));
        }
        Delta::ThoughtSummary(text) => {
            object.insert("type".to_string(), Value::String("thought_summary".to_string()));
            object.insert("content".to_string(), text_content(text));
        }
        Delta::Unknown(raw_type) => {
            object.insert("type".to_string(), Value::String("unknown".to_string()));
            object.insert("rawType".to_string(), Value::String(raw_type.clone()));
        }
    }
    Value::Object(object)
}

fn interaction_error_value(error: &InteractionError) -> Value {
    let mut object = Map::new();
    if let Some(code) = &error.code {
        object.insert("code".to_string(), Value::String(code.clone()));
    }
    if let Some(message) = &error.message {
        object.insert("message".to_string(), Value::String(message.clone()));
    }
    Value::Object(object)
}

fn interaction_value(interaction: &Interaction) -> Value {
    let mut object = Map::new();
    if let Some(id) = &interaction.id {
        object.insert("id".to_string(), Value::String(id.clone()));
    }
    object.insert("status".to_string(), Value::String(interaction.status.clone()));
    if let Some(usage) = &interaction.usage {
        object.insert("usage".to_string(), usage.value());
    }
    if let Some(errors) = &interaction.errors {
        object.insert("errors".to_string(), Value::Array(errors.iter().map(interaction_error_value).collect()));
    }
    Value::Object(object)
}

fn error_event_value(error: Option<&Value>) -> Value {
    let mut object = Map::new();
    object.insert("event_type".to_string(), Value::String("error".to_string()));
    if let Some(error) = error {
        object.insert("error".to_string(), error.clone());
    }
    Value::Object(object)
}

fn completed_event_value(interaction: &Interaction) -> Value {
    let mut object = Map::new();
    object.insert("event_type".to_string(), Value::String("interaction.completed".to_string()));
    object.insert("interaction".to_string(), interaction_value(interaction));
    Value::Object(object)
}

fn failure_reason(
    route: &str,
    code: Option<&str>,
    message: String,
    data: Option<Value>,
    raw_body: &str,
) -> AiErrorReason {
    let normalized = code.map(|code| code.trim().to_uppercase().replace('-', "_"));
    let normalized = normalized.as_deref().filter(|normalized| !normalized.is_empty());
    if let Some(normalized) = normalized {
        if RECOVERABLE_CODES.contains(&normalized) {
            return AiErrorReason::InvalidProviderOutput {
                message,
                body: Some(raw_body.to_string()),
                http: None,
                classification: Some(InvalidProviderOutputClassification::IncompleteStream),
                route: Some(route.to_string()),
            };
        }
        if INVALID_OUTPUT_CODES.contains(&normalized) {
            return AiErrorReason::InvalidProviderOutput {
                message,
                body: Some(raw_body.to_string()),
                http: None,
                classification: None,
                route: Some(route.to_string()),
            };
        }
        if normalized == "CONTENT_BLOCKED" || gemini_generate_content::content_filtered(Some(normalized)) {
            return AiErrorReason::ContentPolicy { message, body: Some(raw_body.to_string()), http: None };
        }
    }
    classify_provider_failure(ProviderFailure {
        message,
        data,
        raw_body: Some(raw_body.to_string()),
        ..ProviderFailure::default()
    })
}

fn index_key(index: &Number) -> i64 {
    index.as_i64().or_else(|| index.as_f64().map(|index| index as i64)).unwrap_or_default()
}

fn block_id(index: &Number) -> String {
    index.to_string()
}

struct State {
    route: String,
    provider_metadata_key: String,
    lifecycle: lifecycle::State,
    has_tool_calls: bool,
    usage: Option<RawUsage>,
    continued_usage: Vec<Option<RawUsage>>,
    steps: BTreeMap<i64, OutputStep>,
    tools: tool_stream::State,
}

impl State {
    fn start(&mut self, index: Number, step: OutputStep, events: &mut Vec<LlmEvent>) -> Result<(), AiError> {
        let key = index_key(&index);
        let id = block_id(&index);
        let mut lifecycle = lifecycle::step_start(self.lifecycle.clone(), events);
        let mut tools = self.tools.clone();
        match step.kind.as_str() {
            "thought" => {
                let metadata = signature_metadata(&self.provider_metadata_key, step.signature.as_deref());
                lifecycle = lifecycle::reasoning_start(lifecycle, events, &id, Some(metadata));
                for text in step.summary.iter().flatten() {
                    lifecycle = lifecycle::reasoning_delta(lifecycle, events, &id, text, None);
                }
            }
            "model_output" => {
                lifecycle = lifecycle::text_start(lifecycle, events, &id, None);
                for part in step.content.iter().flatten() {
                    if part.kind != "text" {
                        return Err(shared::event_error(
                            ID,
                            format!("Unsupported Interactions output: {}", part.kind),
                            Some(crate::json::encode_body(&step_value(&step))),
                        ));
                    }
                    if let Some(text) = part.text.as_deref().filter(|text| !text.is_empty()) {
                        lifecycle = lifecycle::text_delta(lifecycle, events, &id, text, None);
                    }
                }
            }
            "function_call" => {
                let (Some(tool_id), Some(name)) = (step.id.clone(), step.name.clone()) else {
                    return Err(shared::event_error(
                        ID,
                        "Interactions function call lacks id or name",
                        Some(crate::json::encode_body(&step_value(&step))),
                    ));
                };
                let input = match step.arguments.as_ref() {
                    Some(arguments) if arguments.as_object().is_some_and(|object| !object.is_empty()) => {
                        crate::json::encode_body(arguments)
                    }
                    _ => String::new(),
                };
                let metadata = signature_metadata(&self.provider_metadata_key, step.signature.as_deref());
                tool_stream::start(
                    &mut tools,
                    key,
                    tool_stream::PendingTool {
                        id: tool_id.clone(),
                        name: name.clone(),
                        namespace: None,
                        input,
                        provider_executed: None,
                        provider_metadata: Some(metadata.clone()),
                    },
                );
                events.push(LlmEvent::ToolInputStart {
                    id: tool_id,
                    name,
                    namespace: None,
                    provider_executed: None,
                    provider_metadata: Some(metadata),
                });
            }
            other => {
                return Err(shared::event_error(
                    ID,
                    format!("Unsupported Interactions step: {other}"),
                    Some(crate::json::encode_body(&step_value(&step))),
                ));
            }
        }
        self.lifecycle = lifecycle;
        self.tools = tools;
        self.has_tool_calls |= step.kind == "function_call";
        self.steps.insert(key, step);
        Ok(())
    }

    fn delta(&mut self, index: Number, delta: Delta, events: &mut Vec<LlmEvent>) -> Result<(), AiError> {
        let key = index_key(&index);
        let Some(step) = self.steps.get(&key).cloned() else {
            return Err(shared::event_error(
                ID,
                "Interactions delta without step.start",
                Some(crate::json::encode_body(&delta_value(&delta))),
            ));
        };
        let id = block_id(&index);
        match delta {
            Delta::Text(text) if step.kind == "model_output" => {
                self.lifecycle = lifecycle::text_delta(self.lifecycle.clone(), events, &id, &text, None);
            }
            Delta::ThoughtSummary(text) if step.kind == "thought" => {
                self.lifecycle = lifecycle::reasoning_delta(self.lifecycle.clone(), events, &id, &text, None);
            }
            Delta::ThoughtSignature(signature) => {
                let mut next = step;
                next.signature = Some(signature);
                if let Some(tool) = self.tools.get_mut(&key) {
                    tool.provider_metadata =
                        Some(signature_metadata(&self.provider_metadata_key, next.signature.as_deref()));
                }
                self.steps.insert(key, next);
            }
            Delta::Arguments(arguments) if step.kind == "function_call" => {
                let outcome = tool_stream::append_existing(
                    ID,
                    &mut self.tools,
                    key,
                    &arguments,
                    "Interactions arguments without function call",
                )?;
                events.extend(outcome.events);
            }
            delta => {
                let kind = match &delta {
                    Delta::Unknown(raw_type) => raw_type.clone(),
                    Delta::Text(_) => "text".to_string(),
                    Delta::Arguments(_) => "arguments_delta".to_string(),
                    Delta::ThoughtSignature(_) => "thought_signature".to_string(),
                    Delta::ThoughtSummary(_) => "thought_summary".to_string(),
                };
                return Err(shared::event_error(
                    ID,
                    format!("Unsupported Interactions delta: {kind}"),
                    Some(crate::json::encode_body(&delta_value(&delta))),
                ));
            }
        }
        Ok(())
    }

    fn stop(&mut self, index: Number, events: &mut Vec<LlmEvent>) -> Result<(), AiError> {
        let key = index_key(&index);
        let Some(step) = self.steps.get(&key).cloned() else {
            return Err(shared::event_error(ID, "Interactions step.stop without step.start", None));
        };
        let id = block_id(&index);
        match step.kind.as_str() {
            "thought" => {
                let metadata = signature_metadata(&self.provider_metadata_key, step.signature.as_deref());
                self.lifecycle = lifecycle::reasoning_end(self.lifecycle.clone(), events, &id, Some(metadata), None);
            }
            "model_output" => {
                self.lifecycle = lifecycle::text_end(self.lifecycle.clone(), events, &id, None, None);
            }
            _ => {
                let outcome = tool_stream::finish(ID, &mut self.tools, key, None)?;
                events.extend(outcome.events);
            }
        }
        Ok(())
    }

    fn completed(&mut self, interaction: Interaction, events: &mut Vec<LlmEvent>) -> Result<(), AiError> {
        if interaction.status == CONTINUATION {
            if interaction.usage.is_some() {
                self.usage = interaction.usage;
            }
            return Ok(());
        }
        if interaction.status == "failed" || interaction.status == "cancelled" {
            let messages = interaction
                .errors
                .iter()
                .flatten()
                .filter_map(|error| error.message.as_ref().filter(|message| !message.trim().is_empty()).cloned())
                .collect::<Vec<String>>();
            let coded = interaction.errors.iter().flatten().find(|error| error.code.is_some());
            let message = if messages.is_empty() {
                format!("Google Interactions {}", interaction.status)
            } else {
                messages.join("\n")
            };
            let data = match coded {
                Some(error) => interaction_error_value(error),
                None => interaction_value(&interaction),
            };
            let raw_body = crate::json::encode_body(&completed_event_value(&interaction));
            return Err(AiError {
                reason: failure_reason(
                    &self.route,
                    coded.and_then(|error| error.code.as_deref()),
                    message,
                    Some(data),
                    &raw_body,
                ),
            });
        }
        if !matches!(interaction.status.as_str(), "completed" | "requires_action" | "incomplete") {
            return Err(shared::event_error(
                ID,
                format!("Unexpected terminal Interactions status: {}", interaction.status),
                Some(crate::json::encode_body(&completed_event_value(&interaction))),
            ));
        }
        let pending = tool_stream::finish_all(ID, &mut self.tools)?;
        events.extend(pending.events);
        let normalized =
            if interaction.status == "requires_action" || (interaction.status == "completed" && self.has_tool_calls) {
                FinishReason::ToolCalls
            } else if interaction.status == "incomplete" {
                FinishReason::Length
            } else {
                FinishReason::Stop
            };
        let usage = map_usage(
            interaction.usage.as_ref().or(self.usage.as_ref()),
            &self.provider_metadata_key,
            &self.continued_usage,
        );
        let mut inner = Map::new();
        if let Some(id) = &interaction.id {
            inner.insert("interactionId".to_string(), Value::String(id.clone()));
        }
        let mut metadata = ProviderMetadata::new();
        metadata.insert(self.provider_metadata_key.clone(), Value::Object(inner));
        let reason = FinishReasonDetails {
            normalized,
            raw: Some(interaction.status.clone()),
            category: None,
            explanation: None,
        };
        self.lifecycle = lifecycle::finish(self.lifecycle.clone(), events, reason, usage, Some(metadata));
        Ok(())
    }

    fn error(&self, error: Option<Value>) -> AiError {
        let raw_body = crate::json::encode_body(&error_event_value(error.as_ref()));
        let detail = error.as_ref().and_then(decode_error_detail);
        let message = detail
            .as_ref()
            .and_then(|detail| detail.message.as_deref())
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_string)
            .or_else(|| provider_error_message(&raw_body))
            .unwrap_or_else(|| "Google Interactions stream error".to_string());
        let code = detail.and_then(|detail| detail.code.or(detail.error_type));
        AiError { reason: failure_reason(&self.route, code.as_deref(), message, error, &raw_body) }
    }
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = decode_event(&frame, &self.route)?;
        let mut events = Vec::new();
        match event {
            Event::StepStart { index, step } => self.start(index, step, &mut events)?,
            Event::StepDelta { index, delta } => self.delta(index, delta, &mut events)?,
            Event::StepStop { index } => self.stop(index, &mut events)?,
            Event::StatusUpdate { usage } => {
                if usage.is_some() {
                    self.usage = usage;
                }
            }
            Event::Completed { interaction } => self.completed(interaction, &mut events)?,
            Event::Error { error } => return Err(self.error(error)),
            Event::InteractionCreated | Event::Unknown => {}
        }
        Ok(events)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(frame) else {
            return false;
        };
        value.get("event_type").and_then(Value::as_str) == Some("interaction.completed")
            && value
                .get("interaction")
                .and_then(Value::as_object)
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                != Some(CONTINUATION)
    }
}

impl Protocol for GoogleInteractions {
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

    fn new_state(&self, request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        let model = &request.model;
        Box::new(State {
            route: format!("{}/{}", model.provider, model.route.id),
            provider_metadata_key: model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone()),
            lifecycle: lifecycle::initial(),
            has_tool_calls: false,
            usage: None,
            continued_usage: Vec::new(),
            steps: BTreeMap::new(),
            tools: tool_stream::empty(),
        })
    }

    fn sanitizer(&self) -> Option<SanitizerKind> {
        Some(SanitizerKind::Gemini)
    }
}
