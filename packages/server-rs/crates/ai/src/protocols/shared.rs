use std::collections::HashSet;

use serde_json::{Map, Number, Value};

pub use crate::prepare::tool_updates::flatten_tools;
use crate::prepare::tool_updates::{fold_tools, tools_update, withdrawn_tools};
use crate::route::model::Model;
use crate::schema::{
    AiError, AiErrorReason, AssistantMessage, CacheMode, CachePolicy, ContentPart, LlmRequest, MediaAsset, MediaPart,
    MediaSource, Message, ProviderMetadata, SystemMessage, TextPart, ToolCallPart, ToolChanges, ToolChoice,
    ToolChoiceType, ToolContent, ToolEntry, ToolMessage, ToolResultPart, ToolResultValue,
};

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

pub fn prompt_cache_key(request: &LlmRequest, model: &Model) -> Option<String> {
    if matches!(request.cache, Some(CachePolicy::Mode(CacheMode::None))) {
        return None;
    }
    if model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.get("promptCacheKey"))
        .is_some_and(|value| value == &Value::Bool(false))
    {
        return None;
    }
    let key = request.prompt_cache_key.as_ref()?;
    Some(key.chars().take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH).collect())
}

pub fn total_tokens(input: Option<u64>, output: Option<u64>, total: Option<u64>) -> Option<u64> {
    if let Some(total) = total {
        return Some(total);
    }
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(input.unwrap_or(0) + output.unwrap_or(0))
}

pub fn subtract_tokens(total: Option<u64>, subtrahend: Option<u64>) -> Option<u64> {
    let total = total?;
    let Some(subtrahend) = subtrahend else {
        return Some(total);
    };
    Some(total.saturating_sub(subtrahend))
}

pub fn sum_tokens(values: &[Option<u64>]) -> Option<u64> {
    if values.iter().all(Option::is_none) {
        return None;
    }
    Some(values.iter().map(|value| value.unwrap_or(0)).sum())
}

pub fn fit_thinking_budget(budget: &Number, max_tokens: Option<&Number>, minimum: i64) -> Number {
    let Some(max_tokens) = max_tokens.and_then(Number::as_f64) else {
        return normalized_number(budget);
    };
    if budget.as_f64().unwrap_or_default() <= max_tokens / 2.0 {
        return normalized_number(budget);
    }
    number_from_f64((minimum as f64).max((max_tokens / 2.0).floor())).unwrap_or_else(|| Number::from(minimum))
}

fn normalized_number(number: &Number) -> Number {
    number_from_f64(number.as_f64().unwrap_or_default()).unwrap_or_else(|| number.clone())
}

pub fn event_error(route: &str, message: impl Into<String>, body: Option<String>) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message: message.into(),
            body,
            http: None,
            classification: None,
            route: Some(route.to_string()),
        },
    }
}

pub fn invalid_request(message: impl Into<String>) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: message.into(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

pub fn unsupported_operation(
    operation: &str,
    message: impl Into<String>,
    provider: Option<String>,
    route: Option<String>,
) -> AiError {
    AiError {
        reason: AiErrorReason::UnsupportedOperation {
            message: message.into(),
            body: None,
            http: None,
            operation: operation.to_string(),
            provider,
            route,
        },
    }
}

pub fn join_text(texts: impl IntoIterator<Item = String>) -> String {
    texts.into_iter().collect::<Vec<String>>().join("\n")
}

pub fn wrapped_system_update(route: &str, message: &Message) -> Result<TextPart, AiError> {
    let mut texts = Vec::new();
    let mut cache = None;
    for part in message_content(message) {
        let ContentPart::Text(part) = part else {
            return Err(invalid_request(format!("{route} system messages only support text content for now")));
        };
        texts.push(part.text.clone());
        cache = part.cache.clone();
    }
    let escaped = join_text(texts).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    Ok(TextPart { text: format!("<system-update>\n{escaped}\n</system-update>"), cache, provider_metadata: None })
}

pub fn tool_result_text(part: &ToolResultPart) -> String {
    match &part.result {
        ToolResultValue::Json { value } => crate::json::encode_body(value),
        ToolResultValue::Text { value } => js_string(value),
        ToolResultValue::Error { value } => match value {
            Value::Array(_) | Value::Object(_) => crate::json::encode_body(value),
            value => js_string(value),
        },
        ToolResultValue::Content { value } => {
            let items = value.iter().map(tool_content_value).collect::<Vec<Value>>();
            crate::json::encode_body(&Value::Array(items))
        }
    }
}

pub fn flatten_request(request: &LlmRequest) -> LlmRequest {
    let all = request
        .tools
        .iter()
        .cloned()
        .chain(
            request
                .messages
                .iter()
                .flat_map(|message| tools_update(message).into_iter().flat_map(|changes| changes.add.iter().cloned())),
        )
        .collect::<Vec<ToolEntry>>();
    let declared = flatten_tools(&all).into_iter().map(|tool| tool.name).collect::<HashSet<String>>();
    let mut tools = request.tools.clone();
    let messages = request
        .messages
        .iter()
        .map(|message| match message {
            Message::System(system) => {
                let Some(changes) = system.tools.as_ref() else {
                    return message.clone();
                };
                let next = fold_tools(tools.clone(), &changes.add, &changes.remove);
                let remove = withdrawn_tools(&tools, &next);
                tools = next;
                Message::System(SystemMessage {
                    tools: Some(ToolChanges {
                        add: flatten_tools(&changes.add).into_iter().map(ToolEntry::Tool).collect(),
                        remove,
                    }),
                    ..system.clone()
                })
            }
            Message::Assistant(message) => {
                let content = message.content.iter().map(|part| match part {
                    ContentPart::ToolCall(part) => ContentPart::ToolCall(flatten_tool_call(part, &declared)),
                    ContentPart::ToolResult(part) => ContentPart::ToolResult(flatten_tool_result(part, &declared)),
                    part => part.clone(),
                });
                Message::Assistant(AssistantMessage { content: content.collect(), ..message.clone() })
            }
            Message::Tool(message) => {
                let content = message.content.iter().map(|part| match part {
                    ContentPart::ToolCall(part) => ContentPart::ToolCall(flatten_tool_call(part, &declared)),
                    ContentPart::ToolResult(part) => ContentPart::ToolResult(flatten_tool_result(part, &declared)),
                    part => part.clone(),
                });
                Message::Tool(ToolMessage { content: content.collect(), ..message.clone() })
            }
            Message::User(message) => Message::User(message.clone()),
        })
        .collect();
    LlmRequest {
        tools: flatten_tools(&request.tools).into_iter().map(ToolEntry::Tool).collect(),
        messages,
        tool_choice: flatten_tool_choice(request.tool_choice.clone(), &all, &declared),
        ..request.clone()
    }
}

fn flatten_tool_choice(
    choice: Option<ToolChoice>,
    tools: &[ToolEntry],
    declared: &HashSet<String>,
) -> Option<ToolChoice> {
    let choice = choice?;
    if choice.r#type != ToolChoiceType::Tool {
        return Some(choice);
    }
    let Some(name) = choice.name.as_ref() else {
        return Some(choice);
    };
    if declared.contains(name) {
        return Some(choice);
    }
    let Some(flattened) = namespaced_paths(tools, &[]).into_iter().find(|(path, _)| path == name).map(|(_, name)| name)
    else {
        return Some(choice);
    };
    Some(ToolChoice { name: Some(flattened), ..choice })
}

fn namespaced_paths(tools: &[ToolEntry], path: &[String]) -> Vec<(String, String)> {
    tools
        .iter()
        .flat_map(|tool| match tool {
            ToolEntry::Namespace(namespace) => {
                let mut nested = path.to_vec();
                nested.push(namespace.name.clone());
                namespaced_paths(&namespace.tools, &nested)
            }
            ToolEntry::Tool(tool) if !path.is_empty() => {
                let mut full = path.to_vec();
                full.push(tool.name.clone());
                vec![(full.join("."), full.join("_"))]
            }
            ToolEntry::Tool(_) => Vec::new(),
        })
        .collect()
}

pub fn content_filter_category(sources: &[&Value]) -> Option<String> {
    for source in sources {
        let Some(entries) = source.as_object() else {
            continue;
        };
        for (key, value) in entries {
            let accepted = match value.as_object() {
                Some(object) if object.contains_key("filtered") => object.get("filtered") == Some(&Value::Bool(true)),
                _ => is_truthy(value),
            };
            if accepted {
                return Some(key.clone());
            }
        }
    }
    None
}

pub fn reported_billing(usage: &Value, frames: &[&Value]) -> Option<Number> {
    if let Some(cost) = usage.get("cost").and_then(Value::as_f64).filter(|cost| cost.is_finite() && *cost >= 0.0) {
        return number_from_f64(cost);
    }
    frames.iter().find_map(|frame| {
        let value = frame
            .get("copilot_usage")?
            .get("total_nano_aiu")?
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)?;
        number_from_f64(value / 100_000_000_000.0)
    })
}

pub struct InlineMedia {
    pub mime: String,
    pub base64: String,
    pub data_url: String,
}

pub fn inline_media(asset: &MediaAsset) -> Option<InlineMedia> {
    let (data, media_type) = match &asset.source {
        MediaSource::Bytes { data, media_type } | MediaSource::Base64 { data, media_type } => (data, media_type),
        MediaSource::Url { .. } | MediaSource::Ref { .. } => return None,
    };
    let mime = media_type.to_lowercase();
    Some(InlineMedia { mime: mime.clone(), base64: data.clone(), data_url: format!("data:{mime};base64,{data}") })
}

pub fn media_type(asset: &MediaAsset) -> String {
    match &asset.source {
        MediaSource::Bytes { media_type, .. } | MediaSource::Base64 { media_type, .. } => media_type.clone(),
        MediaSource::Url { media_type, .. } | MediaSource::Ref { media_type, .. } => {
            media_type.clone().unwrap_or_else(|| "application/octet-stream".to_string())
        }
    }
}

pub fn media_kind(media_type: &str) -> &'static str {
    let media_type = media_type.to_lowercase();
    if media_type.starts_with("image/") {
        return "image";
    }
    if media_type.starts_with("video/") {
        return "video";
    }
    if media_type.starts_with("audio/") {
        return "audio";
    }
    if media_type == "application/pdf" || media_type.starts_with("text/") {
        return "document";
    }
    "other"
}

pub fn media_url(asset: &MediaAsset) -> Option<&str> {
    match &asset.source {
        MediaSource::Url { url, .. } => Some(url),
        _ => None,
    }
}

pub fn require_inline_media(route: &str, asset: &MediaAsset) -> Result<InlineMedia, AiError> {
    let Some(inline) = inline_media(asset) else {
        return Err(invalid_request(format!(
            "{route} requires inline media (bytes or base64); {} sources must be materialized first",
            source_type(&asset.source)
        )));
    };
    Ok(inline)
}

pub fn tool_file_media(item: &ToolContent) -> Option<MediaPart> {
    let ToolContent::File { uri, mime, name } = item else {
        return None;
    };
    let source = match data_url_payload(uri) {
        Some(data) => MediaSource::Base64 { data: data.to_string(), media_type: mime.clone() },
        None if uri.contains("://") => {
            MediaSource::Url { url: uri.clone(), media_type: Some(mime.clone()), expires_at: None }
        }
        None => MediaSource::Base64 { data: uri.clone(), media_type: mime.clone() },
    };
    Some(MediaPart {
        media: MediaAsset { source, info: None, provider_metadata: None, headers: None },
        filename: name.clone(),
        cache: None,
        provider_metadata: None,
    })
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn flatten_tool_call(part: &ToolCallPart, declared: &HashSet<String>) -> ToolCallPart {
    let name = flat_tool_name(&part.name, part.namespace.as_deref(), declared);
    if name == part.name && part.namespace.is_none() {
        return part.clone();
    }
    ToolCallPart { name, namespace: None, ..part.clone() }
}

fn flatten_tool_result(part: &ToolResultPart, declared: &HashSet<String>) -> ToolResultPart {
    let name = flat_tool_name(&part.name, part.namespace.as_deref(), declared);
    if name == part.name && part.namespace.is_none() {
        return part.clone();
    }
    ToolResultPart { name, namespace: None, ..part.clone() }
}

fn flat_tool_name(name: &str, namespace: Option<&str>, declared: &HashSet<String>) -> String {
    let flat = match namespace {
        Some(namespace) => format!("{namespace}_{name}"),
        None => name.to_string(),
    };
    if declared.contains(&flat) {
        return flat;
    }
    let sanitized = flat
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => character,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        return "_".to_string();
    }
    sanitized
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Null => "null".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_string(item) })
            .collect::<Vec<String>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn tool_content_value(item: &ToolContent) -> Value {
    match item {
        ToolContent::Text { text } => {
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), Value::String("text".to_string()));
            object.insert("text".to_string(), Value::String(text.clone()));
            Value::Object(object)
        }
        ToolContent::File { uri, mime, name } => {
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), Value::String("file".to_string()));
            object.insert("uri".to_string(), Value::String(uri.clone()));
            object.insert("mime".to_string(), Value::String(mime.clone()));
            if let Some(name) = name {
                object.insert("name".to_string(), Value::String(name.clone()));
            }
            Value::Object(object)
        }
    }
}

pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub fn number_from_f64(value: f64) -> Option<Number> {
    if value.fract() == 0.0 && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
        return Some(Number::from(value as i64));
    }
    Number::from_f64(value)
}

pub(crate) fn provider_metadata_value<'a>(metadata: Option<&'a ProviderMetadata>, key: &str) -> Option<&'a Value> {
    metadata?.get(key)
}

fn source_type(source: &MediaSource) -> &'static str {
    match source {
        MediaSource::Bytes { .. } => "bytes",
        MediaSource::Base64 { .. } => "base64",
        MediaSource::Url { .. } => "url",
        MediaSource::Ref { .. } => "ref",
    }
}

fn data_url_payload(value: &str) -> Option<&str> {
    let rest = value.get(..5).filter(|prefix| prefix.eq_ignore_ascii_case("data:")).map(|_| &value[5..])?;
    let media_type_end = rest.find([';', ',']).filter(|end| *end > 0)?;
    if !rest[media_type_end..].starts_with(';') {
        return None;
    }
    let comma = media_type_end + rest[media_type_end..].find(',')?;
    let parameters = rest.get(media_type_end..comma)?;
    let marker = parameters.get(parameters.len().checked_sub(7)?..)?;
    if !marker.eq_ignore_ascii_case(";base64") {
        return None;
    }
    rest.get(comma + 1..)
}

pub(crate) fn optional_string(object: &Map<String, Value>, key: &str) -> bool {
    matches!(object.get(key), None | Some(Value::String(_)))
}

pub(crate) fn optional_null_string(object: &Map<String, Value>, key: &str) -> bool {
    matches!(object.get(key), None | Some(Value::Null) | Some(Value::String(_)))
}

pub(crate) fn optional_null_object(object: &Map<String, Value>, key: &str) -> bool {
    matches!(object.get(key), None | Some(Value::Null) | Some(Value::Object(_)))
}
