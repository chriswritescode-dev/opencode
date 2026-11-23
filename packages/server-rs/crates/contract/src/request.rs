use crate::codec::{Decode, DecodeError, Path, decode_json};

pub struct RawRequest {
    pub path_params: Vec<(String, String)>,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct RequestSlot {
    pub operation_id: &'static str,
    pub slot: &'static str,
    pub decode: fn(&RawRequest) -> Result<String, DecodeError>,
}

pub fn parse_query(input: &str) -> Vec<(String, Vec<String>)> {
    let bytes = input.as_bytes();
    let mut result: Vec<(String, Vec<String>)> = Vec::new();
    let mut start = 0;
    let mut index = 0;
    loop {
        let code = if index < bytes.len() { bytes[index] } else { b'&' };
        if code == b'&' {
            push_pair(&input[start..index], &mut result);
            start = index + 1;
        }
        if index == bytes.len() {
            break;
        }
        index += 1;
    }
    result
}

fn push_pair(pair: &str, result: &mut Vec<(String, Vec<String>)>) {
    let separator = pair.find('=');
    let (raw_key, raw_value) = match separator {
        Some(index) => (&pair[..index], Some(&pair[index + 1..])),
        None => (pair, None),
    };
    if raw_value.is_none() && raw_key.is_empty() {
        return;
    }
    let key = decode_form(raw_key);
    let value = raw_value.map(decode_form).unwrap_or_default();
    if let Some((_, values)) = result.iter_mut().find(|(name, _)| *name == key) {
        values.push(value);
        return;
    }
    result.push((key, vec![value]));
}

fn decode_form(value: &str) -> String {
    if !value.contains(['%', '+']) {
        return value.to_owned();
    }
    let replaced = value.replace('+', " ");
    match opencode_api_types::js::decode_component(&replaced) {
        Some(decoded) if !decoded.is_empty() => decoded,
        _ => replaced,
    }
}

pub fn value_pairs(pairs: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut result: Vec<(String, Vec<String>)> = Vec::new();
    for (key, value) in pairs {
        if let Some((_, values)) = result.iter_mut().find(|(name, _)| name == key) {
            values.push(value.clone());
            continue;
        }
        result.push((key.clone(), vec![value.clone()]));
    }
    result
}

pub fn field_values<'a>(parsed: &'a [(String, Vec<String>)], key: &str) -> Option<&'a [String]> {
    parsed.iter().find(|(name, _)| name == key).map(|(_, values)| values.as_slice())
}

pub fn single<'a>(values: &'a [String], path: &Path) -> Result<&'a str, DecodeError> {
    match values {
        [value] => Ok(value),
        _ => Err(path.at("expected a single value")),
    }
}

pub fn number_value(text: &str, path: &Path) -> Result<serde_json::Value, DecodeError> {
    match opencode_api_types::js::parse_number(text).and_then(serde_json::Number::from_f64) {
        Some(number) => Ok(serde_json::Value::Number(number)),
        None => Err(path.at("expected a number")),
    }
}

pub fn boolean_value(text: &str, path: &Path) -> Result<serde_json::Value, DecodeError> {
    match text {
        "true" => Ok(serde_json::Value::Bool(true)),
        "false" => Ok(serde_json::Value::Bool(false)),
        _ => Err(path.at("expected a boolean")),
    }
}

pub fn location_directory(request: &RawRequest) -> Option<String> {
    let directory = url::form_urlencoded::parse(request.query.as_bytes())
        .find(|(key, _)| key == "location[directory]")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty());
    if let Some(value) = directory {
        return Some(value);
    }
    let header = request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("x-opencode-directory"))
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())?;
    Some(opencode_api_types::js::decode_component(header).unwrap_or_else(|| header.clone()))
}

pub fn wildcard(request: &RawRequest) -> Option<&str> {
    request.path_params.iter().find(|(key, _)| key == "*").map(|(_, value)| value.as_str())
}

pub fn json_body<T: Decode>(request: &RawRequest) -> Result<T, DecodeError> {
    let text =
        std::str::from_utf8(&request.body).map_err(|error| Path::new().at(format!("expected UTF-8 JSON: {error}")))?;
    decode_json(text)
}
