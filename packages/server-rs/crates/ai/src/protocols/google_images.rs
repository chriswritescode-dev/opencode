use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse, token_usage};
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::gemini_generate_content::{content_filtered, media_part};
use crate::protocols::utils::media_input::{decoded_asset, merge_body};
use crate::schema::{AiError, AiErrorReason, ProviderId, ProviderMetadata};

pub const ID: &str = "google-images";
pub const NAME: &str = "Google Images";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const PROVIDER: &str = "google";

pub fn protocol() -> InlineProtocol<ImageRequest, ImageResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: Vec::new(),
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

pub fn path(model: &str) -> String {
    format!("/models/{model}:generateContent")
}

fn body(request: &ImageRequest) -> Result<Body, AiError> {
    let identity = identity();
    if let Some(n) = request.n.filter(|n| *n > 1) {
        return Err(identity.unsupported(
            "media.n",
            format!("{NAME} generates one image per request; call it once per image instead of n={n}"),
        ));
    }
    let mut parts = vec![json!({ "text": request.prompt })];
    for image in &request.images {
        parts.push(media_part(NAME, image)?);
    }
    let contents = vec![json!({ "role": "user", "parts": parts })];
    let mut base = Map::new();
    base.insert("contents".to_string(), Value::Array(contents));
    base.insert("generationConfig".to_string(), Value::Object(generation_config(request)));
    let body = merge_body(base, None, request.http.as_ref());
    Ok(Body::Json { value: body, query: None })
}

fn generation_config(request: &ImageRequest) -> Map<String, Value> {
    let Some(options) = request.provider_options.as_ref() else {
        return merge_config(request, Map::new());
    };
    merge_config(request, {
        let mut native = Map::new();
        for (key, value) in options {
            if matches!(key.as_str(), "thinkingLevel" | "includeThoughts") {
                continue;
            }
            native.insert(key.clone(), value.clone());
        }
        native
    })
}

fn merge_config(request: &ImageRequest, native: Map<String, Value>) -> Map<String, Value> {
    let options = request.provider_options.as_ref();
    let mut base = Map::new();
    base.insert("responseModalities".to_string(), json!(["IMAGE"]));
    let image_size = request.resolution.as_deref().map(|resolution| {
        if resolution.eq_ignore_ascii_case("0.5K") { "512".to_string() } else { resolution.to_ascii_uppercase() }
    });
    if request.aspect.is_some() || image_size.is_some() {
        let mut image_config = Map::new();
        if let Some(aspect) = &request.aspect {
            image_config.insert("aspectRatio".to_string(), Value::String(aspect.clone()));
        }
        if let Some(image_size) = &image_size {
            image_config.insert("imageSize".to_string(), Value::String(image_size.clone()));
        }
        base.insert("imageConfig".to_string(), Value::Object(image_config));
    }
    let thinking_level = options.and_then(|options| options.get("thinkingLevel")).cloned();
    let include_thoughts = options.and_then(|options| options.get("includeThoughts")).cloned();
    if thinking_level.is_some() || include_thoughts.is_some() {
        let mut thinking_config = Map::new();
        if let Some(thinking_level) = thinking_level {
            thinking_config.insert("thinkingLevel".to_string(), thinking_level);
        }
        if let Some(include_thoughts) = include_thoughts {
            thinking_config.insert("includeThoughts".to_string(), include_thoughts);
        }
        base.insert("thinkingConfig".to_string(), Value::Object(thinking_config));
    }
    crate::json::merge_records([&base, &native]).unwrap_or(base)
}

fn decode(text: &TextResponse, _context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = identity();
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let decoded = value.as_object().ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let candidates = decoded.get("candidates").and_then(Value::as_array).cloned().unwrap_or_default();
    let candidate_metadata = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let parts = candidate
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let parts = parts.iter().map(candidate_part).collect::<Vec<Value>>();
            let mut metadata = Map::new();
            metadata.insert("index".to_string(), candidate.get("index").cloned().unwrap_or(Value::from(index as i64)));
            insert_optional(&mut metadata, "finishReason", candidate.get("finishReason"));
            insert_optional(&mut metadata, "finishMessage", candidate.get("finishMessage"));
            insert_optional(&mut metadata, "safetyRatings", candidate.get("safetyRatings"));
            insert_optional(&mut metadata, "citationMetadata", candidate.get("citationMetadata"));
            insert_optional(&mut metadata, "groundingMetadata", candidate.get("groundingMetadata"));
            metadata.insert("parts".to_string(), Value::Array(parts));
            Value::Object(metadata)
        })
        .collect::<Vec<Value>>();
    let mut encoded = Vec::new();
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let parts = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (part_index, part) in parts.iter().enumerate() {
            let Some(inline) = part.get("inlineData").and_then(Value::as_object) else {
                continue;
            };
            if part.get("thought") == Some(&Value::Bool(true)) {
                continue;
            }
            encoded.push((candidate_index, part_index, candidate.clone(), part.clone(), inline.clone()));
        }
    }
    let mut images = Vec::new();
    for (candidate_index, part_index, candidate, part, inline) in encoded {
        let mime_type = inline.get("mimeType").and_then(Value::as_str).unwrap_or_default();
        let data = inline.get("data").and_then(Value::as_str).unwrap_or_default();
        let mut google = Map::new();
        google.insert(
            "candidateIndex".to_string(),
            candidate.get("index").cloned().unwrap_or(Value::from(candidate_index as i64)),
        );
        google.insert("partIndex".to_string(), Value::from(part_index as i64));
        insert_optional(&mut google, "finishReason", candidate.get("finishReason"));
        insert_optional(&mut google, "safetyRatings", candidate.get("safetyRatings"));
        insert_optional(&mut google, "citationMetadata", candidate.get("citationMetadata"));
        insert_optional(&mut google, "groundingMetadata", candidate.get("groundingMetadata"));
        insert_optional(&mut google, "thoughtSignature", part.get("thoughtSignature"));
        let mut provider_metadata = Map::new();
        provider_metadata.insert(PROVIDER.to_string(), Value::Object(google));
        let image = decoded_asset(
            &format!("{NAME} candidate {candidate_index} part {part_index}"),
            data,
            Some(mime_type),
            None,
            Some(provider_metadata as ProviderMetadata),
        )
        .map_err(invalid)?;
        images.push(image);
    }
    if images.is_empty() {
        let finish_reasons = candidates
            .iter()
            .filter_map(|candidate| candidate.get("finishReason").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        if finish_reasons.iter().any(|reason| content_filtered(Some(reason))) {
            return Err(AiError {
                reason: AiErrorReason::ContentPolicy {
                    message: format!("{NAME} withheld its images (finish reasons: {})", finish_reasons.join(", ")),
                    body: Some(text.body.clone()),
                    http: Some(text.http.clone()),
                },
            });
        }
        let suffix = if finish_reasons.is_empty() {
            String::new()
        } else {
            format!(" (finish reasons: {})", finish_reasons.join(", "))
        };
        return Err(invalid(format!(
            "{NAME} returned no final images{suffix}; inspect body for prompt feedback and candidate details"
        )));
    }
    let usage_object = decoded.get("usageMetadata").and_then(Value::as_object);
    let usage = usage_object.and_then(|usage| {
        let input = usage.get("promptTokenCount").and_then(Value::as_f64);
        let output = usage
            .get("candidatesTokenCount")
            .and_then(Value::as_f64)
            .map(|candidates| candidates + usage.get("thoughtsTokenCount").and_then(Value::as_f64).unwrap_or(0.0));
        let total = match usage.get("totalTokenCount").and_then(Value::as_f64) {
            Some(total) => Some(total),
            None => match (input, output) {
                (None, None) => None,
                (input, output) => Some(input.unwrap_or(0.0) + output.unwrap_or(0.0)),
            },
        };
        let mut details = Map::new();
        if let Some(thoughts) = usage.get("thoughtsTokenCount") {
            details.insert("reasoningTokens".to_string(), thoughts.clone());
        }
        if let Some(cached) = usage.get("cachedContentTokenCount") {
            details.insert("cacheReadInputTokens".to_string(), cached.clone());
        }
        details.insert(PROVIDER.to_string(), Value::Object(usage.clone()));
        token_usage(
            input.and_then(number_from_f64),
            output.and_then(number_from_f64),
            total.and_then(number_from_f64),
            Some(details),
        )
    });
    let mut google = Map::new();
    insert_optional(&mut google, "modelVersion", decoded.get("modelVersion"));
    insert_optional(&mut google, "responseId", decoded.get("responseId"));
    insert_optional(&mut google, "promptFeedback", decoded.get("promptFeedback"));
    google.insert("candidates".to_string(), Value::Array(candidate_metadata));
    let mut provider_metadata = Map::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(google));
    Ok(ImageResponse { images, usage, provider_metadata: Some(provider_metadata as ProviderMetadata) })
}

fn candidate_part(part: &Value) -> Value {
    if let Some(inline) = part.get("inlineData").and_then(Value::as_object) {
        let mut metadata = Map::new();
        metadata.insert("type".to_string(), Value::String("inlineData".to_string()));
        metadata.insert("mediaType".to_string(), inline.get("mimeType").cloned().unwrap_or(Value::Null));
        insert_optional(&mut metadata, "thought", part.get("thought"));
        insert_optional(&mut metadata, "thoughtSignature", part.get("thoughtSignature"));
        return Value::Object(metadata);
    }
    let mut metadata = Map::new();
    metadata.insert("type".to_string(), Value::String("text".to_string()));
    insert_optional(&mut metadata, "text", part.get("text"));
    insert_optional(&mut metadata, "thought", part.get("thought"));
    insert_optional(&mut metadata, "thoughtSignature", part.get("thoughtSignature"));
    Value::Object(metadata)
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
