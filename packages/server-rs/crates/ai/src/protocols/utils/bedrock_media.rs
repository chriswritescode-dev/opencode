use std::collections::HashSet;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Map, Value};

use crate::protocols::shared;
use crate::schema::{AiError, MediaPart};

pub fn lower(part: &MediaPart, document_names: &mut HashSet<String>) -> Result<Vec<Value>, AiError> {
    let media_type = shared::media_type(&part.media).to_lowercase();
    if let Some(format) = image_format(&media_type) {
        return Ok(vec![image_block(format, &media_base64(part)?)]);
    }
    if media_type.starts_with("image/") {
        return Err(shared::invalid_request(format!(
            "Bedrock Converse does not support image media type {media_type}"
        )));
    }
    let Some(format) = document_format(&media_type) else {
        return Err(shared::invalid_request(format!("Bedrock Converse does not support media type {media_type}")));
    };
    let name = document_name(part.filename.as_deref(), document_names);
    let block = document_block(format, &name, &media_base64(part)?);
    match part.filename.as_deref() {
        Some(filename) if filename != name => Ok(vec![document_label(filename, &name), block]),
        _ => Ok(vec![block]),
    }
}

fn image_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpeg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn document_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "application/pdf" => Some("pdf"),
        "text/csv" => Some("csv"),
        "application/msword" => Some("doc"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "text/html" => Some("html"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        _ => None,
    }
}

fn media_base64(part: &MediaPart) -> Result<String, AiError> {
    let inline = shared::require_inline_media("Bedrock Converse", &part.media)?;
    let bytes = BASE64_STANDARD
        .decode(&inline.base64)
        .map_err(|_| shared::invalid_request("Bedrock Converse media data must be valid base64"))?;
    Ok(BASE64_STANDARD.encode(bytes))
}

fn image_block(format: &str, bytes: &str) -> Value {
    let mut source = Map::new();
    source.insert("bytes".to_string(), Value::String(bytes.to_string()));
    let mut image = Map::new();
    image.insert("format".to_string(), Value::String(format.to_string()));
    image.insert("source".to_string(), Value::Object(source));
    let mut block = Map::new();
    block.insert("image".to_string(), Value::Object(image));
    Value::Object(block)
}

fn document_block(format: &str, name: &str, bytes: &str) -> Value {
    let mut source = Map::new();
    source.insert("bytes".to_string(), Value::String(bytes.to_string()));
    let mut document = Map::new();
    document.insert("format".to_string(), Value::String(format.to_string()));
    document.insert("name".to_string(), Value::String(name.to_string()));
    document.insert("source".to_string(), Value::Object(source));
    let mut block = Map::new();
    block.insert("document".to_string(), Value::Object(document));
    Value::Object(block)
}

fn document_label(filename: &str, name: &str) -> Value {
    let filename = crate::json::encode_body(&Value::String(filename.to_string()));
    let name = crate::json::encode_body(&Value::String(name.to_string()));
    let mut block = Map::new();
    block.insert("text".to_string(), Value::String(format!("Attached file {filename} has document label {name}.")));
    Value::Object(block)
}

fn document_name(filename: Option<&str>, names: &mut HashSet<String>) -> String {
    let filename = filename.unwrap_or("");
    let stem = match filename.rfind('.') {
        Some(index) => &filename[..index],
        None => filename,
    };
    let cleaned = stem
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | ' ' | '(' | ')' | '[' | ']' | '-' => character,
            _ => ' ',
        })
        .collect::<String>();
    let collapsed = cleaned.split_whitespace().collect::<Vec<&str>>().join(" ");
    let truncated = collapsed.chars().take(200).collect::<String>();
    let base = match truncated.trim() {
        "" => "document".to_string(),
        name => name.to_string(),
    };
    let mut name = base.clone();
    let mut index = 2;
    while names.contains(&name) {
        let suffix = format!(" {index}");
        let limit = 200usize.saturating_sub(suffix.chars().count());
        let prefix = base.chars().take(limit).collect::<String>();
        name = format!("{}{suffix}", prefix.trim_end());
        index += 1;
    }
    names.insert(name.clone());
    name
}
