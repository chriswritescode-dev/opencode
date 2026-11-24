use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::media::generation::{Reason, Snapshot, Status};
use crate::media::protocol::{Body, DecodeContext, Failure, Identity, State, TextResponse, failure, state, status};
use crate::media::route::{Cancel, PollContext, QueuedProtocol, Started};
use crate::modalities::video::{MediaUsage, VideoRequest, VideoResponse, seconds};
use crate::protocols::shared;
use crate::protocols::utils::media_input::{MediaReference, expiring_url, media_reference, merge_body, url_asset};
use crate::schema::{AiError, MediaAsset, MediaInfo, MediaSource, ProviderId, ProviderMetadata};

pub const ID: &str = "seedance-video";
pub const NAME: &str = "Seedance Video";
pub const PATH: &str = "/contents/generations/tasks";
pub const VOLCENGINE_BASE_URL: &str = "https://ark.cn-beijing.volces.com/api/v3";
pub const BYTEPLUS_BASE_URL: &str = "https://ark.ap-southeast.bytepluses.com/api/v3";
const OUTPUT_RETENTION_MS: u64 = 86_400_000;

const STATUS: &[(&str, State)] = &[
    ("queued", State::Running),
    ("running", State::Running),
    ("succeeded", State::Completed),
    ("failed", State::Failed),
    ("cancelled", State::Cancelled),
    ("expired", State::Expired),
];

const FAILURE: &[(&str, Failure)] = &[
    ("InvalidParameter", Failure::InvalidRequest),
    ("MissingParameter", Failure::InvalidRequest),
    ("InvalidArgumentError", Failure::InvalidRequest),
    ("InvalidImageURL", Failure::InvalidRequest),
    ("AuthenticationError", Failure::Authentication),
    ("AccessDenied", Failure::Authentication),
    ("QuotaExceeded", Failure::RateLimit),
    ("RateLimitExceeded", Failure::RateLimit),
];

pub fn base_url(provider: &str) -> &'static str {
    if provider == "volcengine" { VOLCENGINE_BASE_URL } else { BYTEPLUS_BASE_URL }
}

pub struct SeedanceVideo {
    provider: ProviderId,
}

impl SeedanceVideo {
    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }
}

pub fn protocol(provider: &str) -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(SeedanceVideo { provider: ProviderId::from(provider) })
}

#[derive(serde::Deserialize)]
struct StartResponse {
    id: String,
}

#[derive(serde::Deserialize)]
struct Task {
    #[serde(default)]
    model: Option<String>,
    status: String,
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    error: Option<TaskError>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    updated_at: Option<Number>,
    #[serde(default)]
    seed: Option<Number>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    ratio: Option<String>,
    #[serde(default)]
    duration: Option<Number>,
    #[serde(default)]
    frames: Option<Number>,
    #[serde(default)]
    framespersecond: Option<Number>,
    #[serde(default)]
    generate_audio: Option<bool>,
    #[serde(default)]
    output_format: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    service_tier: Option<String>,
}

#[derive(serde::Deserialize)]
struct Content {
    #[serde(default)]
    video_url: Option<String>,
    #[serde(default)]
    last_frame_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct TaskError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(serde::Deserialize)]
struct Usage {
    #[serde(default)]
    completion_tokens: Option<Number>,
    #[serde(default)]
    total_tokens: Option<Number>,
}

impl QueuedProtocol<VideoRequest, VideoResponse, String> for SeedanceVideo {
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
        body(request, &self.provider)
    }

    fn start_decode(
        &self,
        response: &TextResponse,
        _context: &DecodeContext<VideoRequest>,
    ) -> Result<Started<String>, AiError> {
        let identity = self.identity();
        let start: StartResponse = identity.decode_json(response)?;
        Ok(Started {
            snapshot: Snapshot { id: start.id.clone(), status: Status::Running, reason: None, progress: None },
            token: start.id,
        })
    }

    fn status_path(&self, token: &String) -> String {
        task_path(token)
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<String>) -> Result<Snapshot, AiError> {
        let identity = self.identity();
        let task: Task = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, &task.status);
        snapshot.id = context.token.clone();
        Ok(snapshot)
    }

    fn result_path(&self, token: &String) -> String {
        task_path(token)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<String>,
    ) -> crate::route::executor::BoxFuture<Result<VideoResponse, AiError>> {
        let identity = self.identity();
        let provider = self.provider.clone();
        Box::pin(async move {
            let task: Task = identity.decode_json(&response)?;
            let state = state(STATUS, &task.status);
            let output = identity.output(&response);
            if state == State::Running {
                return Err(output.pending(NAME, &context.token));
            }
            if state == State::Failed {
                let code = task.error.as_ref().and_then(|error| error.code.as_deref());
                let detail = task.error.as_ref().and_then(|error| error.message.as_deref());
                let message = format!(
                    "{NAME} generation failed{}{}",
                    code.map(|code| format!(" ({code})")).unwrap_or_default(),
                    detail.map(|detail| format!(": {detail}")).unwrap_or_default()
                );
                if code.is_some_and(|code| code.contains("SensitiveContentDetected")) {
                    return Err(output.content_policy(message));
                }
                let category = code.and_then(|code| code.split('.').next()).map(|code| Value::String(code.to_string()));
                return Err(output.ended(Reason::Failed, message, failure(FAILURE, category.as_ref())));
            }
            if state == State::Cancelled {
                return Err(output.ended(
                    Reason::Cancelled,
                    format!("{NAME} task {} was cancelled", context.token),
                    Failure::ProviderInternal,
                ));
            }
            if state == State::Expired {
                return Err(output.ended(
                    Reason::Expired,
                    format!("{NAME} task {} expired", context.token),
                    Failure::ProviderInternal,
                ));
            }
            let video_url = task.content.as_ref().and_then(|content| content.video_url.as_deref());
            let Some(video_url) = video_url else {
                return Err(output.invalid(format!("{NAME} completed without a video URL")));
            };
            let duration = duration_seconds(&task);
            let media_type = if task.output_format.as_deref() == Some("mov") { "video/quicktime" } else { "video/mp4" };
            let mut video: MediaAsset = match task.updated_at.as_ref().and_then(Number::as_u64) {
                Some(updated_at) => url_asset(
                    video_url,
                    Some(media_type),
                    Some(Number::from(updated_at.saturating_mul(1000).saturating_add(OUTPUT_RETENTION_MS))),
                    None,
                ),
                None => {
                    let mut video = expiring_url(video_url, OUTPUT_RETENTION_MS, context.clock.now_ms());
                    if let MediaSource::Url { media_type: slot, .. } = &mut video.source {
                        *slot = Some(media_type.to_string());
                    }
                    video
                }
            };
            video.info =
                duration.map(|duration| MediaInfo { duration_seconds: Some(duration), ..MediaInfo::default() });
            let usage = task.usage.as_ref().and_then(|usage| usage.completion_tokens.clone()).map(|completion| {
                MediaUsage::Tokens {
                    input: None,
                    output: Some(completion),
                    total: task.usage.as_ref().and_then(|usage| usage.total_tokens.clone()),
                    details: None,
                }
            });
            let mut metadata = Map::new();
            metadata.insert("taskId".to_string(), Value::String(context.token.clone()));
            insert_string(&mut metadata, "model", task.model.as_deref());
            insert_number(&mut metadata, "seed", task.seed.as_ref());
            insert_string(&mut metadata, "ratio", task.ratio.as_deref());
            insert_string(&mut metadata, "resolution", task.resolution.as_deref());
            insert_number(&mut metadata, "duration", task.duration.as_ref());
            insert_number(&mut metadata, "frames", task.frames.as_ref());
            insert_number(&mut metadata, "framesPerSecond", task.framespersecond.as_ref());
            if let Some(generate_audio) = task.generate_audio {
                metadata.insert("generateAudio".to_string(), Value::Bool(generate_audio));
            }
            insert_string(&mut metadata, "outputFormat", task.output_format.as_deref());
            if let Some(draft) = task.draft {
                metadata.insert("draft".to_string(), Value::Bool(draft));
            }
            insert_string(&mut metadata, "serviceTier", task.service_tier.as_deref());
            insert_string(
                &mut metadata,
                "lastFrameUrl",
                task.content.as_ref().and_then(|content| content.last_frame_url.as_deref()),
            );
            let mut provider_metadata = ProviderMetadata::new();
            provider_metadata.insert(provider, Value::Object(metadata));
            Ok(VideoResponse { videos: vec![video], usage, provider_metadata: Some(provider_metadata) })
        })
    }

    fn cancel(&self) -> Option<Cancel<String>> {
        Some(Cancel { method: "DELETE".to_string(), path: Arc::new(|token: &String| task_path(token)) })
    }
}

fn task_path(token: &str) -> String {
    format!("{PATH}/{}", opencode_api_types::js::encode_component(token))
}

fn duration_seconds(task: &Task) -> Option<Number> {
    if let (Some(frames), Some(fps)) = (task.frames.as_ref(), task.framespersecond.as_ref()) {
        let fps = fps.as_f64().unwrap_or(0.0);
        if fps != 0.0
            && let Some(frames) = frames.as_f64()
        {
            return shared::number_from_f64(frames / fps);
        }
    }
    task.duration.clone()
}

fn insert_string(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn insert_number(metadata: &mut Map<String, Value>, key: &str, value: Option<&Number>) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), Value::Number(value.clone()));
    }
}

fn body(request: &VideoRequest, provider: &ProviderId) -> Result<Body, AiError> {
    let mut content = Vec::new();
    if !request.prompt.is_empty() {
        content.push(json!({ "type": "text", "text": request.prompt }));
    }
    if let Some(start) = request.start.as_ref() {
        content.push(item(start, "image_url", "first_frame", provider)?);
    }
    if let Some(end) = request.end.as_ref() {
        content.push(item(end, "image_url", "last_frame", provider)?);
    }
    for asset in &request.references {
        let (kind, role) = match shared::media_kind(&shared::media_type(asset)) {
            "video" => ("video_url", "reference_video"),
            "audio" => ("audio_url", "reference_audio"),
            _ => ("image_url", "reference_image"),
        };
        content.push(item(asset, kind, role, provider)?);
    }
    if let Some(draft_task) = request.provider_options.as_ref().and_then(|options| options.get("draft_task")) {
        content.push(json!({ "type": "draft_task", "draft_task": draft_task }));
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("content".to_string(), Value::Array(content));
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        base.insert("duration".to_string(), Value::Number(duration));
    }
    let aspect = request.aspect.as_deref();
    let ratio = match aspect {
        Some("auto") => Some("adaptive".to_string()),
        Some(aspect) => Some(aspect.to_string()),
        None => None,
    };
    if let Some(ratio) = ratio {
        base.insert("ratio".to_string(), Value::String(ratio));
    }
    if let Some(resolution) = request.resolution.as_deref() {
        base.insert("resolution".to_string(), Value::String(resolution.to_string()));
    }
    if let Some(sound) = request.sound {
        base.insert("generate_audio".to_string(), Value::Bool(sound));
    }
    let native = request.provider_options.as_ref().map(|options| {
        options
            .iter()
            .filter(|(key, _)| key.as_str() != "draft_task")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Map<String, Value>>()
    });
    Ok(Body::Json { value: merge_body(base, native.as_ref(), request.http.as_ref()), query: None })
}

fn item(asset: &MediaAsset, kind: &str, role: &str, provider: &ProviderId) -> Result<Value, AiError> {
    let reference = media_reference(asset, Some(provider), NAME).map_err(shared::invalid_request)?;
    let url = match reference {
        MediaReference::Ref(value) => format!("asset://{value}"),
        other => other.value(),
    };
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String(kind.to_string()));
    object.insert(kind.to_string(), json!({ "url": url }));
    object.insert("role".to_string(), Value::String(role.to_string()));
    Ok(Value::Object(object))
}
