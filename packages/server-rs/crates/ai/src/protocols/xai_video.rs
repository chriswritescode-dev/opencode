use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::media::generation::{Reason, Snapshot, Status};
use crate::media::protocol::{Body, DecodeContext, Failure, Identity, State, TextResponse, failure, state, status};
use crate::media::route::{PollContext, QueuedProtocol, Started};
use crate::modalities::video::{VideoRequest, VideoResponse, seconds};
use crate::protocols::shared;
use crate::protocols::utils::media_input::{MediaReference, media_reference, merge_body, url_asset};
use crate::schema::{AiError, MediaInfo, ProviderId, ProviderMetadata};

pub const ID: &str = "xai-video";
pub const NAME: &str = "xAI Video";
pub const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
pub const PATH: &str = "/videos/generations";
pub const EDIT_PATH: &str = "/videos/edits";
pub const EXTEND_PATH: &str = "/videos/extensions";
pub const STATUS_PATH: &str = "/videos";
const PROVIDER: &str = "xai";

const STATUS: &[(&str, State)] =
    &[("pending", State::Running), ("done", State::Completed), ("failed", State::Failed), ("expired", State::Expired)];

const FAILURE: &[(&str, Failure)] = &[
    ("invalid_argument", Failure::InvalidRequest),
    ("failed_precondition", Failure::InvalidRequest),
    ("permission_denied", Failure::Authentication),
];

pub struct XaiVideo {
    provider: ProviderId,
}

impl XaiVideo {
    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }
}

pub fn protocol() -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(XaiVideo { provider: ProviderId::from(PROVIDER) })
}

pub fn path(request: &VideoRequest) -> String {
    match request.provider_options.as_ref().and_then(|options| options.get("mode")).and_then(Value::as_str) {
        Some("edit") => EDIT_PATH.to_string(),
        Some("extend") => EXTEND_PATH.to_string(),
        _ => PATH.to_string(),
    }
}

#[derive(serde::Deserialize)]
struct StartResponse {
    request_id: String,
}

#[derive(serde::Deserialize)]
struct VideoStatus {
    status: String,
    #[serde(default)]
    progress: Option<Number>,
    #[serde(default)]
    video: Option<VideoInfo>,
    #[serde(default)]
    error: Option<VideoError>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(serde::Deserialize)]
struct VideoInfo {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    duration: Option<Number>,
    #[serde(default)]
    respect_moderation: Option<bool>,
}

#[derive(serde::Deserialize)]
struct VideoError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl QueuedProtocol<VideoRequest, VideoResponse, String> for XaiVideo {
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
        Ok(Started {
            snapshot: Snapshot { id: start.request_id.clone(), status: Status::Running, reason: None, progress: None },
            token: start.request_id,
        })
    }

    fn status_path(&self, token: &String) -> String {
        status_path(token)
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<String>) -> Result<Snapshot, AiError> {
        let identity = self.identity();
        let decoded: VideoStatus = identity.decode_json(response)?;
        let mut snapshot = status(STATUS, &decoded.status);
        snapshot.id = context.token.clone();
        snapshot.progress = fraction(decoded.progress.as_ref());
        Ok(snapshot)
    }

    fn result_path(&self, token: &String) -> String {
        status_path(token)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<String>,
    ) -> crate::route::executor::BoxFuture<Result<VideoResponse, AiError>> {
        let identity = self.identity();
        Box::pin(async move {
            let decoded: VideoStatus = identity.decode_json(&response)?;
            let state = state(STATUS, &decoded.status);
            let output = identity.output(&response);
            if state == State::Running {
                return Err(output.pending(NAME, &context.token));
            }
            if state == State::Failed {
                let code = decoded.error.as_ref().and_then(|error| error.code.as_deref());
                let message = decoded.error.as_ref().and_then(|error| error.message.as_deref());
                let text = format!(
                    "{NAME} generation failed{}{}",
                    code.map(|code| format!(" ({code})")).unwrap_or_default(),
                    message.map(|message| format!(": {message}")).unwrap_or_default()
                );
                let category = code.map(|code| Value::String(code.to_string()));
                return Err(output.ended(Reason::Failed, text, failure(FAILURE, category.as_ref())));
            }
            if state != State::Completed {
                return Err(output.ended(
                    Reason::Expired,
                    format!("{NAME} request {} expired", context.token),
                    Failure::ProviderInternal,
                ));
            }
            let video_info = decoded.video.as_ref();
            let moderated = video_info.and_then(|video| video.respect_moderation) == Some(false);
            let url = video_info.and_then(|video| video.url.as_deref());
            let Some(url) = url else {
                if moderated {
                    return Err(output.content_policy(format!("{NAME} withheld the video for moderation")));
                }
                return Err(output.invalid(format!("{NAME} completed without a video URL")));
            };
            let duration = video_info.and_then(|video| video.duration.clone());
            let mut video = url_asset(url, Some("video/mp4"), None, None);
            video.info =
                duration.map(|duration| MediaInfo { duration_seconds: Some(duration), ..MediaInfo::default() });
            let mut metadata = Map::new();
            metadata.insert("requestId".to_string(), Value::String(context.token.clone()));
            if let Some(model) = decoded.model.as_ref() {
                metadata.insert("model".to_string(), Value::String(model.clone()));
            }
            if let Some(respect) = video_info.and_then(|video| video.respect_moderation) {
                metadata.insert("respectModeration".to_string(), Value::Bool(respect));
            }
            let mut provider_metadata = ProviderMetadata::new();
            provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
            Ok(VideoResponse { videos: vec![video], usage: None, provider_metadata: Some(provider_metadata) })
        })
    }
}

fn status_path(token: &str) -> String {
    format!("{STATUS_PATH}/{}", opencode_api_types::js::encode_component(token))
}

fn fraction(progress: Option<&Number>) -> Option<f64> {
    let progress = progress?.as_f64()?;
    (0.0..=100.0).contains(&progress).then_some(progress / 100.0)
}

fn body(request: &VideoRequest, identity: &Identity) -> Result<Body, AiError> {
    if request.references.iter().any(|asset| shared::media_kind(&shared::media_type(asset)) == "video") {
        return Err(identity.unsupported("video.references", format!("{NAME} has no reference video input")));
    }
    let image = match request.start.as_ref() {
        Some(start) => Some(media_input(start)?),
        None => None,
    };
    let last_frame = match request.end.as_ref() {
        Some(end) => Some(media_input(end)?),
        None => None,
    };
    let mut images = Vec::new();
    for asset in request.references.iter().filter(|asset| shared::media_kind(&shared::media_type(asset)) != "audio") {
        images.push(media_input(asset)?);
    }
    let mut audios = Vec::new();
    for asset in request.references.iter().filter(|asset| shared::media_kind(&shared::media_type(asset)) == "audio") {
        let reference = media_reference(asset, Some(PROVIDER), NAME).map_err(shared::invalid_request)?;
        audios.push(json!({ "url": reference.value() }));
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if let Some(image) = image {
        base.insert("image".to_string(), image);
    }
    if let Some(last_frame) = last_frame {
        base.insert("last_frame".to_string(), last_frame);
    }
    if !images.is_empty() {
        base.insert("reference_images".to_string(), Value::Array(images));
    }
    if !audios.is_empty() {
        base.insert("reference_audios".to_string(), Value::Array(audios));
    }
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        base.insert("duration".to_string(), Value::Number(duration));
    }
    let aspect = request.aspect.as_deref();
    if !(aspect == Some("auto") && request.start.is_some())
        && let Some(aspect) = aspect
    {
        base.insert("aspect_ratio".to_string(), Value::String(aspect.to_string()));
    }
    if let Some(resolution) = request.resolution.as_deref() {
        base.insert("resolution".to_string(), Value::String(resolution.to_string()));
    }
    if let Some(sound) = request.sound {
        base.insert("generate_audio".to_string(), Value::Bool(sound));
    }
    let native = request.provider_options.as_ref().map(native_options);
    Ok(Body::Json { value: merge_body(base, native.as_ref(), request.http.as_ref()), query: None })
}

fn native_options(options: &Map<String, Value>) -> Map<String, Value> {
    options.iter().filter(|(key, _)| key.as_str() != "mode").map(|(key, value)| (key.clone(), value.clone())).collect()
}

fn media_input(asset: &crate::schema::MediaAsset) -> Result<Value, AiError> {
    let reference = media_reference(asset, Some(PROVIDER), NAME).map_err(shared::invalid_request)?;
    Ok(match reference {
        MediaReference::Ref(value) => json!({ "file_id": value }),
        other => json!({ "url": other.value() }),
    })
}
