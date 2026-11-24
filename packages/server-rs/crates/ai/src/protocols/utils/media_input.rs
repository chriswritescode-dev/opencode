use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Query, QueryValue};
use crate::protocols::utils::media_type::detect_media_type;
use crate::schema::{AiError, MediaAsset, MediaInfo, MediaSource, ProviderMetadata, StringMap};

pub fn ref_id(asset: &MediaAsset, provider: &str) -> Option<String> {
    let MediaSource::Ref { provider: source_provider, id, .. } = &asset.source else {
        return None;
    };
    if source_provider != provider {
        return None;
    }
    Some(id.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaReference {
    DataUrl(String),
    Url(String),
    Ref(String),
}

impl MediaReference {
    pub fn value(self) -> String {
        match self {
            MediaReference::DataUrl(value) | MediaReference::Url(value) | MediaReference::Ref(value) => value,
        }
    }
}

pub fn media_reference(asset: &MediaAsset, provider: Option<&str>, label: &str) -> Result<MediaReference, String> {
    if let Some(inline) = crate::protocols::shared::inline_media(asset) {
        return Ok(MediaReference::DataUrl(inline.data_url));
    }
    if let MediaSource::Url { url, .. } = &asset.source {
        return Ok(MediaReference::Url(url.clone()));
    }
    if let Some(provider) = provider
        && let MediaSource::Ref { provider: source_provider, id, .. } = &asset.source
        && source_provider == provider
    {
        return Ok(MediaReference::Ref(id.clone()));
    }
    let accepted = match provider {
        Some(provider) => format!(", and {provider} references"),
        None => String::new(),
    };
    let got = match &asset.source {
        MediaSource::Ref { provider, id, .. } => format!("; got {provider}:{id}"),
        _ => String::new(),
    };
    Err(format!("{label} accepts inline bytes, data URLs, http(s) URLs{accepted}{got}"))
}

pub fn inline_bytes(route: &str, asset: &MediaAsset) -> Result<Vec<u8>, String> {
    let data = match &asset.source {
        MediaSource::Bytes { data, .. } | MediaSource::Base64 { data, .. } => data,
        MediaSource::Url { .. } | MediaSource::Ref { .. } => {
            return Err(format!(
                "{route} requires inline media (bytes or base64); {} sources must be materialized first",
                source_type(&asset.source)
            ));
        }
    };
    BASE64_STANDARD.decode(data).map_err(|_| format!("{route} media contains invalid base64 data"))
}

pub fn query(route: &str, values: &Map<String, Value>) -> Result<Query, AiError> {
    let invalid = |key: &str| {
        crate::protocols::shared::invalid_request(format!("{route} cannot send \"{key}\" as a query parameter"))
    };
    let mut query = Vec::with_capacity(values.len());
    for (key, value) in values {
        let item = match value {
            Value::String(value) => QueryValue::One(value.clone()),
            Value::Number(value) => QueryValue::One(value.to_string()),
            Value::Bool(value) => QueryValue::One(value.to_string()),
            Value::Array(items) => QueryValue::Many(
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(value) => Ok(value.clone()),
                        Value::Number(value) => Ok(value.to_string()),
                        Value::Bool(value) => Ok(value.to_string()),
                        _ => Err(invalid(key)),
                    })
                    .collect::<Result<Vec<String>, AiError>>()?,
            ),
            _ => return Err(invalid(key)),
        };
        query.push((key.clone(), item));
    }
    Ok(query)
}

pub fn decoded_asset(
    label: &str,
    data: &str,
    media_type: Option<&str>,
    info: Option<MediaInfo>,
    provider_metadata: Option<ProviderMetadata>,
) -> Result<MediaAsset, String> {
    let bytes = BASE64_STANDARD.decode(data).map_err(|_| format!("{label} contains invalid base64 data"))?;
    let media_type = media_type
        .map(str::to_string)
        .or_else(|| detect_media_type(&bytes).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok(MediaAsset {
        source: MediaSource::Bytes { data: data.to_string(), media_type },
        info,
        provider_metadata,
        headers: None,
    })
}

pub fn image_output(
    label: &str,
    b64_json: Option<&str>,
    url: Option<&str>,
    media_type: Option<&str>,
    info: Option<MediaInfo>,
) -> Result<MediaAsset, String> {
    if let Some(data) = b64_json.filter(|value| !value.is_empty()) {
        return decoded_asset(label, data, media_type, info, None);
    }
    if let Some(url) = url {
        if let Some((mime, data)) = parse_data_url(url) {
            return Ok(MediaAsset {
                source: MediaSource::Base64 { data, media_type: media_type.unwrap_or(mime).to_string() },
                info,
                provider_metadata: None,
                headers: None,
            });
        }
        if url.starts_with("data:") {
            return Err(format!("{label} has an invalid data URL"));
        }
        return Ok(MediaAsset {
            source: MediaSource::Url {
                url: url.to_string(),
                media_type: media_type.map(str::to_string),
                expires_at: None,
            },
            info,
            provider_metadata: None,
            headers: None,
        });
    }
    Err(format!("{label} has neither image data nor a URL"))
}

pub fn bytes_asset(bytes: &[u8]) -> MediaAsset {
    let media_type =
        detect_media_type(bytes).map(str::to_string).unwrap_or_else(|| "application/octet-stream".to_string());
    MediaAsset {
        source: MediaSource::Bytes { data: BASE64_STANDARD.encode(bytes), media_type },
        info: None,
        provider_metadata: None,
        headers: None,
    }
}

pub fn expiring_url(url: &str, retention_ms: u64, now_ms: u64) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Url {
            url: url.to_string(),
            media_type: None,
            expires_at: Some(Number::from(now_ms.saturating_add(retention_ms))),
        },
        info: None,
        provider_metadata: None,
        headers: None,
    }
}

pub fn url_asset(
    url: &str,
    media_type: Option<&str>,
    expires_at: Option<Number>,
    headers: Option<StringMap>,
) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Url { url: url.to_string(), media_type: media_type.map(str::to_string), expires_at },
        info: None,
        provider_metadata: None,
        headers,
    }
}

pub fn media_info(format: Option<&str>, size: Option<&str>) -> MediaInfo {
    let dimensions = size.and_then(parse_size);
    MediaInfo {
        width: dimensions.map(|(width, _)| Number::from(width)),
        height: dimensions.map(|(_, height)| Number::from(height)),
        format: format.map(str::to_string),
        ..MediaInfo::default()
    }
}

fn parse_size(value: &str) -> Option<(i64, i64)> {
    let (width, height) = value.split_once('x')?;
    if width.is_empty() || height.is_empty() {
        return None;
    }
    if !width.chars().all(|character| character.is_ascii_digit())
        || !height.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn parse_data_url(value: &str) -> Option<(&str, String)> {
    let rest = value.strip_prefix("data:")?;
    let (metadata, data) = rest.split_once(',')?;
    let mime = metadata.strip_suffix(";base64")?;
    if mime.is_empty() {
        return None;
    }
    Some((mime, data.to_string()))
}

fn source_type(source: &MediaSource) -> &'static str {
    match source {
        MediaSource::Bytes { .. } => "bytes",
        MediaSource::Base64 { .. } => "base64",
        MediaSource::Url { .. } => "url",
        MediaSource::Ref { .. } => "ref",
    }
}

pub fn object(entries: Vec<(impl Into<String>, Value)>) -> Map<String, Value> {
    entries.into_iter().map(|(key, value)| (key.into(), value)).collect()
}

pub fn merge_body(
    base: Map<String, Value>,
    native: Option<&Map<String, Value>>,
    http: Option<&crate::schema::HttpOptions>,
) -> Map<String, Value> {
    let http_body = http.and_then(|http| http.body.clone());
    let merged = {
        let mut items: Vec<&Map<String, Value>> = vec![&base];
        if let Some(native) = native {
            items.push(native);
        }
        if let Some(body) = http_body.as_ref() {
            items.push(body);
        }
        crate::json::merge_records(items)
    };
    merged.unwrap_or(base)
}
