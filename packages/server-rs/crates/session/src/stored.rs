use serde_json::Value;

pub(crate) fn reference(hash: &str) -> String {
    format!("media:{hash}")
}

pub(crate) fn ref_hash(uri: &str) -> Option<&str> {
    let hash = uri.strip_prefix("media:")?;
    if hash.len() != 64 {
        return None;
    }
    hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)).then_some(hash)
}

pub(crate) fn is_file_ref(file: &Value) -> bool {
    file.get("uri").and_then(Value::as_str).is_some_and(|uri| ref_hash(uri).is_some())
        && file.get("mime").is_some_and(Value::is_string)
        && file.get("source").is_some_and(Value::is_object)
}

pub(crate) fn is_tool_file_ref(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("file")
        && item.get("mime").is_some_and(Value::is_string)
        && item.get("uri").and_then(Value::as_str).is_some_and(|uri| ref_hash(uri).is_some())
}
