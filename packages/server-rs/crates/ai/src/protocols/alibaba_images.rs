use std::sync::Arc;

use serde_json::{Map, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse};
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::image_sizing::{Limits, exact, shape};
use crate::protocols::utils::media_input::{expiring_url, media_reference, merge_body};
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "alibaba-images";
pub const NAME: &str = "Alibaba Images";
pub const PATH: &str = "/services/aigc/multimodal-generation/generation";
const PROVIDER: &str = "alibaba";
const OUTPUT_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;

pub fn protocol() -> InlineProtocol<ImageRequest, ImageResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: Vec::new(),
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

fn body(request: &ImageRequest) -> Result<Body, AiError> {
    let identity = identity();
    let mut content = Vec::new();
    for image in &request.images {
        let reference = media_reference(image, None, &format!("{NAME} input")).map_err(invalid_request)?;
        content.push(object([("image", Value::String(reference.value()))]));
    }
    content.push(object([("text", Value::String(request.prompt.clone()))]));
    let input = object([(
        "messages",
        Value::Array(vec![object([("role", Value::String("user".to_string())), ("content", Value::Array(content))])]),
    )]);
    let size = exact(
        &identity,
        &shape(request),
        &Limits { multiple: 16, min_pixels: Some(512 * 512), max_pixels: Some(2048 * 2048), ..Limits::default() },
    )?;
    let mut parameters = Map::new();
    if let Some(size) = size {
        parameters.insert("size".to_string(), Value::String(format!("{}*{}", size.width, size.height)));
    }
    if let Some(n) = request.n {
        parameters.insert("n".to_string(), Value::from(n));
    }
    let native = native_options(request.provider_options.as_ref());
    let parameters = crate::json::merge_records([&parameters, &native]).unwrap_or_default();
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("input".to_string(), input);
    if !parameters.is_empty() {
        base.insert("parameters".to_string(), Value::Object(parameters));
    }
    let body = merge_body(base, None, request.http.as_ref());
    Ok(Body::Json { value: body, query: None })
}

fn decode(text: &TextResponse, _context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = identity();
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let object = value.as_object().ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let output = object
        .get("output")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let choices = output
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let urls = choices
        .iter()
        .flat_map(|choice| {
            choice
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .map(|content| content.as_slice())
                .unwrap_or_default()
        })
        .filter_map(|item| item.get("image").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(invalid(format!("{NAME} returned no images")));
    }
    let images = urls.iter().map(|url| expiring_url(url, OUTPUT_RETENTION_MS, _context.now_ms)).collect();
    let mut metadata = Map::new();
    if let Some(request_id) = object.get("request_id").and_then(Value::as_str) {
        metadata.insert("requestID".to_string(), Value::String(request_id.to_string()));
    }
    if let Some(usage) = object.get("usage").and_then(Value::as_object) {
        metadata.insert("usage".to_string(), Value::Object(usage.clone()));
    }
    let mut provider_metadata = Map::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(ImageResponse { images, usage: None, provider_metadata: Some(provider_metadata as ProviderMetadata) })
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else {
        return Map::new();
    };
    let mut native = Map::new();
    if let Some(value) = options.get("negativePrompt") {
        native.insert("negative_prompt".to_string(), value.clone());
    }
    if let Some(value) = options.get("promptExtend") {
        native.insert("prompt_extend".to_string(), value.clone());
    }
    if let Some(value) = options.get("thinkingMode") {
        native.insert("thinking_mode".to_string(), value.clone());
    }
    if let Some(value) = options.get("enableSequential") {
        native.insert("enable_sequential".to_string(), value.clone());
    }
    for (key, value) in options {
        if matches!(key.as_str(), "negativePrompt" | "promptExtend" | "thinkingMode" | "enableSequential") {
            continue;
        }
        native.insert(key.clone(), value.clone());
    }
    native
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(entries.into_iter().map(|(key, value)| (key.to_string(), value)).collect())
}
