use std::sync::Arc;

use serde_json::{Map, Number, Value, json};

use crate::media::generation::{Reason, Snapshot, Status};
use crate::media::protocol::{
    Body, DecodeContext, Failure, Identity, Output, State, TextResponse, failure, state, status,
};
use crate::media::route::{CallResponse, Cancel, PollContext, QueuedProtocol, Started, read_text};
use crate::modalities::video::{MediaUsage, VideoRequest, VideoResponse, seconds};
use crate::protocols::shared;
use crate::protocols::utils::media_input::{MediaReference, expiring_url, media_reference, merge_body, url_asset};
use crate::schema::{AiError, MediaAsset, MediaInfo, MediaSource, ProviderId, ProviderMetadata};

pub const ID: &str = "minimax-video";
pub const NAME: &str = "MiniMax Video";
pub const HAILUO_ID: &str = "minimax-hailuo-video";
pub const HAILUO_NAME: &str = "MiniMax Hailuo Video";
pub const DEFAULT_BASE_URL: &str = "https://api.minimax.io";
pub const PATH: &str = "/v2/video_generation";
pub const STATUS_PATH: &str = "/v2/query/video_generation";
pub const HAILUO_PATH: &str = "/v1/video_generation";
pub const HAILUO_STATUS_PATH: &str = "/v1/query/video_generation";
pub const FILE_PATH: &str = "/v1/files/retrieve";
const PROVIDER: &str = "minimax";
const DOWNLOAD_RETENTION_MS: u64 = 3_600_000;

const STATUS: &[(&str, State)] = &[
    ("queued", State::Running),
    ("running", State::Running),
    ("succeeded", State::Completed),
    ("failed", State::Failed),
    ("cancelled", State::Cancelled),
];

const HAILUO_STATUS: &[(&str, State)] = &[
    ("Preparing", State::Running),
    ("Queueing", State::Running),
    ("Processing", State::Running),
    ("Success", State::Completed),
    ("Fail", State::Failed),
];

const FAILURE: &[(&str, Failure)] = &[
    ("1002", Failure::RateLimit),
    ("1004", Failure::Authentication),
    ("1039", Failure::RateLimit),
    ("2013", Failure::InvalidRequest),
    ("2049", Failure::Authentication),
];

const INSUFFICIENT_BALANCE: i64 = 1008;

pub struct MiniMaxVideo {
    hailuo: bool,
    provider: ProviderId,
}

impl MiniMaxVideo {
    fn identity(&self) -> Identity {
        let name = if self.hailuo { HAILUO_NAME } else { NAME };
        let id = if self.hailuo { HAILUO_ID } else { ID };
        Identity { id: id.to_string(), name: name.to_string(), provider: self.provider.clone() }
    }

    fn name(&self) -> &'static str {
        if self.hailuo { HAILUO_NAME } else { NAME }
    }
}

pub fn h3_protocol() -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(MiniMaxVideo { hailuo: false, provider: ProviderId::from(PROVIDER) })
}

pub fn hailuo_protocol() -> Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>> {
    Arc::new(MiniMaxVideo { hailuo: true, provider: ProviderId::from(PROVIDER) })
}

pub fn is_hailuo(model_id: &str) -> bool {
    static HAILUO: std::sync::LazyLock<regex_lite::Regex> = std::sync::LazyLock::new(|| {
        regex_lite::Regex::new(r"(?i)^(minimax-hailuo-|[tis]2v-01|video-01)").expect("hailuo pattern compiles")
    });
    HAILUO.is_match(model_id)
}

#[derive(serde::Deserialize)]
struct BaseResp {
    status_code: Number,
    #[serde(default)]
    status_msg: Option<String>,
}

#[derive(serde::Deserialize)]
struct StartResponse {
    task_id: String,
}

#[derive(serde::Deserialize)]
struct HailuoStartResponse {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(serde::Deserialize)]
struct HailuoTask {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
    #[serde(default)]
    video_width: Option<Number>,
    #[serde(default)]
    video_height: Option<Number>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(serde::Deserialize)]
struct HailuoFile {
    #[serde(default)]
    file: Option<HailuoFileInner>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(serde::Deserialize)]
struct HailuoFileInner {
    #[serde(default)]
    download_url: Option<String>,
}

impl QueuedProtocol<VideoRequest, VideoResponse, String> for MiniMaxVideo {
    fn id(&self) -> &str {
        if self.hailuo { HAILUO_ID } else { ID }
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn token_decode(&self, value: Value) -> Result<String, AiError> {
        match value {
            Value::String(value) => Ok(value),
            _ => Err(self.identity().invalid(format!("{} returned an invalid token", self.name()), None, None)),
        }
    }

    fn token_encode(&self, token: &String) -> Value {
        Value::String(token.clone())
    }

    fn body_from(&self, request: &VideoRequest) -> Result<Body, AiError> {
        if self.hailuo { hailuo_body(request, &self.identity()) } else { h3_body(request) }
    }

    fn start_decode(
        &self,
        response: &TextResponse,
        _context: &DecodeContext<VideoRequest>,
    ) -> Result<Started<String>, AiError> {
        let identity = self.identity();
        if self.hailuo {
            let start: HailuoStartResponse = identity.decode_json(response)?;
            let output = identity.output(response);
            if let Some(error) = base_error(&output, start.base_resp.as_ref()) {
                return Err(error);
            }
            let token = start.task_id.unwrap_or_default();
            if token.is_empty() {
                return Err(output.invalid(format!("{HAILUO_NAME} returned no task_id")));
            }
            return Ok(Started {
                snapshot: Snapshot { id: token.clone(), status: Status::Running, reason: None, progress: None },
                token,
            });
        }
        let start: StartResponse = identity.decode_json(response)?;
        Ok(Started {
            snapshot: Snapshot { id: start.task_id.clone(), status: Status::Running, reason: None, progress: None },
            token: start.task_id,
        })
    }

    fn status_path(&self, token: &String) -> String {
        if self.hailuo {
            format!("{HAILUO_STATUS_PATH}?task_id={}", opencode_api_types::js::encode_component(token))
        } else {
            format!("{STATUS_PATH}/{}", opencode_api_types::js::encode_component(token))
        }
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<String>) -> Result<Snapshot, AiError> {
        let identity = self.identity();
        if self.hailuo {
            let task: HailuoTask = identity.decode_json(response)?;
            let mut snapshot = status(HAILUO_STATUS, task.status.as_deref().unwrap_or(""));
            snapshot.id = context.token.clone();
            if snapshot.reason != Some(Reason::Failed)
                && let Some(error) = base_error(&identity.output(response), task.base_resp.as_ref())
            {
                return Err(error);
            }
            return Ok(snapshot);
        }
        let value: Map<String, Value> = identity.decode_json(response)?;
        let task = task_value(&value);
        let mut snapshot = status(STATUS, task.get("status").and_then(Value::as_str).unwrap_or(""));
        snapshot.id = context.token.clone();
        Ok(snapshot)
    }

    fn result_path(&self, token: &String) -> String {
        self.status_path(token)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<String>,
    ) -> crate::route::executor::BoxFuture<Result<VideoResponse, AiError>> {
        let identity = self.identity();
        if self.hailuo {
            return Box::pin(async move { hailuo_result(&identity, &response, &context).await });
        }
        Box::pin(async move { h3_result(&identity, &response, &context) })
    }

    fn cancel(&self) -> Option<Cancel<String>> {
        if self.hailuo {
            return None;
        }
        Some(Cancel {
            method: "DELETE".to_string(),
            path: Arc::new(|token: &String| format!("{PATH}/{}", opencode_api_types::js::encode_component(token))),
        })
    }
}

fn h3_result(
    identity: &Identity,
    response: &TextResponse,
    context: &PollContext<String>,
) -> Result<VideoResponse, AiError> {
    let value: Map<String, Value> = identity.decode_json(response)?;
    let task = task_value(&value);
    let state = state(STATUS, task.get("status").and_then(Value::as_str).unwrap_or(""));
    let output = identity.output(response);
    if state == State::Running {
        return Err(output.pending(NAME, &context.token));
    }
    let error = task.get("error").and_then(Value::as_object);
    let code = error.and_then(|error| error.get("code"));
    let detail = error.and_then(|error| error.get("message")).and_then(Value::as_str);
    if state == State::Failed {
        return Err(failed(&output, NAME, code, detail));
    }
    if state != State::Completed {
        return Err(output.ended(
            Reason::Cancelled,
            format!("{NAME} task {} was cancelled", context.token),
            Failure::ProviderInternal,
        ));
    }
    let url =
        task.get("content").and_then(Value::as_object).and_then(|content| content.get("url")).and_then(Value::as_str);
    let Some(url) = url else {
        return Err(output.invalid(format!("{NAME} completed without a video URL")));
    };
    let duration = task.get("duration").and_then(Value::as_number).cloned();
    let mut video: MediaAsset = url_asset(url, Some("video/mp4"), None, None);
    video.info = duration.map(|duration| MediaInfo { duration_seconds: Some(duration), ..MediaInfo::default() });
    let total = task
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("total_seconds"))
        .and_then(Value::as_number)
        .cloned();
    let mut metadata = Map::new();
    metadata.insert("taskId".to_string(), Value::String(context.token.clone()));
    if let Some(model) = task.get("model").and_then(Value::as_str) {
        metadata.insert("model".to_string(), Value::String(model.to_string()));
    }
    if let Some(resolution) = task.get("resolution").and_then(Value::as_str) {
        metadata.insert("resolution".to_string(), Value::String(resolution.to_string()));
    }
    if let Some(ratio) = task.get("ratio").and_then(Value::as_str) {
        metadata.insert("ratio".to_string(), Value::String(ratio.to_string()));
    }
    if let Some(usage) = task.get("usage").and_then(Value::as_object) {
        metadata.insert("usage".to_string(), Value::Object(usage.clone()));
    }
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(VideoResponse {
        videos: vec![video],
        usage: total.map(|total| MediaUsage::Seconds { seconds: total }),
        provider_metadata: Some(provider_metadata),
    })
}

fn task_value(value: &Map<String, Value>) -> &Map<String, Value> {
    value.get("task").and_then(Value::as_object).unwrap_or(value)
}

fn h3_body(request: &VideoRequest) -> Result<Body, AiError> {
    let mut content = vec![json!({ "type": "text", "text": request.prompt })];
    if let Some(start) = request.start.as_ref() {
        content.push(item(start, "first_frame")?);
    }
    if let Some(end) = request.end.as_ref() {
        content.push(item(end, "last_frame")?);
    }
    for asset in &request.references {
        content.push(item(asset, reference_role(asset))?);
    }
    let aspect = request.aspect.as_deref();
    let ratio = if content.len() == 1 && (aspect.is_none() || aspect == Some("auto")) {
        Some("16:9".to_string())
    } else if aspect == Some("auto") {
        Some("adaptive".to_string())
    } else {
        aspect.map(str::to_string)
    };
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("content".to_string(), Value::Array(content));
    if let Some(resolution) = request.resolution.as_deref() {
        base.insert("resolution".to_string(), Value::String(resolution.to_uppercase()));
    }
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        base.insert("duration".to_string(), Value::Number(duration));
    }
    if let Some(ratio) = ratio {
        base.insert("ratio".to_string(), Value::String(ratio));
    }
    Ok(Body::Json { value: merge_body(base, request.provider_options.as_ref(), request.http.as_ref()), query: None })
}

fn hailuo_body(request: &VideoRequest, identity: &Identity) -> Result<Body, AiError> {
    let references = &request.references;
    if references.iter().any(|asset| {
        let kind = shared::media_kind(&shared::media_type(asset));
        kind == "video" || kind == "audio"
    }) {
        return Err(
            identity.unsupported("video.references", format!("{HAILUO_NAME} takes only image (subject) references"))
        );
    }
    if let Some(aspect) = request.aspect.as_deref()
        && aspect != "auto"
    {
        return Err(identity.unsupported("video.aspect", format!("{HAILUO_NAME} has no aspect ratio input")));
    }
    let mut subjects = Vec::new();
    for asset in references {
        let url = hailuo_url(asset)?;
        subjects.push(json!({ "type": "character", "image": [url] }));
    }
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if let Some(start) = request.start.as_ref() {
        base.insert("first_frame_image".to_string(), Value::String(hailuo_url(start)?));
    }
    if let Some(end) = request.end.as_ref() {
        base.insert("last_frame_image".to_string(), Value::String(hailuo_url(end)?));
    }
    if !subjects.is_empty() {
        base.insert("subject_reference".to_string(), Value::Array(subjects));
    }
    if let Some(duration) = request.duration.as_deref().and_then(seconds) {
        base.insert("duration".to_string(), Value::Number(duration));
    }
    if let Some(resolution) = request.resolution.as_deref() {
        base.insert("resolution".to_string(), Value::String(resolution.to_uppercase()));
    }
    Ok(Body::Json { value: merge_body(base, request.provider_options.as_ref(), request.http.as_ref()), query: None })
}

fn hailuo_url(asset: &MediaAsset) -> Result<String, AiError> {
    media_reference(asset, None, HAILUO_NAME).map(MediaReference::value).map_err(shared::invalid_request)
}

fn item(asset: &MediaAsset, role: &str) -> Result<Value, AiError> {
    let url = h3_url(asset)?;
    let key = match shared::media_kind(&shared::media_type(asset)) {
        "video" => "video_url",
        "audio" => "audio_url",
        _ => "image_url",
    };
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String(key.to_string()));
    object.insert(key.to_string(), json!({ "url": url }));
    object.insert("role".to_string(), Value::String(role.to_string()));
    Ok(Value::Object(object))
}

fn reference_role(asset: &MediaAsset) -> &'static str {
    match shared::media_kind(&shared::media_type(asset)) {
        "video" => "reference_video",
        "audio" => "reference_audio",
        _ => "reference_image",
    }
}

fn h3_url(asset: &MediaAsset) -> Result<String, AiError> {
    media_reference(asset, Some(PROVIDER), NAME)
        .map(|reference| match reference {
            MediaReference::Ref(value) => format!("mm_file://{value}"),
            other => other.value(),
        })
        .map_err(shared::invalid_request)
}

async fn hailuo_result(
    identity: &Identity,
    response: &TextResponse,
    context: &PollContext<String>,
) -> Result<VideoResponse, AiError> {
    let task: HailuoTask = identity.decode_json(response)?;
    let output = identity.output(response);
    let state = state(HAILUO_STATUS, task.status.as_deref().unwrap_or(""));
    let error = base_error(&output, task.base_resp.as_ref());
    if state == State::Failed {
        return Err(match error {
            Some(error) => error,
            None => failed(&output, HAILUO_NAME, None, None),
        });
    }
    if let Some(error) = error {
        return Err(error);
    }
    if state != State::Completed {
        return Err(output.pending(HAILUO_NAME, &context.token));
    }
    let file_id = task.file_id.clone().unwrap_or_default();
    if file_id.is_empty() {
        return Err(output.invalid(format!("{HAILUO_NAME} completed without a file_id")));
    }
    let path = format!("{FILE_PATH}?file_id={}", opencode_api_types::js::encode_component(&file_id));
    let called: CallResponse = (context.get)(path).await?;
    let file_text = read_text(called.response, called.url).await?;
    let file: HailuoFile = identity.decode_json(&file_text)?;
    let file_output = identity.output(&file_text);
    if let Some(error) = base_error(&file_output, file.base_resp.as_ref()) {
        return Err(error);
    }
    let url = file.file.as_ref().and_then(|file| file.download_url.as_deref());
    let Some(url) = url else {
        return Err(file_output.invalid(format!("{HAILUO_NAME} returned no download_url for {file_id}")));
    };
    let mut video = expiring_url(url, DOWNLOAD_RETENTION_MS, context.clock.now_ms());
    if let MediaSource::Url { media_type, .. } = &mut video.source {
        *media_type = Some("video/mp4".to_string());
    }
    video.info = if task.video_width.is_none() && task.video_height.is_none() {
        None
    } else {
        Some(MediaInfo { width: task.video_width.clone(), height: task.video_height.clone(), ..MediaInfo::default() })
    };
    let mut metadata = Map::new();
    metadata.insert("taskId".to_string(), Value::String(context.token.clone()));
    metadata.insert("fileId".to_string(), Value::String(file_id));
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(PROVIDER.to_string(), Value::Object(metadata));
    Ok(VideoResponse { videos: vec![video], usage: None, provider_metadata: Some(provider_metadata) })
}

fn failed(output: &Output, name: &str, code: Option<&Value>, detail: Option<&str>) -> AiError {
    let code_text = code.and_then(code_text);
    let detail = detail.map(str::trim).filter(|detail| !detail.is_empty());
    let message = format!(
        "{name} generation failed{}{}",
        code_text.as_deref().map(|code| format!(" ({code})")).unwrap_or_default(),
        detail.map(|detail| format!(": {detail}")).unwrap_or_default()
    );
    if let Some(code) = code_text.as_deref()
        && (code == "1026" || code == "1027")
    {
        return output.content_policy(message);
    }
    if let Some(code) = code_text.as_deref()
        && code.parse::<i64>() == Ok(INSUFFICIENT_BALANCE)
    {
        return AiError {
            reason: crate::schema::AiErrorReason::QuotaExceeded {
                message,
                body: Some(output.body.clone()),
                http: Some(output.http.clone()),
                retry_after_ms: None,
            },
        };
    }
    output.ended(Reason::Failed, message, failure(FAILURE, code))
}

fn code_text(code: &Value) -> Option<String> {
    match code {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    }
}

fn base_error(output: &Output, base: Option<&BaseResp>) -> Option<AiError> {
    let base = base?;
    if base.status_code.as_i64() == Some(0) {
        return None;
    }
    Some(failed(output, HAILUO_NAME, Some(&Value::Number(base.status_code.clone())), base.status_msg.as_deref()))
}
