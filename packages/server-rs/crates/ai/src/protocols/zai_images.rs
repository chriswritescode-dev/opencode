use std::sync::Arc;

use serde_json::{Map, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse};
use crate::protocols::openai_images::shape;
use crate::protocols::utils::image_sizing::{Limits, exact};
use crate::protocols::utils::media_input::expiring_url;
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "zai-images";
pub const NAME: &str = "Z.ai Images";
pub const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
pub const PATH: &str = "/images/generations";
const PROVIDER: &str = "zai";
const OUTPUT_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;

pub fn protocol() -> InlineProtocol<ImageRequest, ImageResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: vec!["images", "n"],
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

fn body(request: &ImageRequest) -> Result<Body, AiError> {
    let identity = identity();
    let limits = if request.model.id.starts_with("glm-image") {
        Limits {
            multiple: 32,
            min_edge: Some(1024),
            max_edge: Some(2048),
            max_pixels: Some(4_194_304),
            ..Limits::default()
        }
    } else {
        Limits {
            multiple: 16,
            min_edge: Some(512),
            max_edge: Some(2048),
            max_pixels: Some(2_097_152),
            ..Limits::default()
        }
    };
    let size = exact(&identity, &shape(request), &limits)?;
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if let Some(size) = size {
        base.insert("size".to_string(), Value::String(format!("{}x{}", size.width, size.height)));
    }
    let native = native_options(request.provider_options.as_ref());
    let body = crate::json::merge_records([&base, &native, &http_body(request)]).unwrap_or(base);
    Ok(Body::Json { value: body, query: None })
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else {
        return Map::new();
    };
    let mut native = Map::new();
    if let Some(value) = options.get("userID") {
        native.insert("user_id".to_string(), value.clone());
    }
    for (key, value) in options {
        if key == "userID" {
            continue;
        }
        native.insert(key.clone(), value.clone());
    }
    native
}

fn decode(text: &TextResponse, context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = identity();
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let decoded = value.as_object().ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let data = decoded.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    let urls = data.iter().filter_map(|item| item.get("url").and_then(Value::as_str));
    let images = urls.map(|url| expiring_url(url, OUTPUT_RETENTION_MS, context.now_ms)).collect::<Vec<_>>();
    if images.is_empty() {
        return Err(invalid(format!("{NAME} returned no images")));
    }
    let mut metadata = Map::new();
    insert_optional(&mut metadata, "created", decoded.get("created"));
    insert_optional(&mut metadata, "id", decoded.get("id"));
    insert_optional(&mut metadata, "requestID", decoded.get("request_id"));
    insert_optional(&mut metadata, "contentFilter", decoded.get("content_filter"));
    let mut provider_metadata = Map::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(ImageResponse { images, usage: None, provider_metadata: Some(provider_metadata as ProviderMetadata) })
}

fn insert_optional(target: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    let Some(value) = value else { return };
    if !value.is_null() {
        target.insert(key.to_string(), value.clone());
    }
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn http_body(request: &ImageRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}
