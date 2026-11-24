use opencode_contract::codec::encode_json;
use serde_json::Value;

use crate::StorageError;

pub fn stored_absolute_path(input: &str) -> Option<String> {
    let result = storage_path(input);
    if result.starts_with('/') || is_windows_storage_path(&result) { Some(result) } else { None }
}

pub fn to_driver_absolute(input: &str) -> Result<String, StorageError> {
    stored_absolute_path(input).ok_or_else(|| StorageError::NotAbsolute(input.to_string()))
}

pub fn from_driver_absolute(stored: &str) -> Result<String, StorageError> {
    Ok(to_platform(to_driver_absolute(stored)?))
}

pub fn encode_absolute_array(paths: &[String]) -> Result<String, StorageError> {
    let items = paths
        .iter()
        .map(|path| to_driver_absolute(path).map(Value::String))
        .collect::<Result<Vec<Value>, StorageError>>()?;
    Ok(encode_json(&Value::Array(items)))
}

pub fn decode_absolute_array(text: &str) -> Result<Vec<String>, StorageError> {
    let items: Vec<String> = serde_json::from_str(text)?;
    items.iter().map(|item| from_driver_absolute(item)).collect()
}

#[cfg(windows)]
fn storage_path(input: &str) -> String {
    input.replace('\\', "/")
}

#[cfg(not(windows))]
fn storage_path(input: &str) -> String {
    input.to_string()
}

#[cfg(windows)]
fn to_platform(input: String) -> String {
    if is_windows_storage_path(&input) { input.replace('/', "\\") } else { input }
}

#[cfg(not(windows))]
fn to_platform(input: String) -> String {
    input
}

fn is_windows_storage_path(input: &str) -> bool {
    let bytes = input.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/')
        || input.starts_with("//")
}
