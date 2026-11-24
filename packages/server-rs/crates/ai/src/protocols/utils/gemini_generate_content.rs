use serde_json::{Value, json};

use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::schema::{AiError, MediaAsset, MediaPart, MediaSource, ToolContent};

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
