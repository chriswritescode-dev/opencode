use std::collections::BTreeMap;

use opencode_bus::BusError;
use opencode_contract::codec::{Decode, Path};
use opencode_contract::generated::{SessionMessageInfo, SessionMessageMediaInfo};
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::{blob, legacy};

#[derive(Clone)]
pub(crate) struct MessageRow {
    pub id: String,
    pub session_id: String,
    pub seq: i64,
    pub r#type: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDecodeError {
    pub session_id: String,
    pub message_id: String,
}

impl std::fmt::Display for MessageDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Failed to decode message {} in session {}", self.message_id, self.session_id)
    }
}

impl std::error::Error for MessageDecodeError {}

#[derive(Debug)]
pub enum HistoryError {
    Database(BusError),
    Decode(MessageDecodeError),
}

impl From<BusError> for HistoryError {
    fn from(error: BusError) -> Self {
        Self::Database(error)
    }
}

impl From<rusqlite::Error> for HistoryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(BusError::from(error))
    }
}

impl HistoryError {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Decode(_) => "Session.MessageDecodeError",
            Self::Database(_) => "Database",
        }
    }
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "{error}"),
            Self::Decode(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for HistoryError {}

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
    Ok(Value::Object(message_input(id, r#type, stored_object(r#type, data)?)))
}

fn message_input(id: &str, r#type: &str, object: Map<String, Value>) -> Map<String, Value> {
    let mut data = legacy::message(r#type, object);
    data.insert("id".to_string(), Value::String(id.to_string()));
    data.insert("type".to_string(), Value::String(r#type.to_string()));
    data
}

fn row_input(row: &MessageRow) -> Result<Value, HistoryError> {
    let Some(object) = row.data.as_object().cloned() else {
        return Err(decode_error(row));
    };
    Ok(Value::Object(message_input(&row.id, &row.r#type, object)))
}

fn decode_error(row: &MessageRow) -> HistoryError {
    HistoryError::Decode(MessageDecodeError { session_id: row.session_id.clone(), message_id: row.id.clone() })
}

pub(crate) fn decode_message_rows(
    connection: &Connection,
    rows: &[MessageRow],
) -> Result<Vec<SessionMessageInfo>, HistoryError> {
    let rows = blob::hydrate(connection, &with_parts(connection, rows)?)?;
    rows.iter()
        .map(|row| {
            let input = row_input(row)?;
            SessionMessageInfo::decode(&input, &mut Path::new()).map_err(|_| decode_error(row))
        })
        .collect()
}

pub(crate) fn decode_message_rows_with_refs(
    connection: &Connection,
    rows: &[MessageRow],
) -> Result<Vec<SessionMessageMediaInfo>, HistoryError> {
    let (rows, meta) = blob::keep_refs(connection, &with_parts(connection, rows)?)?;
    rows.iter()
        .map(|row| {
            let input = row_input(row)?;
            let mut message =
                SessionMessageMediaInfo::decode(&input, &mut Path::new()).map_err(|_| decode_error(row))?;
            if let SessionMessageMediaInfo::MediaUser(user) = &mut message
                && let Some(files) = &mut user.files
            {
                blob::describe(files, &meta);
            }
            Ok(message)
        })
        .collect()
}

fn with_parts(connection: &Connection, rows: &[MessageRow]) -> Result<Vec<MessageRow>, HistoryError> {
    let ids = rows.iter().filter(|row| row.r#type == "assistant").map(|row| row.id.as_str()).collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(rows.to_vec());
    }
    let mut statement =
        connection.prepare("SELECT data FROM session_part WHERE message_id = ?1 ORDER BY ordinal ASC")?;
    let mut parts = BTreeMap::<String, Vec<Value>>::new();
    for id in ids {
        let stored = statement.query_map([id], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        if stored.is_empty() {
            continue;
        }
        let values = stored
            .iter()
            .map(|data| serde_json::from_str::<Value>(data))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| HistoryError::Database(invalid("session_part", error.to_string())))?;
        parts.insert(id.to_string(), values);
    }
    if parts.is_empty() {
        return Ok(rows.to_vec());
    }
    Ok(rows
        .iter()
        .map(|row| {
            let Some(found) = parts.get(&row.id) else {
                return row.clone();
            };
            let mut data = row.data.clone();
            let inline = data.get("content").cloned();
            let content = match inline {
                Some(Value::Array(inline)) => found.iter().chain(inline.iter()).cloned().collect(),
                _ => found.clone(),
            };
            if let Some(object) = data.as_object_mut() {
                object.insert("content".to_string(), Value::Array(content));
            }
            MessageRow { data, ..row.clone() }
        })
        .collect())
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
    Ok(legacy::part(Value::Object(object)))
}

fn stored_object(r#type: &str, data: &str) -> Result<Map<String, Value>, BusError> {
    match serde_json::from_str::<Value>(data).map_err(|error| invalid(r#type, error.to_string()))? {
        Value::Object(object) => Ok(object),
        _ => Err(invalid(r#type, "expected a stored object")),
    }
}
