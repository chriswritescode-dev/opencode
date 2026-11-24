use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::media::generation::Reason;
use crate::media::generation::Snapshot;
use crate::media::protocol::{Body, DecodeContext, Failure, Identity, State, TextResponse, state, status};
use crate::media::route::{PollContext, QueuedProtocol, Started};
use crate::modalities::video::{VideoRequest, VideoResponse, seconds};
use crate::protocols::shared;
use crate::protocols::utils::media_input::{media_reference, merge_body, url_asset};
use crate::schema::{AiError, MediaAsset, MediaSource, ProviderId, ProviderMetadata};

pub const ID: &str = "openrouter-video";
pub const NAME: &str = "OpenRouter Video";
pub const PATH: &str = "/videos";
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const PROVIDER: &str = "openrouter";

const STATUS: &[(&str, State)] = &[
    ("pending", State::Running),
    ("in_progress", State::Running),
    ("completed", State::Completed),
    ("failed", State::Failed),
    ("cancelled", State::Cancelled),
    ("expired", State::Expired),
];

pub struct OpenRouterVideo {
    provider: ProviderId,
}

impl OpenRouterVideo {
    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }
}

pub fn protocol() -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(OpenRouterVideo { provider: ProviderId::from(PROVIDER) })
}

#[derive(serde::Deserialize)]
struct StartResponse {
    id: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(serde::Deserialize)]
struct Job {
    #[serde(default)]
    generation_id: Option<String>,
    status: String,
    #[serde(default)]
    unsigned_urls: Option<Vec<String>>,
    #[serde(default)]
    expires_at: Option<Number>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    usage: Option<JobUsage>,
}

#[derive(serde::Deserialize)]
struct JobUsage {
    #[serde(default)]
    cost: Option<Number>,
    #[serde(default)]
    is_byok: Option<bool>,
}

impl QueuedProtocol<VideoRequest, VideoResponse, String> for OpenRouterVideo {
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
        let start: StartResponse = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, start.status.as_deref().unwrap_or("pending"));
        snapshot.id = start.id.clone();
        Ok(Started { token: start.id, snapshot })
    }

    fn status_path(&self, token: &String) -> String {
        job_path(token)
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<String>) -> Result<Snapshot, AiError> {
        let identity = self.identity();
        let job: Job = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, &job.status);
        snapshot.id = context.token.clone();
        Ok(snapshot)
    }

    fn result_path(&self, token: &String) -> String {
        job_path(token)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<String>,
    ) -> crate::route::executor::BoxFuture<Result<VideoResponse, AiError>> {
        let identity = self.identity();
        Box::pin(async move {
            let job: Job = identity.decode_json(&response)?;
            let state = state(STATUS, &job.status);
            let output = identity.output(&response);
            if state == State::Running {
                return Err(output.pending(NAME, &context.token));
            }
            let detail = job.error.as_deref().map(|error| format!(": {error}")).unwrap_or_default();
            if state == State::Failed {
                let message = format!("{NAME} generation failed{detail}");
                if job.error.as_deref().is_some_and(is_content_policy) {
                    return Err(output.content_policy(message));
                }
                return Err(output.ended(Reason::Failed, message, Failure::ProviderInternal));
            }
            if state != State::Completed {
                let reason = match state {
                    State::Cancelled => Reason::Cancelled,
                    _ => Reason::Expired,
                };
                return Err(output.ended(
                    reason,
                    format!("{NAME} job {} {}{detail}", context.token, job.status),
                    Failure::ProviderInternal,
                ));
            }
            let urls = job.unsigned_urls.clone().unwrap_or_default();
            if urls.is_empty() {
                return Err(output.invalid(format!("{NAME} completed without a video URL")));
            }
            let expires_at =
                job.expires_at.as_ref().and_then(Number::as_u64).map(|seconds| seconds.saturating_mul(1000));
            let mut videos = Vec::new();
            for url in urls {
                let credentials = (context.credentials)(url.clone()).await?;
                videos.push(url_asset(&url, Some("video/mp4"), expires_at.map(Number::from), credentials));
            }
            let mut metadata = Map::new();
            metadata.insert("id".to_string(), Value::String(context.token.clone()));
            if let Some(generation_id) = job.generation_id.as_ref() {
                metadata.insert("generationId".to_string(), Value::String(generation_id.clone()));
            }
            if let Some(cost) = job.usage.as_ref().and_then(|usage| usage.cost.clone()) {
                metadata.insert("cost".to_string(), Value::Number(cost));
            }
            if let Some(is_byok) = job.usage.as_ref().and_then(|usage| usage.is_byok) {
                metadata.insert("isByok".to_string(), Value::Bool(is_byok));
            }
            let mut provider_metadata = ProviderMetadata::new();
            provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
            Ok(VideoResponse { videos, usage: None, provider_metadata: Some(provider_metadata) })
        })
    }
}

fn job_path(token: &str) -> String {
    format!("{PATH}/{}", opencode_api_types::js::encode_component(token))
}

fn is_content_policy(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("content policy") || error.contains("moderat")
}

fn body(request: &VideoRequest, identity: &Identity) -> Result<Body, AiError> {
    let mut frames = Vec::new();
    if let Some(start) = request.start.as_ref() {
        frames.push(image_frame(start, "first_frame")?);
    }
    if let Some(end) = request.end.as_ref() {
        frames.push(image_frame(end, "last_frame")?);
    }
    let mut references = Vec::new();
    for asset in &request.references {
        references.push(reference_part(asset, identity)?);
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if !frames.is_empty() {
        base.insert("frame_images".to_string(), Value::Array(frames));
    }
    if !references.is_empty() {
        base.insert("input_references".to_string(), Value::Array(references));
    }
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        base.insert("duration".to_string(), Value::Number(duration));
    }
    if let Some(aspect) = request.aspect.as_deref().filter(|aspect| *aspect != "auto") {
        base.insert("aspect_ratio".to_string(), Value::String(aspect.to_string()));
    }
    if let Some(resolution) = request.resolution.as_deref() {
        base.insert("resolution".to_string(), Value::String(resolution_value(resolution)));
    }
    if let Some(sound) = request.sound {
        base.insert("generate_audio".to_string(), Value::Bool(sound));
    }
    Ok(Body::Json { value: merge_body(base, request.provider_options.as_ref(), request.http.as_ref()), query: None })
}

fn image_frame(asset: &MediaAsset, frame_type: &str) -> Result<Value, AiError> {
    let reference = media_reference(asset, None, NAME).map_err(shared::invalid_request)?;
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("image_url".to_string()));
    object.insert("image_url".to_string(), json!({ "url": reference.value() }));
    object.insert("frame_type".to_string(), Value::String(frame_type.to_string()));
    Ok(Value::Object(object))
}

fn reference_part(asset: &MediaAsset, identity: &Identity) -> Result<Value, AiError> {
    let kind = shared::media_kind(&shared::media_type(asset));
    if (kind == "video" || kind == "audio") && !matches!(asset.source, MediaSource::Url { .. }) {
        return Err(
            identity.unsupported("video.references", format!("{NAME} only takes reference {kind} as http(s) URLs"))
        );
    }
    let reference = media_reference(asset, None, NAME).map_err(shared::invalid_request)?;
    let key = match kind {
        "video" => "video_url",
        "audio" => "audio_url",
        _ => "image_url",
    };
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String(key.to_string()));
    object.insert(key.to_string(), json!({ "url": reference.value() }));
    Ok(Value::Object(object))
}

fn resolution_value(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_digit() && bytes[1] == b'k' {
        return value.to_uppercase();
    }
    value.to_string()
}
