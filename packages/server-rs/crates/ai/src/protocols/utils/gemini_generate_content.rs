use serde_json::{Map, Number, Value, json};

use crate::media::route::Mode;
use crate::modalities::image::{MediaUsage, token_usage};
use crate::protocols::shared;
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::media_input;
use crate::schema::{AiError, AiErrorReason, MediaAsset, MediaPart, MediaSource, ProviderMetadata, ToolContent};

const PROVIDER: &str = "google";

const CONTENT_FILTER_REASONS: &[&str] = &[
    "IMAGE_SAFETY",
    "RECITATION",
    "SAFETY",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "MODEL_ARMOR",
    "IMAGE_PROHIBITED_CONTENT",
    "IMAGE_RECITATION",
    "LANGUAGE",
];

pub fn content_filtered(finish_reason: Option<&str>) -> bool {
    finish_reason.is_some_and(|finish_reason| CONTENT_FILTER_REASONS.contains(&finish_reason))
}

pub fn path(model: &str, mode: Mode) -> String {
    if mode == Mode::Stream {
        return format!("/models/{model}:streamGenerateContent?alt=sse");
    }
    format!("/models/{model}:generateContent")
}

#[derive(Default)]
pub struct Metadata {
    usage: Option<Map<String, Value>>,
    finish_reason: Option<String>,
    model_version: Option<String>,
    response_id: Option<String>,
}

impl Metadata {
    pub fn track(&mut self, object: &Map<String, Value>) {
        if let Some(usage) = object.get("usageMetadata").and_then(Value::as_object) {
            self.usage = Some(usage.clone());
        }
        if let Some(reason) = finish_reason(object) {
            self.finish_reason = Some(reason.to_string());
        }
        if let Some(version) = object.get("modelVersion").and_then(Value::as_str) {
            self.model_version = Some(version.to_string());
        }
        if let Some(id) = object.get("responseId").and_then(Value::as_str) {
            self.response_id = Some(id.to_string());
        }
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    pub fn usage(&self) -> Option<MediaUsage> {
        let usage = self.usage.as_ref()?;
        let input = token_count(usage, "promptTokenCount");
        let output = token_count(usage, "candidatesTokenCount");
        let reported = token_count(usage, "totalTokenCount");
        let total = reported.clone().or_else(|| {
            if input.is_none() && output.is_none() {
                return None;
            }
            number_from_f64(
                input.as_ref().and_then(Number::as_f64).unwrap_or_default()
                    + output.as_ref().and_then(Number::as_f64).unwrap_or_default(),
            )
        });
        let mut google = Map::new();
        for (key, value) in
            [("promptTokenCount", &input), ("candidatesTokenCount", &output), ("totalTokenCount", &reported)]
        {
            if let Some(value) = value {
                google.insert(key.to_string(), Value::Number(value.clone()));
            }
        }
        let mut details = Map::new();
        details.insert("google".to_string(), Value::Object(google));
        token_usage(input, output, total, Some(details))
    }

    pub fn provider_metadata(&self) -> ProviderMetadata {
        let mut google = Map::new();
        if let Some(finish_reason) = &self.finish_reason {
            google.insert("finishReason".to_string(), Value::String(finish_reason.clone()));
        }
        if let Some(model_version) = &self.model_version {
            google.insert("modelVersion".to_string(), Value::String(model_version.clone()));
        }
        if let Some(response_id) = &self.response_id {
            google.insert("responseId".to_string(), Value::String(response_id.clone()));
        }
        let mut metadata = Map::new();
        metadata.insert("google".to_string(), Value::Object(google));
        metadata
    }
}

pub fn blocked(name: &str, object: &Map<String, Value>, frame: &str) -> Option<AiError> {
    let feedback = object.get("promptFeedback").and_then(Value::as_object)?;
    let reason = feedback.get("blockReason").and_then(Value::as_str)?;
    let message = match feedback.get("blockReasonMessage").and_then(Value::as_str) {
        Some(message) => format!("{name} blocked the request ({reason}): {message}"),
        None => format!("{name} blocked the request ({reason})"),
    };
    Some(AiError { reason: AiErrorReason::ContentPolicy { message, body: Some(frame.to_string()), http: None } })
}

pub fn withheld(name: &str, finish_reason: Option<&str>, frame: &str) -> Option<AiError> {
    let finish_reason = finish_reason.filter(|reason| content_filtered(Some(reason)))?;
    Some(AiError {
        reason: AiErrorReason::ContentPolicy {
            message: format!("{name} withheld its output ({finish_reason})"),
            body: Some(frame.to_string()),
            http: None,
        },
    })
}

fn finish_reason(object: &Map<String, Value>) -> Option<&str> {
    object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str)
}

fn token_count(usage: &Map<String, Value>, key: &str) -> Option<Number> {
    usage.get(key).and_then(Value::as_f64).and_then(number_from_f64)
}

pub fn media_part(route: &str, asset: &MediaAsset) -> Result<Value, AiError> {
    let media_type = shared::media_type(asset);
    let file_uri = if shared::media_kind(&media_type) == "video"
        && let Some(url) = shared::media_url(asset).filter(|url| is_http_url(url))
    {
        Some(url.to_string())
    } else {
        media_input::ref_id(asset, PROVIDER)
    };
    if let Some(file_uri) = file_uri {
        return Ok(json!({ "fileData": { "mimeType": media_type, "fileUri": file_uri } }));
    }
    let inline = shared::require_inline_media(route, asset)?;
    Ok(json!({ "inlineData": { "mimeType": inline.mime, "data": inline.base64 } }))
}

pub fn tool_media(item: &ToolContent) -> Option<MediaPart> {
    if let ToolContent::File { uri, mime, name } = item
        && mime.starts_with("video/")
        && uri.starts_with("gs://")
    {
        return Some(MediaPart {
            media: MediaAsset {
                source: MediaSource::Ref {
                    provider: PROVIDER.to_string(),
                    id: uri.clone(),
                    media_type: Some(mime.clone()),
                },
                info: None,
                provider_metadata: None,
                headers: None,
            },
            filename: name.clone(),
            cache: None,
            provider_metadata: None,
        });
    }
    shared::tool_file_media(item)
}

fn is_http_url(url: &str) -> bool {
    url.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || url.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}
