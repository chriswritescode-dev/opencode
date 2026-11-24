use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse, token_usage};
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::image_sizing::{Limits, Shape, Size, fit};
use crate::protocols::utils::media_input::{image_output, media_info, media_reference};
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "meta-images";
pub const NAME: &str = "Meta Images";
pub const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";

pub fn protocol() -> InlineProtocol<ImageRequest, ImageResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from("meta"),
        unsupported: Vec::new(),
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

pub fn path(is_edit: bool) -> String {
    format!("/images/{}", if is_edit { "edits" } else { "generations" })
}

fn body(request: &ImageRequest) -> Result<Body, AiError> {
    let mut references = Vec::new();
    for image in &request.images {
        let reference = media_reference(image, None, NAME).map_err(invalid_request)?;
        references.push(json!({ "image_url": reference.value() }));
    }
    let options = request.provider_options.as_ref();
    if options.and_then(|options| options.get("size")).is_some() && request.aspect.is_some() {
        return Err(invalid_request("Meta providerOptions.size replaces aspect; set one or the other"));
    }
    let hint = request.aspect.as_deref().map(|aspect| {
        fit(&Shape { aspect: Some(aspect), resolution: None }, &Limits { multiple: 16, ..Limits::default() })
    });
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if !references.is_empty() {
        base.insert("images".to_string(), Value::Array(references));
    }
    if let Some(n) = request.n {
        base.insert("n".to_string(), Value::from(n));
    }
    match hint {
        Some(hint) => {
            base.insert("size".to_string(), Value::String(size_string(hint)));
        }
        None => {
            if let Some(size) = options.and_then(|options| options.get("size")) {
                base.insert("size".to_string(), size.clone());
            }
        }
    }
    if let Some(value) = options.and_then(|options| options.get("outputFormat")) {
        base.insert("output_format".to_string(), value.clone());
    }
    if let Some(value) = options.and_then(|options| options.get("responseFormat")) {
        base.insert("response_format".to_string(), value.clone());
    }
    if let Some(value) = options.and_then(|options| options.get("reasoningStrength")) {
        base.insert("reasoning_strength".to_string(), value.clone());
    }
    if let Some(value) = options.and_then(|options| options.get("toolEnablement")) {
        base.insert("tool_enablement".to_string(), value.clone());
    }
    let native = options
        .map(|options| {
            options
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "outputFormat" | "responseFormat" | "reasoningStrength" | "toolEnablement" | "size"
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();
    let body = crate::json::merge_records([&base, &native, &http_body(request)]).unwrap_or(base);
    Ok(Body::Json { value: body, query: None })
}

fn size_string(size: Size) -> String {
    format!("{}x{}", size.width, size.height)
}

fn decode(text: &TextResponse, context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from("meta") };
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let decoded = value.as_object().ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let requested = context
        .request
        .http
        .as_ref()
        .and_then(|http| http.body.as_ref())
        .and_then(|body| body.get("output_format"))
        .and_then(Value::as_str);
    let format = decoded.get("output_format").and_then(Value::as_str).or(requested).unwrap_or("webp").to_string();
    let media_type = format!("image/{format}");
    let data = decoded.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut images = Vec::new();
    for (index, item) in data.iter().enumerate() {
        let item = item.as_object().cloned().unwrap_or_default();
        let image = image_output(
            &format!("{NAME} result {index}"),
            item.get("b64_json").and_then(Value::as_str),
            item.get("url").and_then(Value::as_str),
            Some(&media_type),
            Some(media_info(Some(&format), None)),
        )
        .map_err(invalid)?;
        images.push(image);
    }
    if images.is_empty() {
        return Err(invalid(format!("{NAME} returned no images")));
    }
    let usage = decoded.get("usage").and_then(Value::as_object).map(|usage| {
        let mut filtered = Map::new();
        for key in ["input_tokens", "output_tokens", "total_tokens"] {
            if let Some(value) = usage.get(key) {
                filtered.insert(key.to_string(), value.clone());
            }
        }
        let mut details = Map::new();
        details.insert("meta".to_string(), Value::Object(filtered));
        token_usage(
            usage.get("input_tokens").and_then(Value::as_number).cloned(),
            usage.get("output_tokens").and_then(Value::as_number).cloned(),
            usage.get("total_tokens").and_then(Value::as_number).cloned(),
            Some(details),
        )
    });
    let mut metadata = Map::new();
    metadata.insert("outputFormat".to_string(), Value::String(format));
    let mut provider_metadata = Map::new();
    provider_metadata.insert("meta".to_string(), Value::Object(metadata));
    Ok(ImageResponse { images, usage: usage.flatten(), provider_metadata: Some(provider_metadata as ProviderMetadata) })
}

fn http_body(request: &ImageRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}
