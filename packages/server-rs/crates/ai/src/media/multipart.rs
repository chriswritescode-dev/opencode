use serde_json::{Map, Value};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Form {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text { name: String, value: String },
    File { name: String, bytes: Vec<u8>, filename: Option<String>, content_type: Option<String> },
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.parts.push(Part::Text { name: name.into(), value: value.into() });
    }

    pub fn append_file(
        &mut self,
        name: impl Into<String>,
        bytes: Vec<u8>,
        filename: Option<String>,
        content_type: Option<String>,
    ) {
        self.parts.push(Part::File { name: name.into(), bytes, filename, content_type });
    }

    pub fn serialize(&self, boundary: &str) -> Vec<u8> {
        let mut body = Vec::new();
        for part in &self.parts {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match part {
                Part::Text { name, value } => {
                    body.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                            escape(&normalize_line_endings(name))
                        )
                        .as_bytes(),
                    );
                    body.extend_from_slice(normalize_line_endings(value).as_bytes());
                    body.extend_from_slice(b"\r\n");
                }
                Part::File { name, bytes, filename, content_type } => {
                    body.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                            escape(&normalize_line_endings(name)),
                            escape(filename.as_deref().unwrap_or(""))
                        )
                        .as_bytes(),
                    );
                    body.extend_from_slice(
                        format!("Content-Type: {}\r\n\r\n", content_type_header(content_type.as_deref())).as_bytes(),
                    );
                    body.extend_from_slice(bytes);
                    body.extend_from_slice(b"\r\n");
                }
            }
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }
}

fn normalize_line_endings(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                chars.next_if_eq(&'\n');
                normalized.push_str("\r\n");
            }
            '\n' => normalized.push_str("\r\n"),
            other => normalized.push(other),
        }
    }
    normalized
}

fn escape(value: &str) -> String {
    value.replace('\r', "%0D").replace('\n', "%0A").replace('"', "%22")
}

fn content_type_header(content_type: Option<&str>) -> String {
    let value = content_type.unwrap_or("");
    if !value.bytes().all(|byte| byte.is_ascii_graphic() || byte == b' ') {
        return "application/octet-stream".to_string();
    }
    let canonical = match value {
        "application/webassembly" => Some("application/wasm"),
        "application/javascript" => Some("text/javascript;charset=utf-8"),
        "application/json" => Some("application/json;charset=utf-8"),
        "application/x-www-form-urlencoded" => Some("application/x-www-form-urlencoded;charset=UTF-8"),
        "text/css" => Some("text/css;charset=utf-8"),
        "text/html" => Some("text/html;charset=utf-8"),
        "text/javascript" => Some("text/javascript;charset=utf-8"),
        "text/jsx" => Some("text/javascript;charset=utf-8"),
        "text/plain" => Some("text/plain;charset=utf-8"),
        _ => None,
    };
    match (canonical, value.is_empty()) {
        (Some(header), _) => header.to_string(),
        (None, true) => "application/octet-stream".to_string(),
        (None, false) => value.to_ascii_lowercase(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatArrays {
    Brackets,
    Key,
}

pub struct AppendFieldsOptions<'a> {
    pub overlay: Option<&'a Map<String, Value>>,
    pub reserved: &'a [&'a str],
    pub repeat_arrays: Option<RepeatArrays>,
}

pub fn append_fields(form: &mut Form, fields: &Map<String, Value>, options: AppendFieldsOptions<'_>) {
    let overlay: Map<String, Value> = match options.overlay {
        None => Map::new(),
        Some(overlay) => overlay
            .iter()
            .filter(|(key, _)| !options.reserved.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    };
    let merged = if overlay.is_empty() { fields.clone() } else { crate::json::merge_objects(fields, &overlay) };
    for (key, value) in &merged {
        let repeats = match value {
            Value::Array(items) if items.iter().all(is_scalar) => options.repeat_arrays,
            _ => None,
        };
        if let Some(repeat) = repeats {
            let name = match repeat {
                RepeatArrays::Brackets => format!("{key}[]"),
                RepeatArrays::Key => key.clone(),
            };
            if let Value::Array(items) = value {
                for item in items {
                    form.append(name.clone(), scalar_text(item));
                }
            }
            continue;
        }
        let text = match value {
            Value::String(text) => text.clone(),
            other => crate::json::encode_body(other),
        };
        form.append(key.clone(), text);
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
