use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use serde_json::{Map, Value};

use crate::media::multipart::{AppendFieldsOptions, Form, append_fields};
use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::image::{ImageEvent, ImageRequest};
use crate::protocols::shared::{invalid_request, media_type};
use crate::protocols::utils::image_sizing::{Limits, Size, fit, is_requested, nearest, shape};
use crate::protocols::utils::media_input::{
    decoded_asset, image_output, inline_bytes, media_info, media_reference, merge_body,
};
use crate::route::executor::ByteStream;
use crate::schema::{AiError, ProviderId, ProviderMetadata};

pub const ID: &str = "openai-images";
pub const NAME: &str = "OpenAI Images";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/images/generations";
pub const EDIT_PATH: &str = "/images/edits";

const RESERVED_FORM_FIELDS: [&str; 6] = ["model", "prompt", "image", "image[]", "images", "mask"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageInput {
    Edits,
    JsonEdits,
    References,
}

pub fn path(image_input: ImageInput, request: &ImageRequest) -> String {
    if image_input != ImageInput::References && !request.images.is_empty() {
        return EDIT_PATH.to_string();
    }
    PATH.to_string()
}

pub type Sizing = Arc<dyn Fn(&ImageRequest) -> Option<Map<String, Value>> + Send + Sync>;

pub struct OpenAiImages {
    pub provider: ProviderId,
    pub sizing: Sizing,
    pub image_input: ImageInput,
}

impl OpenAiImages {
    pub fn new(provider: impl Into<ProviderId>, sizing: Sizing, image_input: ImageInput) -> Self {
        Self { provider: provider.into(), sizing, image_input }
    }

    pub fn default_protocol() -> Self {
        Self::new("openai", Arc::new(pixel_size), ImageInput::Edits)
    }

    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }

    fn build_body(&self, request: &ImageRequest, mode: Mode) -> Result<Body, AiError> {
        let images = &request.images;
        let mask = provider_mask(request)?;
        if self.image_input == ImageInput::References && mask.is_some() {
            return Err(self
                .identity()
                .unsupported("media.mask", format!("{} image edits take no mask", self.provider)));
        }
        if mask.is_some() && images.is_empty() {
            return Err(invalid_request("An OpenAI image mask requires at least one input image"));
        }
        if request.provider_options.as_ref().and_then(|options| options.get("size")).is_some()
            && is_requested(&shape(request))
        {
            return Err(invalid_request(
                "OpenAI providerOptions.size replaces aspect and resolution; set one or the other",
            ));
        }
        let mut fields = Map::new();
        if let Some(n) = request.n {
            fields.insert("n".to_string(), Value::from(n));
        }
        if let Some(sizing) = (self.sizing)(request) {
            for (key, value) in sizing {
                fields.insert(key, value);
            }
        }
        if mode == Mode::Stream {
            if let Some(n) = request.n.filter(|n| *n > 1) {
                return Err(self
                    .identity()
                    .unsupported("media.n", format!("{NAME} streams one image; use Image.generate for n={n}")));
            }
            fields.insert("stream".to_string(), Value::Bool(true));
            let partial = request
                .provider_options
                .as_ref()
                .and_then(|options| options.get("partialImages"))
                .cloned()
                .unwrap_or_else(|| Value::from(2));
            fields.insert("partial_images".to_string(), partial);
        }
        let native = native_options(request.provider_options.as_ref());
        let body = merge_body(fields, Some(&native), request.http.as_ref());

        if self.image_input == ImageInput::References {
            let references = images
                .iter()
                .map(|image| {
                    media_reference(image, None, NAME)
                        .map(|value| json_entry("type", Value::String("image_url".to_string()), value.value()))
                        .map_err(invalid_request)
                })
                .collect::<Result<Vec<Value>, AiError>>()?;
            let mut base = Map::new();
            base.insert("model".to_string(), Value::String(request.model.id.clone()));
            base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
            if !references.is_empty() {
                base.insert("input_references".to_string(), Value::Array(references));
            }
            return Ok(Body::Json { value: merged(&[&base, &body]), query: None });
        }

        let inline = images.iter().all(is_inline);
        let mask_inline = mask.as_ref().is_none_or(is_inline);
        if self.image_input == ImageInput::Edits && !images.is_empty() && inline && mask_inline {
            let mut form = Form::new();
            let base = map([
                ("model", Value::String(request.model.id.clone())),
                ("prompt", Value::String(request.prompt.clone())),
            ]);
            append_fields(
                &mut form,
                &base,
                AppendFieldsOptions { overlay: Some(&body), reserved: &RESERVED_FORM_FIELDS, repeat_arrays: None },
            );
            for (index, image) in images.iter().enumerate() {
                let bytes = inline_bytes(ID, image).map_err(invalid_request)?;
                form.append_file("image[]", bytes, Some(format!("image-{index}")), Some(media_type(image)));
            }
            if let Some(mask) = &mask {
                let bytes = inline_bytes(ID, mask).map_err(invalid_request)?;
                form.append_file("mask", bytes, Some("mask".to_string()), Some(media_type(mask)));
            }
            return Ok(Body::Multipart(form));
        }

        let references = images
            .iter()
            .map(|image| {
                media_reference(image, Some(&self.provider), NAME)
                    .map(|reference| match reference {
                        crate::protocols::utils::media_input::MediaReference::Ref(value) => {
                            map([("file_id", Value::String(value))])
                        }
                        other => map([("image_url", Value::String(other.value()))]),
                    })
                    .map_err(invalid_request)
            })
            .collect::<Result<Vec<Map<String, Value>>, AiError>>()?;
        let mask_reference = mask.as_ref().map(|mask| {
            media_reference(mask, Some(&self.provider), NAME)
                .map(|reference| match reference {
                    crate::protocols::utils::media_input::MediaReference::Ref(value) => {
                        map([("file_id", Value::String(value))])
                    }
                    other => map([("image_url", Value::String(other.value()))]),
                })
                .map_err(invalid_request)
        });
        let mut base = Map::new();
        base.insert("model".to_string(), Value::String(request.model.id.clone()));
        base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
        if !references.is_empty() {
            base.insert("images".to_string(), Value::Array(references.into_iter().map(Value::Object).collect()));
        }
        if let Some(mask_reference) = mask_reference {
            base.insert("mask".to_string(), Value::Object(mask_reference?));
        }
        Ok(Body::Json { value: merged(&[&base, &body]), query: None })
    }
}

pub enum Frame {
    Event(String),
    Document(String),
}

#[derive(Clone, Default)]
pub struct State {
    completed: usize,
    format: Option<String>,
    size: Option<String>,
    quality: Option<String>,
    background: Option<String>,
    usage: Option<Map<String, Value>>,
}

impl StreamedProtocol<ImageRequest, ImageEvent, Frame, State> for OpenAiImages {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn body_from(&self, request: &ImageRequest, mode: Mode) -> Result<Body, AiError> {
        self.build_body(request, mode)
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<ImageRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<Frame, AiError>> + Send>> {
        use futures_util::StreamExt;
        if context.mode == Mode::Stream {
            Box::pin(crate::route::framing::sse_frames(bytes, None, false).map(|item| item.map(Frame::Event)))
        } else {
            Box::pin(crate::route::framing::document_frames(bytes).map(|item| item.map(Frame::Document)))
        }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: Frame) -> Result<(State, Vec<ImageEvent>), AiError> {
        match frame {
            Frame::Event(frame) => self.on_event(state, &frame),
            Frame::Document(document) => self.on_document(&document),
        }
    }

    fn finish(&self, state: &State, context: &ResponseContext<ImageRequest>) -> Result<Vec<ImageEvent>, AiError> {
        let identity = self.identity();
        if state.completed == 0 {
            return Err(identity.incomplete());
        }
        let provider = context.request.model.provider.clone();
        let usage = state.usage.as_ref().and_then(|usage| {
            let mut details = Map::new();
            details.insert(provider.clone(), Value::Object(usage.clone()));
            crate::modalities::image::token_usage(
                usage.get("input_tokens").and_then(Value::as_number).cloned(),
                usage.get("output_tokens").and_then(Value::as_number).cloned(),
                usage.get("total_tokens").and_then(Value::as_number).cloned(),
                Some(details),
            )
        });
        let mut metadata = Map::new();
        insert_optional(&mut metadata, "outputFormat", state.format.clone().map(Value::String));
        insert_optional(&mut metadata, "size", state.size.clone().map(Value::String));
        insert_optional(&mut metadata, "quality", state.quality.clone().map(Value::String));
        insert_optional(&mut metadata, "background", state.background.clone().map(Value::String));
        let mut provider_metadata = Map::new();
        provider_metadata.insert(provider, Value::Object(metadata));
        Ok(vec![ImageEvent::Finish { usage, provider_metadata: Some(provider_metadata as ProviderMetadata) }])
    }
}

impl OpenAiImages {
    fn on_event(&self, state: State, frame: &str) -> Result<(State, Vec<ImageEvent>), AiError> {
        let identity = self.identity();
        let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
        let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
        let Some(object) = value.as_object() else {
            return Err(invalid());
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            return Err(invalid());
        };
        if !EVENT_TYPES.contains(&kind) {
            return Ok((state, Vec::new()));
        }
        if kind == "error" {
            let message = object
                .get("error")
                .and_then(Value::as_object)
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(AiError {
                reason: crate::provider_error::classify_provider_failure(crate::provider_error::ProviderFailure {
                    message: format!("{NAME} stream failed: {message}"),
                    raw_body: Some(frame.to_string()),
                    ..Default::default()
                }),
            });
        }
        let data = object.get("b64_json").and_then(Value::as_str).ok_or_else(invalid)?;
        let format = object.get("output_format").and_then(Value::as_str).ok_or_else(invalid)?;
        let size = object.get("size").and_then(Value::as_str);
        if kind.ends_with(".partial_image") {
            let index = object.get("partial_image_index").and_then(Value::as_i64).ok_or_else(invalid)?;
            let info = media_info(Some(format), size);
            let image = decoded_asset(
                &format!("{NAME} partial image"),
                data,
                Some(&format!("image/{format}")),
                Some(info),
                None,
            )
            .map_err(|message| identity.frame_error(message, Some(frame.to_string())))?;
            return Ok((state, vec![ImageEvent::ImagePartial { index, image }]));
        }
        let info = media_info(Some(format), size);
        let image = decoded_asset(
            &format!("{NAME} result {}", state.completed),
            data,
            Some(&format!("image/{format}")),
            Some(info),
            None,
        )
        .map_err(|message| identity.frame_error(message, Some(frame.to_string())))?;
        let index = state.completed as i64;
        let usage = usage_details(object.get("usage"));
        let next = State {
            completed: state.completed + 1,
            format: Some(format.to_string()),
            size: size.map(str::to_string),
            quality: object.get("quality").and_then(Value::as_str).map(str::to_string),
            background: object.get("background").and_then(Value::as_str).map(str::to_string),
            usage,
        };
        Ok((next, vec![ImageEvent::Image { index, image }]))
    }

    fn on_document(&self, document: &str) -> Result<(State, Vec<ImageEvent>), AiError> {
        let identity = self.identity();
        let value: Value = serde_json::from_str(document).map_err(|_| {
            identity.frame_error(format!("{NAME} returned an invalid response"), Some(document.to_string()))
        })?;
        let object = value.as_object().cloned().unwrap_or_default();
        let data = object.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        let format = object.get("output_format").and_then(Value::as_str);
        let size = object.get("size").and_then(Value::as_str);
        let media_type = format.map(|format| format!("image/{format}"));
        let invalid = |message: String| identity.frame_error(message, Some(document.to_string()));
        let info = media_info(format, size);
        let mut images = Vec::with_capacity(data.len());
        for (index, item) in data.iter().enumerate() {
            let item = item.as_object().cloned().unwrap_or_default();
            let image = image_output(
                &format!("{NAME} result {index}"),
                item.get("b64_json").and_then(Value::as_str),
                item.get("url").and_then(Value::as_str),
                media_type.as_deref(),
                Some(info.clone()),
            )
            .map_err(invalid)?;
            images.push(image);
        }
        if images.is_empty() {
            return Err(invalid(format!("{NAME} returned no images")));
        }
        let usage = usage_details(object.get("usage"));
        let state = State {
            completed: images.len(),
            format: format.map(str::to_string),
            size: size.map(str::to_string),
            quality: object.get("quality").and_then(Value::as_str).map(str::to_string),
            background: object.get("background").and_then(Value::as_str).map(str::to_string),
            usage,
        };
        let events = images
            .into_iter()
            .enumerate()
            .map(|(index, image)| ImageEvent::Image { index: index as i64, image })
            .collect();
        Ok((state, events))
    }
}

pub fn pixel_size(request: &ImageRequest) -> Option<Map<String, Value>> {
    if !is_requested(&shape(request)) {
        return None;
    }
    let size = match sizes(&request.model.id) {
        Sizes::Limits(limits) => fit(&shape(request), &limits),
        Sizes::List(sizes) => nearest(&shape(request), &sizes),
    };
    Some(map([("size", Value::String(format!("{}x{}", size.width, size.height)))]))
}

enum Sizes {
    Limits(Limits),
    List(Vec<Size>),
}

fn sizes(model: &str) -> Sizes {
    let id = model.rsplit('/').next().unwrap_or(model);
    if id.starts_with("dall-e-2") {
        return Sizes::List(vec![square(256), square(512), square(1024)]);
    }
    if id.starts_with("dall-e-3") {
        return Sizes::List(vec![
            Size { width: 1024, height: 1024 },
            Size { width: 1792, height: 1024 },
            Size { width: 1024, height: 1792 },
        ]);
    }
    if id.starts_with("gpt-image-1") || id.starts_with("chatgpt-image") {
        return Sizes::List(vec![
            Size { width: 1024, height: 1024 },
            Size { width: 1536, height: 1024 },
            Size { width: 1024, height: 1536 },
        ]);
    }
    if id.starts_with("gpt-image-") {
        return Sizes::Limits(Limits {
            multiple: 16,
            max_edge: Some(3840),
            min_pixels: Some(655_360),
            max_pixels: Some(8_294_400),
            max_ratio: Some(3.0),
            ..Limits::default()
        });
    }
    Sizes::Limits(Limits { multiple: 16, ..Limits::default() })
}

fn square(edge: i64) -> Size {
    Size { width: edge, height: edge }
}

fn provider_mask(request: &ImageRequest) -> Result<Option<crate::schema::MediaAsset>, AiError> {
    let Some(value) = request.provider_options.as_ref().and_then(|options| options.get("mask")) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| invalid_request("OpenAI providerOptions.mask is not a media asset"))
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else {
        return Map::new();
    };
    let mut native = Map::new();
    if let Some(value) = options.get("outputCompression") {
        native.insert("output_compression".to_string(), value.clone());
    }
    if let Some(value) = options.get("outputFormat") {
        native.insert("output_format".to_string(), value.clone());
    }
    for (key, value) in options {
        if matches!(key.as_str(), "outputCompression" | "outputFormat" | "partialImages" | "mask") {
            continue;
        }
        native.insert(key.clone(), value.clone());
    }
    if options.get("response_format").is_none_or(Value::is_null) {
        native.remove("response_format");
    }
    native
}

fn merged(items: &[&Map<String, Value>]) -> Map<String, Value> {
    crate::json::merge_records(items.iter().copied()).unwrap_or_default()
}

fn map<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries.into_iter().map(|(key, value)| (key.to_string(), value)).collect()
}

fn usage_details(value: Option<&Value>) -> Option<Map<String, Value>> {
    let usage = value?.as_object()?;
    let mut filtered = Map::new();
    for key in ["input_tokens", "output_tokens", "total_tokens", "input_tokens_details", "output_tokens_details"] {
        if let Some(value) = usage.get(key) {
            filtered.insert(key.to_string(), value.clone());
        }
    }
    Some(filtered)
}

fn is_inline(asset: &crate::schema::MediaAsset) -> bool {
    matches!(asset.source, crate::schema::MediaSource::Bytes { .. } | crate::schema::MediaSource::Base64 { .. })
}

fn insert_optional(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}

fn json_entry(kind_key: &str, kind: Value, value: String) -> Value {
    let mut entry = Map::new();
    entry.insert(kind_key.to_string(), kind);
    entry.insert("image_url".to_string(), Value::Object(map([("url", Value::String(value))])));
    Value::Object(entry)
}

const EVENT_TYPES: [&str; 5] = [
    "image_generation.partial_image",
    "image_edit.partial_image",
    "image_generation.completed",
    "image_edit.completed",
    "error",
];
