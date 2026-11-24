use std::sync::Arc;

use serde_json::{Map, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse};
use crate::protocols::openai_images::shape;
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::image_sizing::{Limits, Size, exact, is_requested, nearest, tier};
use crate::protocols::utils::media_input::{decoded_asset, expiring_url, media_reference};
use crate::schema::{AiError, AiErrorReason, HttpContext, ProviderId, ProviderMetadata};

pub const ID: &str = "minimax-images";
pub const NAME: &str = "MiniMax Images";
pub const DEFAULT_BASE_URL: &str = "https://api.minimax.io/v1";
pub const PATH: &str = "/image_generation";
const PROVIDER: &str = "minimax";
const OUTPUT_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;
const INSUFFICIENT_BALANCE: i64 = 1008;
const CONTENT_POLICY: i64 = 1026;
const LIMITS: Limits = Limits {
    multiple: 8,
    min_edge: Some(512),
    max_edge: Some(2048),
    min_pixels: None,
    max_pixels: None,
    max_ratio: None,
};

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
    if request.images.len() > 1 {
        return Err(identity.unsupported("media.images", format!("{NAME} takes one subject reference image")));
    }
    if is_native_size(request.provider_options.as_ref()) && is_requested(&shape(request)) {
        return Err(invalid_request(
            "MiniMax providerOptions aspect_ratio, width and height replace aspect and resolution; set one or the other",
        ));
    }
    let mut references = Vec::new();
    for image in &request.images {
        let reference = media_reference(image, None, &format!("{NAME} input")).map_err(invalid_request)?;
        references.push(crate::protocols::utils::media_input::object(vec![
            ("type", Value::String("character".to_string())),
            ("image_file", Value::String(reference.value())),
        ]));
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if !references.is_empty() {
        base.insert("subject_reference".to_string(), Value::Array(references.into_iter().map(Value::Object).collect()));
    }
    if let Some(n) = request.n {
        base.insert("n".to_string(), Value::from(n));
    }
    for (key, value) in sizing(&identity, request)? {
        base.insert(key, value);
    }
    let native = native_options(request.provider_options.as_ref());
    let body = crate::json::merge_records([&base, &native, &http_body(request)]).unwrap_or(base);
    Ok(Body::Json { value: body, query: None })
}

fn sizing(identity: &Identity, request: &ImageRequest) -> Result<Map<String, Value>, AiError> {
    if !is_requested(&shape(request)) {
        return Ok(Map::new());
    }
    if let Some(aspect) = preset(request) {
        return Ok(crate::protocols::utils::media_input::object(vec![("aspect_ratio", Value::String(aspect))]));
    }
    if request.model.id == "image-01" {
        let mut sizes = Map::new();
        if let Some(size) = exact(identity, &shape(request), &LIMITS)? {
            sizes.insert("width".to_string(), Value::from(size.width));
            sizes.insert("height".to_string(), Value::from(size.height));
        }
        return Ok(sizes);
    }
    let available = presets(&request.model.id);
    let nearest = nearest(&shape(request), &available.iter().map(|(_, size)| *size).collect::<Vec<Size>>());
    let name = available.iter().find(|(_, size)| *size == nearest).map(|(name, _)| name.clone()).unwrap_or_default();
    let requested = [request.aspect.clone(), request.resolution.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<String>>()
        .join(" at ");
    let operation = if request.resolution.is_none() { "media.aspect" } else { "media.resolution" };
    Err(identity.unsupported(
        operation,
        format!(
            "{} can't render {requested}; the closest size it takes is {}x{} ({name}). Change aspect or resolution, or set aspect_ratio in providerOptions",
            request.model.id, nearest.width, nearest.height
        ),
    ))
}

fn presets(model: &str) -> Vec<(String, Size)> {
    PRESET_SIZES
        .iter()
        .filter(|(aspect, _)| model == "image-01" || *aspect != "21:9")
        .map(|(aspect, size)| ((*aspect).to_string(), *size))
        .collect()
}

const PRESET_SIZES: [(&str, Size); 8] = [
    ("1:1", Size { width: 1024, height: 1024 }),
    ("16:9", Size { width: 1280, height: 720 }),
    ("4:3", Size { width: 1152, height: 864 }),
    ("3:2", Size { width: 1248, height: 832 }),
    ("2:3", Size { width: 832, height: 1248 }),
    ("3:4", Size { width: 864, height: 1152 }),
    ("9:16", Size { width: 720, height: 1280 }),
    ("21:9", Size { width: 1344, height: 576 }),
];

fn preset(request: &ImageRequest) -> Option<String> {
    let aspect = request.aspect.as_deref().unwrap_or("1:1");
    if !presets(&request.model.id).iter().any(|(name, _)| name == aspect) {
        return None;
    }
    if request.resolution.as_deref().and_then(tier).is_some_and(|tier| (tier - 1.0).abs() > f64::EPSILON) {
        return None;
    }
    Some(aspect.to_string())
}

fn is_native_size(options: Option<&Map<String, Value>>) -> bool {
    options.is_some_and(|options| {
        options.contains_key("aspect_ratio") || options.contains_key("width") || options.contains_key("height")
    })
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else {
        return crate::protocols::utils::media_input::object(vec![(
            "response_format",
            Value::String("base64".to_string()),
        )]);
    };
    let mut native = Map::new();
    native.insert(
        "response_format".to_string(),
        options.get("responseFormat").cloned().unwrap_or_else(|| Value::String("base64".to_string())),
    );
    if let Some(value) = options.get("promptOptimizer") {
        native.insert("prompt_optimizer".to_string(), value.clone());
    }
    for (key, value) in options {
        if matches!(key.as_str(), "responseFormat" | "promptOptimizer") {
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
    if let Some(status) = decoded.get("base_resp").and_then(Value::as_object) {
        let code = status.get("status_code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let message = format!(
                "{NAME} failed ({code}): {}",
                status.get("status_msg").and_then(Value::as_str).unwrap_or("unknown error")
            );
            if code == CONTENT_POLICY {
                return Err(AiError {
                    reason: AiErrorReason::ContentPolicy {
                        message,
                        body: Some(text.body.clone()),
                        http: Some(text.http.clone()),
                    },
                });
            }
            if code == INSUFFICIENT_BALANCE {
                return Err(AiError {
                    reason: AiErrorReason::QuotaExceeded {
                        message,
                        body: Some(text.body.clone()),
                        http: Some(text.http.clone()),
                        retry_after_ms: None,
                    },
                });
            }
            return Err(AiError { reason: ended(&message, text, code) });
        }
    }
    let data = decoded.get("data").and_then(Value::as_object);
    let mut images = Vec::new();
    if let Some(data) = data {
        for (index, entry) in data.get("image_base64").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let Some(entry) = entry.as_str() else { continue };
            images.push(decoded_asset(&format!("{NAME} result {index}"), entry, None, None, None).map_err(invalid)?);
        }
        for url in data.get("image_urls").and_then(Value::as_array).into_iter().flatten() {
            if let Some(url) = url.as_str() {
                images.push(expiring_url(url, OUTPUT_RETENTION_MS, context.now_ms));
            }
        }
    }
    let failed =
        decoded.get("metadata").and_then(|metadata| metadata.get("failed_count")).map(json_number).unwrap_or(0.0);
    if images.is_empty() && failed > 0.0 {
        return Err(AiError {
            reason: AiErrorReason::ContentPolicy {
                message: format!("{NAME} blocked {} image(s) for content safety", display_number(failed)),
                body: Some(text.body.clone()),
                http: Some(text.http.clone()),
            },
        });
    }
    if images.is_empty() {
        return Err(invalid(format!("{NAME} returned no images")));
    }
    let mut metadata = Map::new();
    insert_optional(&mut metadata, "id", decoded.get("id"));
    insert_optional(
        &mut metadata,
        "successCount",
        decoded.get("metadata").and_then(|metadata| metadata.get("success_count")),
    );
    insert_optional(
        &mut metadata,
        "failedCount",
        decoded.get("metadata").and_then(|metadata| metadata.get("failed_count")),
    );
    let mut provider_metadata = Map::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(ImageResponse { images, usage: None, provider_metadata: Some(provider_metadata as ProviderMetadata) })
}

fn ended(message: &str, text: &TextResponse, code: i64) -> AiErrorReason {
    let failure = crate::media::protocol::failure(&FAILURE, Some(&Value::from(code)));
    let http: Option<HttpContext> = Some(text.http.clone());
    match failure {
        crate::media::protocol::Failure::InvalidRequest => AiErrorReason::InvalidRequest {
            message: message.to_string(),
            body: Some(text.body.clone()),
            http,
            parameter: None,
            classification: None,
        },
        crate::media::protocol::Failure::Authentication => {
            AiErrorReason::Authentication { message: message.to_string(), body: Some(text.body.clone()), http }
        }
        crate::media::protocol::Failure::RateLimit => AiErrorReason::RateLimit {
            message: message.to_string(),
            body: Some(text.body.clone()),
            http,
            retry_after_ms: None,
            rate_limit: None,
        },
        crate::media::protocol::Failure::ProviderInternal => AiErrorReason::ProviderInternal {
            message: message.to_string(),
            body: Some(text.body.clone()),
            http,
            retry_after_ms: None,
        },
    }
}

const FAILURE: [(&str, crate::media::protocol::Failure); 4] = [
    ("1002", crate::media::protocol::Failure::RateLimit),
    ("1004", crate::media::protocol::Failure::Authentication),
    ("2013", crate::media::protocol::Failure::InvalidRequest),
    ("2049", crate::media::protocol::Failure::Authentication),
];

fn json_number(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().unwrap_or(0.0),
        Value::String(text) => text.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn display_number(value: f64) -> String {
    if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") }
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
