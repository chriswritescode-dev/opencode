use std::collections::{BTreeMap, HashSet};

use opencode_bus::BusError;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::sql::now_millis;
use crate::stored;

pub(crate) struct Owner {
    pub session_id: String,
    pub message_id: String,
    pub seq: i64,
}

struct Blob {
    hash: String,
    mime: String,
    bytes: usize,
    data: Vec<u8>,
}

#[allow(dead_code)]
struct Split {
    data: Value,
    moved: bool,
    blobs: Vec<Blob>,
    hashes: HashSet<String>,
}

fn tool_items(part: &Value) -> &[Value] {
    if part.get("type").and_then(Value::as_str) != Some("tool") {
        return &[];
    }
    let Some(state) = part.get("state") else {
        return &[];
    };
    if !matches!(state.get("status").and_then(Value::as_str), Some("completed" | "error")) {
        return &[];
    }
    state.get("content").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn ref_hash_of(item: &Value) -> Option<&str> {
    item.get("uri").and_then(Value::as_str).and_then(stored::ref_hash)
}

fn collect(data: &Value, hashes: &mut HashSet<String>) {
    if let Some(files) = data.get("files").and_then(Value::as_array) {
        hashes
            .extend(files.iter().filter(|file| stored::is_file_ref(file)).filter_map(ref_hash_of).map(str::to_string));
    }
    if let Some(content) = data.get("content").and_then(Value::as_array) {
        hashes.extend(
            content
                .iter()
                .flat_map(tool_items)
                .filter(|item| stored::is_tool_file_ref(item))
                .filter_map(ref_hash_of)
                .map(str::to_string),
        );
    }
    hashes.extend(
        tool_items(data)
            .iter()
            .filter(|item| stored::is_tool_file_ref(item))
            .filter_map(ref_hash_of)
            .map(str::to_string),
    );
}

fn hashes_of(items: &[Value]) -> HashSet<String> {
    let mut hashes = HashSet::new();
    for item in items {
        collect(item, &mut hashes);
    }
    hashes
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

fn base64_decode(input: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = base64_value(byte) else {
            continue;
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    output
}

fn data_uri(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("data:")?;
    let (prefix, data) = rest.split_once(";base64,")?;
    if prefix.contains(',') {
        return None;
    }
    Some(data)
}

fn take(pending: &mut BTreeMap<String, Blob>, raw: &[u8], mime: &str) -> String {
    let hash = opencode_util::hash::sha256_hex(raw);
    if !pending.contains_key(&hash) {
        let blob = Blob { hash: hash.clone(), mime: mime.to_string(), bytes: raw.len(), data: raw.to_vec() };
        pending.insert(hash.clone(), blob);
    }
    stored::reference(&hash)
}

fn file_ref(pending: &mut BTreeMap<String, Blob>, file: &Value) -> Result<Value, BusError> {
    if stored::is_file_ref(file) {
        return Ok(file.clone());
    }
    let Some(data) = file.get("data").and_then(Value::as_str) else {
        return Err(invalid("expected base64 data"));
    };
    let Some(mime) = file.get("mime").and_then(Value::as_str) else {
        return Err(invalid("expected a mime"));
    };
    let uri = take(pending, &base64_decode(data), mime);
    let mut stored = file.clone();
    let Some(object) = stored.as_object_mut() else {
        return Err(invalid("expected a file object"));
    };
    object.shift_remove("data");
    object.insert("uri".to_string(), Value::String(uri));
    Ok(stored)
}

fn tool_ref(pending: &mut BTreeMap<String, Blob>, item: &Value) -> Value {
    if item.get("type").and_then(Value::as_str) != Some("file") {
        return item.clone();
    }
    let Some(data) = item.get("uri").and_then(Value::as_str).and_then(data_uri) else {
        return item.clone();
    };
    let mime = item.get("mime").and_then(Value::as_str).unwrap_or_default();
    with_field(item, "uri", Value::String(take(pending, &base64_decode(data), mime)))
}

fn tool_part_refs(pending: &mut BTreeMap<String, Blob>, part: &Value) -> Value {
    let items = tool_items(part);
    if items.is_empty() {
        return part.clone();
    }
    let content = items.iter().map(|item| tool_ref(pending, item)).collect::<Vec<_>>();
    with_state_content(part, content)
}

fn stored_part_refs(pending: &mut BTreeMap<String, Blob>, part: &Value) -> Value {
    let items = tool_items(part);
    let rewrites = items.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("file")
            && item.get("uri").and_then(Value::as_str).and_then(data_uri).is_some()
    });
    if !rewrites {
        return part.clone();
    }
    let content = items.iter().map(|item| tool_ref(pending, item)).collect::<Vec<_>>();
    with_state_content(part, content)
}

fn with_state_content(part: &Value, content: Vec<Value>) -> Value {
    let mut stored = part.clone();
    if let Some(state) = stored.get_mut("state").and_then(Value::as_object_mut) {
        state.insert("content".to_string(), Value::Array(content));
    }
    stored
}

fn with_field(data: &Value, key: &str, value: Value) -> Value {
    let mut stored = data.clone();
    if let Some(object) = stored.as_object_mut() {
        object.insert(key.to_string(), value);
    }
    stored
}

fn save<'a>(connection: &Connection, blobs: impl IntoIterator<Item = &'a Blob>) -> Result<(), BusError> {
    let time_created = now_millis();
    let mut statement = connection.prepare(
        "INSERT INTO blob (hash, mime, bytes, width, height, data, time_created) \
         VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5) ON CONFLICT(hash) DO NOTHING",
    )?;
    for blob in blobs {
        statement.execute(params![blob.hash, blob.mime, blob.bytes as i64, blob.data, time_created])?;
    }
    Ok(())
}

pub(crate) fn externalize_payload(
    connection: &Connection,
    media_blob: bool,
    payload: &Value,
) -> Result<Value, BusError> {
    if !media_blob {
        return Ok(payload.clone());
    }
    let Some(files) = payload.get("files").and_then(Value::as_array) else {
        return Ok(payload.clone());
    };
    let mut pending = BTreeMap::new();
    let files = files.iter().map(|file| file_ref(&mut pending, file)).collect::<Result<Vec<_>, BusError>>()?;
    if pending.is_empty() {
        return Ok(payload.clone());
    }
    save(connection, pending.values())?;
    Ok(with_field(payload, "files", Value::Array(files)))
}

pub(crate) fn externalize(connection: &Connection, media_blob: bool, message: &Value) -> Result<Value, BusError> {
    match message.get("type").and_then(Value::as_str) {
        Some("user") => externalize_payload(connection, media_blob, message),
        Some("assistant") => {
            if !media_blob {
                return Ok(message.clone());
            }
            let Some(content) = message.get("content").and_then(Value::as_array) else {
                return Ok(message.clone());
            };
            let mut pending = BTreeMap::new();
            let content = content.iter().map(|part| tool_part_refs(&mut pending, part)).collect::<Vec<_>>();
            if pending.is_empty() {
                return Ok(message.clone());
            }
            save(connection, pending.values())?;
            Ok(with_field(message, "content", Value::Array(content)))
        }
        _ => Ok(message.clone()),
    }
}

pub(crate) fn externalize_part(connection: &Connection, media_blob: bool, part: &Value) -> Result<Value, BusError> {
    if !media_blob {
        return Ok(part.clone());
    }
    let mut pending = BTreeMap::new();
    let stored = tool_part_refs(&mut pending, part);
    if pending.is_empty() {
        return Ok(part.clone());
    }
    save(connection, pending.values())?;
    Ok(stored)
}

#[allow(dead_code)]
fn split(data: &Value) -> Result<Split, BusError> {
    let mut pending = BTreeMap::new();
    let stored = if let Some(files) = data.get("files").and_then(Value::as_array) {
        if files.iter().any(|file| !stored::is_file_ref(file)) {
            let files = files.iter().map(|file| file_ref(&mut pending, file)).collect::<Result<Vec<_>, BusError>>()?;
            with_field(data, "files", Value::Array(files))
        } else {
            split_content(data, &mut pending)
        }
    } else {
        split_content(data, &mut pending)
    };
    let moved = !pending.is_empty();
    let mut hashes = HashSet::new();
    collect(&stored, &mut hashes);
    Ok(Split { data: if moved { stored } else { data.clone() }, moved, blobs: pending.into_values().collect(), hashes })
}

fn split_content(data: &Value, pending: &mut BTreeMap<String, Blob>) -> Value {
    if let Some(content) = data.get("content").and_then(Value::as_array) {
        let content = content.iter().map(|part| stored_part_refs(pending, part)).collect::<Vec<_>>();
        return with_field(data, "content", Value::Array(content));
    }
    if data.get("type").and_then(Value::as_str) == Some("tool") {
        return stored_part_refs(pending, data);
    }
    data.clone()
}

#[allow(dead_code)]
fn externalize_row(connection: &Connection, data: &Value) -> Result<Value, BusError> {
    let result = split(data)?;
    if !result.moved {
        return Ok(data.clone());
    }
    save(connection, &result.blobs)?;
    Ok(result.data)
}

pub(crate) fn record_refs(connection: &Connection, owner: &Owner, items: &[Value]) -> Result<(), BusError> {
    for hash in hashes_of(items) {
        let exists =
            connection.query_row("SELECT hash FROM blob WHERE hash = ?1", [&hash], |_| Ok(())).optional()?.is_some();
        if !exists {
            continue;
        }
        connection.execute(
            "INSERT INTO blob_ref (session_id, message_id, hash, seq) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(session_id, message_id, hash) DO UPDATE SET seq = excluded.seq",
            params![owner.session_id, owner.message_id, hash, owner.seq],
        )?;
    }
    Ok(())
}

pub(crate) fn sync_refs(connection: &Connection, owner: &Owner, items: &[Value]) -> Result<(), BusError> {
    record_refs(connection, owner, items)?;
    let present = hashes_of(items);
    if present.is_empty() {
        return release_message(connection, &owner.session_id, &owner.message_id);
    }
    let current = {
        let mut statement =
            connection.prepare("SELECT hash FROM blob_ref WHERE session_id = ?1 AND message_id = ?2")?;
        let rows = statement.query_map([&owner.session_id, &owner.message_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for hash in current.into_iter().filter(|hash| !present.contains(hash)) {
        connection.execute(
            "DELETE FROM blob_ref WHERE session_id = ?1 AND message_id = ?2 AND hash = ?3",
            params![owner.session_id, owner.message_id, hash],
        )?;
        release_orphan(connection, &hash)?;
    }
    Ok(())
}

pub(crate) fn release_session(connection: &Connection, session_id: &str) -> Result<(), BusError> {
    connection.execute(
        "DELETE FROM blob WHERE hash IN (SELECT hash FROM blob_ref WHERE session_id = ?1) \
         AND NOT EXISTS (SELECT 1 FROM blob_ref other WHERE other.hash = blob.hash AND other.session_id <> ?1)",
        [session_id],
    )?;
    connection.execute("DELETE FROM blob_ref WHERE session_id = ?1", [session_id])?;
    Ok(())
}

pub(crate) fn release_message(connection: &Connection, session_id: &str, message_id: &str) -> Result<(), BusError> {
    let hashes = {
        let mut statement =
            connection.prepare("DELETE FROM blob_ref WHERE session_id = ?1 AND message_id = ?2 RETURNING hash")?;
        let hashes = statement.query_map(params![session_id, message_id], |row| row.get::<_, String>(0))?;
        hashes.collect::<Result<Vec<_>, _>>()?
    };
    for hash in hashes {
        release_orphan(connection, &hash)?;
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn release_from(connection: &Connection, session_id: &str, seq: i64) -> Result<(), BusError> {
    let hashes = {
        let mut statement =
            connection.prepare("DELETE FROM blob_ref WHERE session_id = ?1 AND seq >= ?2 RETURNING hash")?;
        let hashes = statement.query_map(params![session_id, seq], |row| row.get::<_, String>(0))?;
        hashes.collect::<Result<Vec<_>, _>>()?
    };
    for hash in hashes {
        release_orphan(connection, &hash)?;
    }
    Ok(())
}

fn release_orphan(connection: &Connection, hash: &str) -> Result<(), BusError> {
    connection.execute(
        "DELETE FROM blob WHERE hash = ?1 AND NOT EXISTS (SELECT 1 FROM blob_ref WHERE blob_ref.hash = ?1)",
        [hash],
    )?;
    Ok(())
}

fn invalid(message: &str) -> BusError {
    BusError::InvalidDurableEvent { r#type: "session.blob".to_string(), message: message.to_string() }
}
