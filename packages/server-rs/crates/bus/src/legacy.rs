use serde_json::{Map, Value};

pub fn upgrade_event(r#type: &str, data: Value) -> Value {
    let Value::Object(object) = data else {
        return data;
    };
    Value::Object(upgrade(r#type, object))
}

fn upgrade(r#type: &str, data: Map<String, Value>) -> Map<String, Value> {
    match r#type {
        "session.message.content.updated" => content(data),
        "session.compaction.started" => omit(data, "recent"),
        "session.compaction.ended" => provider_context(omit(rename(data, "providerState", "native"), "recent")),
        _ => match renames(r#type) {
            Some((from, to)) => rename(data, from, to),
            None => data,
        },
    }
}

fn renames(r#type: &str) -> Option<(&'static str, &'static str)> {
    match r#type {
        "session.text.ended" => Some(("state", "native")),
        "session.reasoning.started" => Some(("state", "native")),
        "session.reasoning.ended" => Some(("state", "native")),
        "session.tool.called" => Some(("state", "native")),
        "session.step.ended" => Some(("providerState", "native")),
        "session.step.failed" => Some(("providerState", "native")),
        "session.tool.success" => Some(("resultState", "native")),
        "session.tool.failed" => Some(("resultState", "native")),
        _ => None,
    }
}

fn content(mut data: Map<String, Value>) -> Map<String, Value> {
    let parts =
        data.get("content").and_then(Value::as_array).map(|parts| parts.iter().cloned().map(part).collect::<Vec<_>>());
    let Some(parts) = parts else {
        return data;
    };
    data.insert("content".to_string(), Value::Array(parts));
    data
}

fn part(value: Value) -> Value {
    let Value::Object(object) = value else {
        return value;
    };
    let tag = object.get("type").and_then(Value::as_str);
    if matches!(tag, Some("text" | "reasoning")) {
        return Value::Object(rename(object, "state", "native"));
    }
    if tag == Some("tool") {
        return Value::Object(tool_result(rename(object, "providerState", "native")));
    }
    Value::Object(object)
}

fn tool_result(mut value: Map<String, Value>) -> Map<String, Value> {
    let Some(Value::Object(result)) = value.remove("providerResultState") else {
        return value;
    };
    let mut native = match value.remove("native") {
        Some(Value::Object(native)) => native,
        _ => Map::new(),
    };
    native.extend(result);
    value.insert("native".to_string(), Value::Object(native));
    value
}

fn provider_context(mut data: Map<String, Value>) -> Map<String, Value> {
    let Some(Value::Object(context)) = data.remove("providerContext") else {
        return data;
    };
    let mut native = Map::new();
    match context.get("messages") {
        Some(Value::Array(messages)) => {
            native.insert("messages".to_string(), Value::Array(messages.iter().cloned().map(media).collect()));
        }
        Some(messages) => {
            native.insert("messages".to_string(), messages.clone());
        }
        None => {}
    }
    data.insert("native".to_string(), Value::Object(native));
    data
}

fn media(value: Value) -> Value {
    let Value::Object(mut message) = value else {
        return value;
    };
    let parts = message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().cloned().map(media_part).collect::<Vec<_>>());
    let Some(parts) = parts else {
        return Value::Object(message);
    };
    message.insert("content".to_string(), Value::Array(parts));
    Value::Object(message)
}

fn media_part(value: Value) -> Value {
    let Value::Object(mut part) = value else {
        return value;
    };
    let replace = part.get("type").and_then(Value::as_str) == Some("media")
        && !part.contains_key("media")
        && part.get("data").is_some_and(Value::is_string);
    if !replace {
        return Value::Object(part);
    }
    let media_type = part.remove("mediaType");
    let data = part.remove("data").unwrap_or(Value::Null);
    let mut source = Map::new();
    source.insert("type".to_string(), Value::String("base64".to_string()));
    source.insert("data".to_string(), data);
    if let Some(media_type) = media_type {
        source.insert("mediaType".to_string(), media_type);
    }
    let mut rebuilt = Map::new();
    rebuilt.insert("source".to_string(), Value::Object(source));
    part.insert("media".to_string(), Value::Object(rebuilt));
    Value::Object(part)
}

fn omit(mut value: Map<String, Value>, key: &str) -> Map<String, Value> {
    value.remove(key);
    value
}

fn rename(mut value: Map<String, Value>, from: &str, to: &str) -> Map<String, Value> {
    let Some(moved) = value.remove(from) else {
        return value;
    };
    if value.contains_key(to) {
        return value;
    }
    value.insert(to.to_string(), moved);
    value
}
