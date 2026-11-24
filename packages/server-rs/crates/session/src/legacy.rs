use serde_json::{Map, Value};

pub(crate) fn message(r#type: &str, data: Map<String, Value>) -> Map<String, Value> {
    match r#type {
        "assistant" => content(rename(data, "providerState", "native")),
        "compaction" => provider_context(recent(rename(data, "providerState", "native"))),
        _ => data,
    }
}

pub(crate) fn part(value: Value) -> Value {
    let Some(object) = value.as_object() else {
        return value;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("text" | "reasoning") => Value::Object(rename(object.clone(), "state", "native")),
        Some("tool") => Value::Object(tool_result(rename(object.clone(), "providerState", "native"))),
        _ => value,
    }
}

fn content(mut data: Map<String, Value>) -> Map<String, Value> {
    let Some(Value::Array(items)) = data.get("content").cloned() else {
        return data;
    };
    data.insert("content".to_string(), Value::Array(items.into_iter().map(part).collect()));
    data
}

fn recent(data: Map<String, Value>) -> Map<String, Value> {
    let text = data.get("recent").and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_string);
    let mut rest = omit(data, "recent");
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

fn provider_context(data: Map<String, Value>) -> Map<String, Value> {
    let context = data.get("providerContext").cloned();
    let mut rest = omit(data, "providerContext");
    let Some(Value::Object(context)) = context else {
        return rest;
    };
    let mut native = Map::new();
    if let Some(messages) = context.get("messages") {
        native.insert(
            "messages".to_string(),
            match messages {
                Value::Array(items) => Value::Array(items.iter().map(media).collect()),
                other => other.clone(),
            },
        );
    }
    rest.insert("native".to_string(), Value::Object(native));
    rest
}

fn media(value: &Value) -> Value {
    let Some(message) = value.as_object() else {
        return value.clone();
    };
    let Some(Value::Array(parts)) = message.get("content") else {
        return value.clone();
    };
    let mut message = message.clone();
    message.insert("content".to_string(), Value::Array(parts.iter().map(media_part).collect()));
    Value::Object(message)
}

fn media_part(part: &Value) -> Value {
    let Some(object) = part.as_object() else {
        return part.clone();
    };
    if object.get("type").and_then(Value::as_str) != Some("media") || object.contains_key("media") {
        return part.clone();
    }
    let Some(data) = object.get("data").and_then(Value::as_str).map(str::to_string) else {
        return part.clone();
    };
    let mut rest = object.clone();
    rest.shift_remove("mediaType");
    rest.shift_remove("data");
    let mut source = Map::new();
    source.insert("type".to_string(), Value::String("base64".to_string()));
    source.insert("data".to_string(), Value::String(data));
    if let Some(media_type) = object.get("mediaType") {
        source.insert("mediaType".to_string(), media_type.clone());
    }
    let mut media = Map::new();
    media.insert("source".to_string(), Value::Object(source));
    rest.insert("media".to_string(), Value::Object(media));
    Value::Object(rest)
}

fn tool_result(mut data: Map<String, Value>) -> Map<String, Value> {
    let Some(state) = data.shift_remove("providerResultState") else {
        return data;
    };
    let Value::Object(state) = state else {
        return data;
    };
    let mut native = match data.get("native") {
        Some(Value::Object(native)) => native.clone(),
        _ => Map::new(),
    };
    for (key, value) in state {
        native.insert(key, value);
    }
    data.insert("native".to_string(), Value::Object(native));
    data
}

fn rename(mut data: Map<String, Value>, from: &str, to: &str) -> Map<String, Value> {
    let Some(moved) = data.shift_remove(from) else {
        return data;
    };
    if !data.contains_key(to) {
        data.insert(to.to_string(), moved);
    }
    data
}

fn omit(mut data: Map<String, Value>, key: &str) -> Map<String, Value> {
    data.shift_remove(key);
    data
}
