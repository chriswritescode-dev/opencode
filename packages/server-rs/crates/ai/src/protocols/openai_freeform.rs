use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::prepare::tool_updates::{flatten_tools, tools_update};
use crate::protocols::utils::tool_stream;
use crate::schema::{Freeform, LlmEvent, LlmRequest, ProviderMetadata, ToolDefinition, ToolEntry};

const STREAM_END: &str = "\"}";

#[derive(Debug, Clone, PartialEq)]
pub struct Custom {
    pub field: String,
    pub format: Freeform,
}

pub fn resolve(tool: &ToolDefinition) -> Option<Custom> {
    let field = freeform_field(&tool.input_schema)?;
    let format = tool.freeform.clone()?;
    if tool.native.is_some() {
        return None;
    }
    Some(Custom { field, format })
}

pub fn tools(request: &LlmRequest) -> BTreeMap<String, Custom> {
    let added = request
        .messages
        .iter()
        .filter_map(tools_update)
        .flat_map(|changes| changes.add.iter().cloned())
        .collect::<Vec<ToolEntry>>();
    let combined = request.tools.iter().chain(added.iter()).cloned().collect::<Vec<ToolEntry>>();
    flatten_tools(&combined).into_iter().filter_map(|tool| resolve(&tool).map(|custom| (tool.name, custom))).collect()
}

pub fn grammar(format: &Freeform) -> Option<Value> {
    let (syntax, definition) = match format {
        Freeform::Text => return None,
        Freeform::Regex { pattern } => ("regex", pattern),
        Freeform::Lark { grammar } => ("lark", grammar),
    };
    let mut object = Map::new();
    object.insert("syntax".to_string(), Value::String(syntax.to_string()));
    object.insert("definition".to_string(), Value::String(definition.clone()));
    Some(Value::Object(object))
}

pub fn to_raw(custom: &Custom, input: &Value) -> String {
    if let Some(Value::String(value)) = input.as_object().and_then(|object| object.get(&custom.field)) {
        return value.clone();
    }
    if input.is_null() {
        return crate::json::encode_body(&Value::Object(Map::new()));
    }
    crate::json::encode_body(input)
}

pub fn from_raw(custom: Option<&Custom>, raw: &str) -> String {
    let mut object = Map::new();
    object.insert(field(custom).to_string(), Value::String(raw.to_string()));
    crate::json::encode_body(&Value::Object(object))
}

pub fn stream_start(custom: Option<&Custom>, raw: &str) -> String {
    format!("{{{}:\"{}", crate::json::encode_body(&Value::String(field(custom).to_string())), stream_delta(raw))
}

pub fn stream_delta(raw: &str) -> String {
    let encoded = crate::json::encode_body(&Value::String(raw.to_string()));
    encoded.get(1..encoded.len().saturating_sub(1)).unwrap_or_default().to_string()
}

pub struct Final<'a> {
    pub custom: Option<&'a Custom>,
    pub raw: &'a str,
}

pub struct CloseOutcome {
    pub tools: tool_stream::State,
    pub events: Vec<LlmEvent>,
}

pub fn close(
    tools: tool_stream::State,
    keys: impl IntoIterator<Item = i64>,
    final_input: Option<Final<'_>>,
) -> CloseOutcome {
    let mut tools = tools;
    let mut events = Vec::new();
    for key in keys {
        let Some(pending) = tools.get(&key).cloned() else {
            continue;
        };
        if let Some(final_input) = final_input.as_ref() {
            let streamed = decode_arguments(&format!("{}{STREAM_END}", pending.input));
            let value = streamed.as_ref().and_then(|value| value.get(field(final_input.custom)));
            if value.and_then(Value::as_str) != Some(final_input.raw) {
                continue;
            }
        }
        let Some(appended) = tool_stream::append(&mut tools, key, STREAM_END) else {
            continue;
        };
        events.extend(appended.events);
    }
    CloseOutcome { tools, events }
}

pub fn metadata(item_id: &str, custom: bool) -> ProviderMetadata {
    let mut metadata = ProviderMetadata::new();
    metadata.insert("itemId".to_string(), Value::String(item_id.to_string()));
    if custom {
        metadata.insert("custom".to_string(), Value::Bool(true));
    }
    metadata
}

pub fn keeps_item_id(provider_metadata: Option<&ProviderMetadata>, key: &str, custom: bool) -> bool {
    let arrived_custom = provider_metadata
        .and_then(|metadata| metadata.get(key))
        .and_then(|value| value.get("custom"))
        .is_some_and(|value| value == &Value::Bool(true));
    arrived_custom == custom
}

fn field(custom: Option<&Custom>) -> &str {
    custom.map_or("input", |custom| custom.field.as_str())
}

fn decode_arguments(text: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    value.is_object().then_some(value)
}

fn freeform_field(schema: &Value) -> Option<String> {
    let properties = schema.get("properties")?.as_object()?;
    if properties.len() != 1 {
        return None;
    }
    let (name, property) = properties.iter().next()?;
    if property.get("type").and_then(Value::as_str) != Some("string") {
        return None;
    }
    let required = schema.get("required")?.as_array()?;
    let declared = required.iter().any(|value| value.as_str() == Some(name.as_str()));
    declared.then(|| name.clone())
}
