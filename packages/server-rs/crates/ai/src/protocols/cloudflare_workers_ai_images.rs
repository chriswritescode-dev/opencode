use std::sync::Arc;

use serde_json::{Map, Value};

use crate::media::multipart::{AppendFieldsOptions, Form, append_fields};
use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::image::{ImageRequest, ImageResponse};
use crate::protocols::openai_images::shape;
use crate::protocols::shared::{invalid_request, media_type, require_inline_media};
use crate::protocols::utils::image_sizing::{Limits, exact};
use crate::protocols::utils::media_input::{bytes_asset, decoded_asset};
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "cloudflare-workers-ai-images";
pub const NAME: &str = "Cloudflare Workers AI";
const PROVIDER: &str = "cloudflare-workers-ai";
const MULTIPART_MODEL_PREFIX: &str = "@cf/black-forest-labs/flux-2-";
const RESERVED_FORM_FIELDS: [&str; 1] = ["prompt"];

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
    if request.n.is_some_and(|n| n > 1) {
        return Err(
            identity.unsupported("media.n", format!("{NAME} generates one image per request; call it once per image"))
        );
    }
    let mut fields = Map::new();
    fields.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if let Some(size) = exact(
        &identity,
        &shape(request),
        &Limits { multiple: 16, min_edge: Some(256), max_edge: Some(2048), ..Limits::default() },
    )? {
        fields.insert("width".to_string(), Value::from(size.width));
        fields.insert("height".to_string(), Value::from(size.height));
    }
    let provider_options = request.provider_options.clone().unwrap_or_default();
    let http = http_body(request);
    let native = crate::json::merge_records([&provider_options, &http]).unwrap_or_default();
    let images = &request.images;
    if !request.model.id.starts_with(MULTIPART_MODEL_PREFIX) {
        if images.len() > 1 {
            return Err(identity
                .unsupported("media.images", format!("{} takes one input image as image_b64", request.model.id)));
        }
        let image = match images.first() {
            Some(image) => Some(require_inline_media(ID, image)?.base64),
            None => None,
        };
        let mut base = fields.clone();
        if let Some(image) = image {
            base.insert("image_b64".to_string(), Value::String(image));
        }
        let body = crate::json::merge_records([&base, &native]).unwrap_or(base);
        return Ok(Body::Json { value: body, query: None });
    }
    let mut form = Form::new();
    append_fields(
        &mut form,
        &fields,
        AppendFieldsOptions { overlay: Some(&native), reserved: &RESERVED_FORM_FIELDS, repeat_arrays: None },
    );
    for (index, image) in images.iter().enumerate() {
        let bytes = crate::protocols::utils::media_input::inline_bytes(ID, image).map_err(invalid_request)?;
        form.append_file(
            format!("input_image_{index}"),
            bytes,
            Some(format!("image_{index}")),
            Some(media_type(image)),
        );
    }
    Ok(Body::Multipart(form))
}

fn decode(text: &TextResponse, _context: &DecodeContext<ImageRequest>) -> Result<ImageResponse, AiError> {
    let identity = identity();
    let content_type = text
        .http
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value)
        .unwrap_or(&String::new())
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return Ok(ImageResponse { images: vec![bytes_asset(&text.bytes)], usage: None, provider_metadata: None });
    }
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let value: Value =
        serde_json::from_str(&text.body).map_err(|_| invalid(format!("{NAME} returned an invalid response")))?;
    let result = value
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("{NAME} returned an invalid response")))?;
    let image =
        result.get("image").and_then(Value::as_str).ok_or_else(|| invalid(format!("{NAME} returned no image")))?;
    let image = decoded_asset(&format!("{NAME} result"), image, None, None, None).map_err(invalid)?;
    let provider_metadata = result.get("usage").and_then(Value::as_object).map(|usage| {
        let mut metadata = Map::new();
        metadata.insert("usage".to_string(), Value::Object(usage.clone()));
        let mut providers = Map::new();
        providers.insert(PROVIDER.to_string(), Value::Object(metadata));
        providers as ProviderMetadata
    });
    Ok(ImageResponse { images: vec![image], usage: None, provider_metadata })
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn http_body(request: &ImageRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}
