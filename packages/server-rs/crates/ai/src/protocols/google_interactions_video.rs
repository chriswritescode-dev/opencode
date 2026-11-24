use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::media::generation::Snapshot;
use crate::media::protocol::{
    Body, DecodeContext, Failure, Identity, Output, State, TextResponse, failure, state, status,
};
use crate::media::route::{Cancel, PollContext, QueuedProtocol, Started};
use crate::modalities::video::{VideoRequest, VideoResponse, seconds, token_usage};
use crate::protocols::google_interactions::{self, media_content};
use crate::protocols::utils::gemini_generate_content;
use crate::protocols::utils::media_input::{decoded_asset, merge_body};
use crate::schema::{AiError, MediaAsset, MediaSource, ProviderId, ProviderMetadata};

pub const ID: &str = "google-interactions-video";
pub const NAME: &str = "Google Interactions Video";
pub const PATH: &str = "/interactions";
pub const DEFAULT_BASE_URL: &str = google_interactions::DEFAULT_BASE_URL;
const PROVIDER: &str = "google";

const STATUS: &[(&str, State)] = &[
    ("in_progress", State::Running),
    ("running", State::Running),
    ("completed", State::Completed),
    ("incomplete", State::Completed),
    ("failed", State::Failed),
    ("cancelled", State::Cancelled),
];

const FAILURE: &[(&str, Failure)] = &[
    ("INVALID_ARGUMENT", Failure::InvalidRequest),
    ("FAILED_PRECONDITION", Failure::InvalidRequest),
    ("OUT_OF_RANGE", Failure::InvalidRequest),
    ("PERMISSION_DENIED", Failure::Authentication),
    ("UNAUTHENTICATED", Failure::Authentication),
    ("RESOURCE_EXHAUSTED", Failure::RateLimit),
];

pub struct GoogleInteractionsVideo {
    provider: ProviderId,
}

impl GoogleInteractionsVideo {
    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }
}

pub fn protocol() -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(GoogleInteractionsVideo { provider: ProviderId::from(PROVIDER) })
}

#[derive(serde::Deserialize)]
struct Interaction {
    id: String,
    status: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    steps: Option<Vec<Step>>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
    #[serde(default)]
    errors: Option<Vec<InteractionError>>,
}

#[derive(serde::Deserialize)]
struct Step {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Option<Vec<Content>>,
}

#[derive(serde::Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
}

#[derive(serde::Deserialize)]
struct InteractionError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl QueuedProtocol<VideoRequest, VideoResponse, String> for GoogleInteractionsVideo {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn token_decode(&self, value: Value) -> Result<String, AiError> {
        match value {
            Value::String(value) => Ok(value),
            _ => Err(self.identity().invalid(format!("{NAME} returned an invalid token"), None, None)),
        }
    }

    fn token_encode(&self, token: &String) -> Value {
        Value::String(token.clone())
    }

    fn body_from(&self, request: &VideoRequest) -> Result<Body, AiError> {
        body(request, &self.identity())
    }

    fn start_decode(
        &self,
        response: &TextResponse,
        _context: &DecodeContext<VideoRequest>,
    ) -> Result<Started<String>, AiError> {
        let identity = self.identity();
        let interaction: Interaction = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, &interaction.status);
        snapshot.id = interaction.id.clone();
        Ok(Started { token: interaction.id, snapshot })
    }

    fn status_path(&self, token: &String) -> String {
        interaction_path(token)
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<String>) -> Result<Snapshot, AiError> {
        let identity = self.identity();
        let interaction: Interaction = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, &interaction.status);
        snapshot.id = context.token.clone();
        Ok(snapshot)
    }

    fn result_path(&self, token: &String) -> String {
        interaction_path(token)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<String>,
    ) -> crate::route::executor::BoxFuture<Result<VideoResponse, AiError>> {
        let identity = self.identity();
        let provider = self.provider.clone();
        Box::pin(async move { decode_result(&identity, &provider, &response, &context) })
    }

    fn cancel(&self) -> Option<Cancel<String>> {
        Some(Cancel {
            method: "POST".to_string(),
            path: Arc::new(|token: &String| format!("{}/cancel", interaction_path(token))),
        })
    }
}

fn decode_result(
    identity: &Identity,
    provider: &ProviderId,
    response: &TextResponse,
    context: &PollContext<String>,
) -> Result<VideoResponse, AiError> {
    let interaction: Interaction = identity.decode_json(response)?;
    let state = state(STATUS, &interaction.status);
    if state == State::Running {
        return Err(identity.output(response).pending(NAME, &context.token));
    }
    let errors = interaction.errors.as_deref().unwrap_or_default();
    let codes = errors.iter().filter_map(|error| error_code(error.code.as_deref())).collect::<Vec<_>>();
    let messages = errors
        .iter()
        .filter_map(|error| error.message.as_deref())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if state == State::Cancelled {
        return Err(identity.output(response).ended(
            crate::media::generation::Reason::Cancelled,
            format!("{NAME} interaction {} was cancelled", context.token),
            Failure::ProviderInternal,
        ));
    }
    if state == State::Failed {
        return Err(failed(&identity.output(response), &codes, &messages));
    }
    let mut videos = Vec::new();
    for step in interaction.steps.iter().flatten().filter(|step| step.kind == "model_output") {
        for content in step.content.iter().flatten().filter(|content| content.kind == "video") {
            let media_type = content.mime_type.as_deref();
            if let Some(data) = content.data.as_deref() {
                videos.push(decoded_asset_labeled(&identity.output(response), videos.len(), data, media_type)?);
                continue;
            }
            if let Some(uri) = content.uri.as_deref() {
                videos.push(MediaAsset {
                    source: MediaSource::Ref {
                        provider: provider.clone(),
                        id: uri.to_string(),
                        media_type: media_type.map(str::to_string),
                    },
                    info: None,
                    provider_metadata: None,
                    headers: None,
                });
                continue;
            }
            return Err(identity.output(response).invalid(format!("{NAME} returned a video without data or a URI")));
        }
    }
    if videos.is_empty() {
        if has_content_block(&codes) {
            return Err(identity
                .output(response)
                .content_policy(format!("{NAME} withheld the video{}", suffix(&messages))));
        }
        return Err(identity.output(response).invalid(format!("{NAME} {} without a video", interaction.status)));
    }
    let usage = interaction.usage.as_ref().map(|usage| {
        let mut details = Map::new();
        if let Some(reasoning) = usage.get("total_thought_tokens").and_then(Value::as_number) {
            details.insert("reasoningTokens".to_string(), Value::Number(reasoning.clone()));
        }
        details.insert("google".to_string(), Value::Object(usage.clone()));
        token_usage(
            usage.get("total_input_tokens").and_then(Value::as_number).cloned(),
            usage.get("total_output_tokens").and_then(Value::as_number).cloned(),
            usage.get("total_tokens").and_then(Value::as_number).cloned(),
            Some(details),
        )
    });
    let mut metadata = Map::new();
    metadata.insert("interactionId".to_string(), Value::String(interaction.id.clone()));
    if let Some(model) = interaction.model.as_ref() {
        metadata.insert("model".to_string(), Value::String(model.clone()));
    }
    metadata.insert("status".to_string(), Value::String(interaction.status.clone()));
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(VideoResponse { videos, usage: usage.flatten(), provider_metadata: Some(provider_metadata) })
}

fn interaction_path(token: &str) -> String {
    format!("{PATH}/{}", opencode_api_types::js::encode_component(token))
}

fn body(request: &VideoRequest, identity: &Identity) -> Result<Body, AiError> {
    if request.end.is_some() && request.start.is_none() {
        return Err(
            identity.unsupported("video.end", format!("{NAME} reads a lone image as the start frame; send start too"))
        );
    }
    let mut media = Vec::new();
    for asset in request.start.iter().chain(request.end.iter()).chain(request.references.iter()) {
        media.push(media_content(asset)?);
    }
    let mut input = media;
    input.push(json!({ "type": "text", "text": request.prompt }));

    let mut response_format = Map::new();
    let mut any_format = false;
    if let Some(aspect) = request.aspect.as_deref().filter(|aspect| *aspect != "auto") {
        response_format.insert("aspect_ratio".to_string(), Value::String(aspect.to_string()));
        any_format = true;
    }
    if let Some(resolution) = request.resolution.as_deref() {
        response_format.insert("resolution".to_string(), Value::String(resolution.to_lowercase()));
        any_format = true;
    }
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        response_format.insert("duration".to_string(), Value::String(format!("{duration}s")));
        any_format = true;
    }

    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("input".to_string(), Value::Array(input));
    base.insert("background".to_string(), Value::Bool(true));
    if any_format {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("video".to_string()));
        for (key, value) in response_format {
            object.insert(key, value);
        }
        base.insert("response_format".to_string(), Value::Object(object));
    }
    if let Some(task) = task(request) {
        let mut video_config = Map::new();
        video_config.insert("task".to_string(), Value::String(task));
        let mut generation_config = Map::new();
        generation_config.insert("video_config".to_string(), Value::Object(video_config));
        base.insert("generation_config".to_string(), Value::Object(generation_config));
    }
    Ok(Body::Json { value: merge_body(base, request.provider_options.as_ref(), request.http.as_ref()), query: None })
}

fn task(request: &VideoRequest) -> Option<String> {
    let frames = request.start.is_some() || request.end.is_some();
    let references = !request.references.is_empty();
    if frames && !references {
        return Some("image_to_video".to_string());
    }
    if references && !frames {
        return Some("reference_to_video".to_string());
    }
    None
}

fn failed(output: &Output, codes: &[String], messages: &[String]) -> AiError {
    let message = format!(
        "{NAME} generation failed{}{}",
        if codes.is_empty() { String::new() } else { format!(" ({})", codes.join(", ")) },
        suffix(messages)
    );
    if has_content_block(codes) {
        return output.content_policy(message);
    }
    let category = codes.first().map(|code| Value::String(code.clone()));
    output.ended(crate::media::generation::Reason::Failed, message, failure(FAILURE, category.as_ref()))
}

fn has_content_block(codes: &[String]) -> bool {
    codes.iter().any(|code| code == "CONTENT_BLOCKED" || gemini_generate_content::content_filtered(Some(code)))
}

fn suffix(messages: &[String]) -> String {
    if messages.is_empty() { String::new() } else { format!(": {}", messages.join("\n")) }
}

fn error_code(code: Option<&str>) -> Option<String> {
    let code = code?.split('/').next_back()?.trim();
    if code.is_empty() {
        return None;
    }
    Some(code.to_uppercase().replace('-', "_"))
}

fn decoded_asset_labeled(
    output: &Output,
    index: usize,
    data: &str,
    media_type: Option<&str>,
) -> Result<MediaAsset, AiError> {
    decoded_asset(&format!("{NAME} video {index}"), data, media_type, None, None)
        .map_err(|message| output.invalid(message))
}
