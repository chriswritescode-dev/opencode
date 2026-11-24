use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse};
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::image_sizing::tier;
use crate::protocols::utils::media_input::{MediaReference, image_output, media_reference, merge_body};
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "xai-images";
pub const NAME: &str = "xAI Images";
pub const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
pub const PATH: &str = "/images/generations";
pub const EDIT_PATH: &str = "/images/edits";
const PROVIDER: &str = "xai";

pub fn protocol() -> InlineProtocol<ImageRequest, ImageResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: Vec::new(),
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

pub fn path(is_edit: bool) -> String {
    if is_edit { EDIT_PATH.to_string() } else { PATH.to_string() }
}

pub fn resolution(request_resolution: Option<&str>) -> Option<String> {
    let resolution = request_resolution?;
    let tier = tier(resolution).unwrap_or(1.0);
    Some(if tier < 1.5 { "1k".to_string() } else { "2k".to_string() })
}

fn body(request: &ImageRequest) -> Result<Body, AiError> {
    let mut references = Vec::new();
    for image in &request.images {
        let reference = media_reference(image, Some(PROVIDER), NAME).map_err(invalid_request)?;
        references.push(match reference {
            MediaReference::Ref(value) => json!({ "file_id": value }),
            other => json!({ "url": other.value(), "type": "image_url" }),
        });
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if references.len() == 1 {
        base.insert("image".to_string(), references[0].clone());
    }
    if references.len() > 1 {
        base.insert("images".to_string(), Value::Array(references));
    }
    if let Some(n) = request.n {
        base.insert("n".to_string(), Value::from(n));
    }
    if let Some(aspect) = &request.aspect {
        base.insert("aspect_ratio".to_string(), Value::String(aspect.clone()));
    }
    if let Some(resolution) = resolution(request.resolution.as_deref()) {
        base.insert("resolution".to_string(), Value::String(resolution));
    }
    let native = native_options(request.provider_options.as_ref());
    let body = merge_body(base, Some(&native), request.http.as_ref());
    Ok(Body::Json { value: body, query: None })
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else {
        return crate::protocols::utils::media_input::object(vec![(
            "response_format",
            Value::String("b64_json".to_string()),
        )]);
    };
    let mut native = Map::new();
    native.insert(
        "response_format".to_string(),
        options.get("responseFormat").cloned().unwrap_or_else(|| Value::String("b64_json".to_string())),
    );
    for (key, value) in options {
        if key == "responseFormat" {
            continue;
        }
        native.insert(key.clone(), value.clone());
    }
    native
}

fn decode(text: &TextResponse, _context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = identity();
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let decoded = value.as_object().ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let data = decoded.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut images = Vec::new();
    for (index, item) in data.iter().enumerate() {
        let item = item.as_object().cloned().unwrap_or_default();
        let provider_metadata = item.get("revised_prompt").and_then(Value::as_str).map(|prompt| {
            let mut metadata = Map::new();
            metadata.insert("revisedPrompt".to_string(), Value::String(prompt.to_string()));
            let mut providers = Map::new();
            providers.insert(PROVIDER.to_string(), Value::Object(metadata));
            providers as ProviderMetadata
        });
        let image = image_output(
            &format!("{NAME} result {index}"),
            item.get("b64_json").and_then(Value::as_str),
            item.get("url").and_then(Value::as_str),
            item.get("mime_type").and_then(Value::as_str),
            None,
        )
        .map_err(invalid)?;
        images.push(match provider_metadata {
            Some(provider_metadata) => {
                crate::schema::MediaAsset { provider_metadata: Some(provider_metadata), ..image }
            }
            None => image,
        });
    }
    if images.is_empty() {
        return Err(invalid(format!("{NAME} returned no images")));
    }
    let provider_metadata = decoded.get("usage").and_then(Value::as_object).map(|usage| {
        let mut metadata = Map::new();
        metadata.insert("usage".to_string(), Value::Object(usage.clone()));
        let mut providers = Map::new();
        providers.insert(PROVIDER.to_string(), Value::Object(metadata));
        providers as ProviderMetadata
    });
    Ok(ImageResponse { images, usage: None, provider_metadata })
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}
