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

#[allow(dead_code)]
pub(crate) fn encode_part(part: &Value) -> Value {
    part.clone()
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
