use std::sync::Arc;

use serde_json::{Number, Value};

use crate::ExecutionContext;
use crate::media::route::{MediaEndpoint, MediaRequest, QueuedInput, QueuedProtocol, QueuedRoute};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ProviderId, ProviderMetadata, ProviderOptions, RequestOptions,
    RequestTimeout, StringMap,
};

pub use crate::modalities::image::{MediaUsage, token_usage};

#[derive(Clone)]
pub struct VideoModel {
    pub id: String,
    pub provider: ProviderId,
    pub route: Arc<QueuedRoute<VideoRequest, VideoResponse>>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

impl VideoModel {
    #[allow(clippy::too_many_arguments)]
    pub fn queued(
        id: &str,
        provider: ProviderId,
        protocol: Arc<dyn QueuedProtocol<VideoRequest, VideoResponse, String>>,
        endpoint: MediaEndpoint<VideoRequest>,
        auth: Auth,
        headers: Option<StringMap>,
        transport: Option<RequestOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        let route = crate::media::route::queued(QueuedInput {
            protocol,
            endpoint,
            auth,
            headers,
            provider_metadata_key: provider.clone(),
        });
        Self { id: id.to_string(), provider, route: Arc::new(route), transport, provider_options }
    }
}

#[derive(Clone)]
pub struct VideoRequest {
    pub model: VideoModel,
    pub prompt: String,
    pub start: Option<MediaAsset>,
    pub end: Option<MediaAsset>,
    pub references: Vec<MediaAsset>,
    pub duration: Option<String>,
    pub aspect: Option<String>,
    pub resolution: Option<String>,
    pub sound: Option<bool>,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRequestInput {
    pub prompt: String,
    #[serde(default)]
    pub start: Option<MediaAsset>,
    #[serde(default)]
    pub end: Option<MediaAsset>,
    #[serde(default)]
    pub references: Vec<MediaAsset>,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub aspect: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub sound: Option<bool>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl VideoRequestInput {
    pub fn with_model(self, model: VideoModel) -> VideoRequest {
        VideoRequest {
            model,
            prompt: self.prompt,
            start: self.start,
            end: self.end,
            references: self.references,
            duration: self.duration,
            aspect: self.aspect,
            resolution: self.resolution,
            sound: self.sound,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for VideoRequest {
    fn id(&self) -> &str {
        &self.model.id
    }

    fn provider(&self) -> &ProviderId {
        &self.model.provider
    }

    fn headers(&self) -> Option<&StringMap> {
        self.headers.as_ref()
    }

    fn timeout(&self) -> Option<&RequestTimeout> {
        self.timeout.as_ref()
    }

    fn http(&self) -> Option<&HttpOptions> {
        self.http.as_ref()
    }

    fn provider_options(&self) -> Option<&ProviderOptions> {
        self.provider_options.as_ref()
    }

    fn model_transport(&self) -> Option<&RequestOptions> {
        self.model.transport.as_ref()
    }

    fn model_provider_options(&self) -> Option<&ProviderOptions> {
        self.model.provider_options.as_ref()
    }

    fn present(&self, field: &str) -> bool {
        match field {
            "start" => self.start.is_some(),
            "end" => self.end.is_some(),
            "references" => !self.references.is_empty(),
            "duration" => self.duration.is_some(),
            "aspect" => self.aspect.is_some(),
            "resolution" => self.resolution.is_some(),
            "sound" => self.sound.is_some(),
            _ => false,
        }
    }

    fn resolved(
        &self,
        headers: Option<StringMap>,
        timeout: Option<RequestTimeout>,
        http: Option<HttpOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        Self {
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            start: self.start.clone(),
            end: self.end.clone(),
            references: self.references.clone(),
            duration: self.duration.clone(),
            aspect: self.aspect.clone(),
            resolution: self.resolution.clone(),
            sound: self.sound,
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoResponse {
    pub videos: Vec<MediaAsset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MediaUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VideoJob {
    pub id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VideoStatus {
    pub status: crate::media::generation::Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<crate::media::generation::Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
}

pub fn seconds(duration: &str) -> Option<Number> {
    let value = duration.strip_suffix(" seconds").or_else(|| duration.strip_suffix(" second"))?;
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value, None),
    };
    if whole.is_empty() || !whole.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    if let Some(fraction) = fraction
        && (fraction.is_empty() || !fraction.chars().all(|character| character.is_ascii_digit()))
    {
        return None;
    }
    let seconds: f64 = value.parse().ok()?;
    if seconds.fract() == 0.0 {
        return Some(Number::from(seconds as i64));
    }
    Number::from_f64(seconds)
}

pub struct VideoClient;

impl VideoClient {
    pub fn start(
        request: VideoRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<VideoJob, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let generation = route.start.as_ref()(request, &executor, &context).await?;
            let Some(id) = generation.token.as_str() else {
                return Err(AiError {
                    reason: AiErrorReason::InvalidRequest {
                        message: "The video route returned a token that is not a string".to_string(),
                        body: None,
                        http: None,
                        parameter: None,
                        classification: None,
                    },
                });
            };
            Ok(VideoJob { id: id.to_string() })
        })
    }

    pub fn status(
        model: &VideoModel,
        id: &str,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<VideoStatus, AiError>> {
        let route = model.route.clone();
        let transport = model.transport.clone();
        let executor = executor.clone();
        let context = context.clone();
        let id = id.to_string();
        Box::pin(async move {
            let attached = attach(&route, transport.as_ref(), &id, &executor, &context).await?;
            let snapshot = (attached.status)().await?;
            Ok(VideoStatus { status: snapshot.status, reason: snapshot.reason, progress: snapshot.progress })
        })
    }

    pub fn result(
        model: &VideoModel,
        id: &str,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<VideoResponse, AiError>> {
        let route = model.route.clone();
        let transport = model.transport.clone();
        let executor = executor.clone();
        let context = context.clone();
        let id = id.to_string();
        Box::pin(async move {
            let attached = attach(&route, transport.as_ref(), &id, &executor, &context).await?;
            (attached.result)().await
        })
    }

    pub fn cancel(
        model: &VideoModel,
        id: &str,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<(), AiError>> {
        let route = model.route.clone();
        let transport = model.transport.clone();
        let provider = model.provider.clone();
        let executor = executor.clone();
        let context = context.clone();
        let id = id.to_string();
        Box::pin(async move {
            let attached = attach(&route, transport.as_ref(), &id, &executor, &context).await?;
            match attached.cancel {
                Some(cancel) => cancel.as_ref()().await,
                None => Err(AiError {
                    reason: AiErrorReason::UnsupportedOperation {
                        message: format!("{provider}/{} has no way to cancel a video generation", route.id),
                        body: None,
                        http: None,
                        operation: "video.cancel".to_string(),
                        provider: Some(provider),
                        route: Some(route.id.clone()),
                    },
                }),
            }
        })
    }
}

async fn attach(
    route: &QueuedRoute<VideoRequest, VideoResponse>,
    transport: Option<&RequestOptions>,
    id: &str,
    executor: &RequestExecutor,
    context: &ExecutionContext,
) -> Result<crate::media::generation::GenerationRoute<VideoResponse>, AiError> {
    (route.attach)(transport, Value::String(id.to_string()), executor, context).await
}
