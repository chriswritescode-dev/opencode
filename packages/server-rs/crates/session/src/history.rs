use opencode_bus::BusError;
use serde_json::{Map, Value};

pub(crate) fn message_id_from_event(event_id: &str) -> String {
    format!("msg_{}", event_id.strip_prefix("evt_").unwrap_or(event_id))
}

fn invalid(r#type: &str, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: message.into() }
}

fn kind(message: &Value) -> String {
    message.get("type").and_then(Value::as_str).unwrap_or("session_message").to_string()
}

pub(crate) fn message_id(message: &Value) -> Result<String, BusError> {
    message
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(&kind(message), "expected a message id"))
}

pub(crate) fn message_type(message: &Value) -> Result<String, BusError> {
    message
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid("session_message", "expected a message type"))
}

pub(crate) fn message_created(message: &Value) -> Result<f64, BusError> {
    message
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(Value::as_f64)
        .ok_or_else(|| invalid(&kind(message), "expected a message creation time"))
}

pub(crate) fn stored_data(message: &Value) -> Result<Value, BusError> {
    let object = message.as_object().ok_or_else(|| invalid(&kind(message), "expected a message object"))?;
    Ok(Value::Object(
        object
            .iter()
            .filter(|(key, _)| key.as_str() != "id" && key.as_str() != "type")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Map<String, Value>>(),
    ))
}

pub(crate) fn decode_message_row(id: &str, r#type: &str, data: &str) -> Result<Value, BusError> {
    let mut object = stored_object(r#type, data)?;
    object.insert("id".to_string(), Value::String(id.to_string()));
    object.insert("type".to_string(), Value::String(r#type.to_string()));
    Ok(Value::Object(object))
}

pub(crate) fn encode_part(part: &Value) -> Value {
    let Some(object) = part.as_object() else {
        return part.clone();
    };
    let Some(r#type) = object.get("type").and_then(Value::as_str) else {
        return part.clone();
    };
    let mut encoded = Map::new();
    encoded.insert("type".to_string(), Value::String(r#type.to_string()));
    match r#type {
        "text" | "reasoning" => {
            copy(&mut encoded, object, "text");
            copy(&mut encoded, object, "native");
            if r#type == "reasoning"
                && let Some(time) = object.get("time").and_then(Value::as_object)
            {
                encoded.insert("time".to_string(), ordered(time, &["created", "completed"]));
            }
        }
        "tool" => {
            copy(&mut encoded, object, "id");
            copy(&mut encoded, object, "name");
            copy(&mut encoded, object, "executed");
            copy(&mut encoded, object, "native");
            if let Some(state) = object.get("state") {
                encoded.insert("state".to_string(), encode_state(state));
            }
            if let Some(time) = object.get("time").and_then(Value::as_object) {
                encoded.insert("time".to_string(), ordered(time, &["created", "ran", "completed"]));
            }
        }
        _ => return part.clone(),
    }
    Value::Object(encoded)
}

pub(crate) fn assistant_data(message: &Value, content: Option<Value>) -> Result<Value, BusError> {
    let r#type = kind(message);
    let object = message.as_object().ok_or_else(|| invalid(&r#type, "expected a message object"))?;
    let time = object.get("time").and_then(Value::as_object).ok_or_else(|| invalid(&r#type, "expected a time"))?;
    let created = present(time, "created").cloned().ok_or_else(|| invalid(&r#type, "expected a time created"))?;
    let mut ordered_time = Map::new();
    ordered_time.insert("created".to_string(), created);
    for key in ["firstToken", "streamed", "completed"] {
        copy(&mut ordered_time, time, key);
    }
    let mut data = Map::new();
    copy(&mut data, object, "metadata");
    data.insert("time".to_string(), Value::Object(ordered_time));
    data.insert("agent".to_string(), required(object, &r#type, "agent")?);
    data.insert("model".to_string(), required(object, &r#type, "model")?);
    data.insert(
        "content".to_string(),
        content.unwrap_or_else(|| object.get("content").cloned().unwrap_or_else(|| Value::Array(Vec::new()))),
    );
    if let Some(snapshot) = object.get("snapshot").and_then(Value::as_object) {
        data.insert("snapshot".to_string(), ordered(snapshot, &["start", "end", "files"]));
    }
    for key in ["finish", "rawFinish", "native", "cost"] {
        copy(&mut data, object, key);
    }
    if let Some(tokens) = object.get("tokens").and_then(Value::as_object) {
        let mut ordered_tokens = Map::new();
        for key in ["input", "output", "reasoning"] {
            copy(&mut ordered_tokens, tokens, key);
        }
        if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
            ordered_tokens.insert("cache".to_string(), ordered(cache, &["read", "write"]));
        }
        data.insert("tokens".to_string(), Value::Object(ordered_tokens));
    }
    copy(&mut data, object, "error");
    if let Some(retry) = object.get("retry").and_then(Value::as_object) {
        data.insert("retry".to_string(), ordered(retry, &["attempt", "at", "error"]));
    }
    Ok(Value::Object(data))
}

fn encode_state(state: &Value) -> Value {
    let Some(object) = state.as_object() else {
        return state.clone();
    };
    let Some(status) = object.get("status").and_then(Value::as_str) else {
        return state.clone();
    };
    let keys: &[&str] = match status {
        "streaming" => &["input"],
        "running" => &["input", "metadata"],
        "completed" => &["input", "content", "metadata"],
        "error" => &["input", "error", "content", "metadata"],
        _ => return state.clone(),
    };
    let mut encoded = Map::new();
    encoded.insert("status".to_string(), Value::String(status.to_string()));
    for key in keys {
        copy(&mut encoded, object, key);
    }
    Value::Object(encoded)
}

fn ordered(object: &Map<String, Value>, keys: &[&str]) -> Value {
    let mut result = Map::new();
    for key in keys {
        copy(&mut result, object, key);
    }
    Value::Object(result)
}

fn copy(target: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) {
    if let Some(value) = present(source, key) {
        target.insert(key.to_string(), value.clone());
    }
}

fn present<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

fn required(object: &Map<String, Value>, r#type: &str, key: &str) -> Result<Value, BusError> {
    present(object, key).cloned().ok_or_else(|| invalid(r#type, format!("expected {key}")))
}

#[allow(dead_code)]
pub(crate) fn decode_part_row(r#type: &str, data: &str) -> Result<Value, BusError> {
    let mut object = stored_object(r#type, data)?;
    object.insert("type".to_string(), Value::String(r#type.to_string()));
    Ok(Value::Object(object))
}

fn stored_object(r#type: &str, data: &str) -> Result<Map<String, Value>, BusError> {
    match serde_json::from_str::<Value>(data).map_err(|error| invalid(r#type, error.to_string()))? {
        Value::Object(object) => Ok(object),
        _ => Err(invalid(r#type, "expected a stored object")),
    }
}
