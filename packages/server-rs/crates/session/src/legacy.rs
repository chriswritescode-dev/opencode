use opencode_bus::legacy;
use serde_json::{Map, Value};

pub(crate) fn message(r#type: &str, data: Map<String, Value>) -> Map<String, Value> {
    match r#type {
        "assistant" => legacy::content(legacy::rename(data, "providerState", "native")),
        "compaction" => legacy::provider_context(recent(legacy::rename(data, "providerState", "native"))),
        _ => data,
    }
}

pub(crate) fn part(value: Value) -> Value {
    legacy::part(value)
}

fn recent(data: Map<String, Value>) -> Map<String, Value> {
    let text = data.get("recent").and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_string);
    let mut rest = legacy::omit(data, "recent");
    let Some(text) = text else {
        return rest;
    };
    let mut metadata = Map::new();
    metadata.insert("recent".to_string(), Value::String(text));
    if let Some(Value::Object(existing)) = rest.get("metadata") {
        for (key, value) in existing {
            metadata.insert(key.clone(), value.clone());
        }
    }
    rest.insert("metadata".to_string(), Value::Object(metadata));
    rest
}
