use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::media::client::MediaRoute;
use crate::media::generation::AwaitOptions;
use crate::media::route::{InlineInput, InlineProtocol, MediaEndpoint, MediaRequest, StreamedInput, StreamedProtocol};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ModelId, ProviderId, ProviderMetadata, ProviderOptions,
    RequestOptions, RequestTimeout, StringMap,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MediaUsage {
    #[serde(rename_all = "camelCase")]
    Tokens {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Map<String, Value>>,
    },
    Seconds {
        seconds: Number,
    },
    Characters {
        characters: Number,
    },
    Credits {
        credits: Number,
    },
    Compute {
        seconds: Number,
    },
}

pub fn token_usage(
    input: Option<Number>,
    output: Option<Number>,
    total: Option<Number>,
    details: Option<Map<String, Value>>,
) -> Option<MediaUsage> {
    if input.is_none() && output.is_none() && total.is_none() {
        return None;
    }
    Some(MediaUsage::Tokens { input, output, total, details })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ImageEvent {
    #[serde(rename_all = "camelCase")]
    GenerationProgress {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    ImagePartial { index: i64, image: MediaAsset },
    #[serde(rename_all = "camelCase")]
    Image { index: i64, image: MediaAsset },
    #[serde(rename_all = "camelCase")]
    Finish {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<MediaUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageResponse {
    pub images: Vec<MediaAsset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MediaUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageJob {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatus {
    pub status: crate::media::generation::Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<crate::media::generation::Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
}

#[derive(Clone)]
pub struct ImageModel {
    pub id: ModelId,
    pub provider: ProviderId,
    pub route: Arc<MediaRoute<ImageRequest, ImageResponse, ImageEvent>>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct ImageRequest {
    pub model: ImageModel,
    pub prompt: String,
    pub images: Vec<MediaAsset>,
    pub n: Option<i64>,
    pub aspect: Option<String>,
    pub resolution: Option<String>,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRequestInput {
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<MediaAsset>,
    #[serde(default)]
    pub n: Option<i64>,
    #[serde(default)]
    pub aspect: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl ImageRequestInput {
    pub fn with_model(self, model: ImageModel) -> ImageRequest {
        ImageRequest {
            model,
            prompt: self.prompt,
            images: self.images,
            n: self.n,
            aspect: self.aspect,
            resolution: self.resolution,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for ImageRequest {
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
            "images" => !self.images.is_empty(),
            "n" => self.n.is_some(),
            "aspect" => self.aspect.is_some(),
            "resolution" => self.resolution.is_some(),
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
            images: self.images.clone(),
            n: self.n,
            aspect: self.aspect.clone(),
            resolution: self.resolution.clone(),
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

impl ImageModel {
    #[allow(clippy::too_many_arguments)]
    pub fn inline(
        id: &str,
        provider: ProviderId,
        protocol: InlineProtocol<ImageRequest, ImageResponse>,
        endpoint: MediaEndpoint<ImageRequest>,
        auth: Auth,
        headers: Option<StringMap>,
        transport: Option<RequestOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        let route = crate::media::route::inline(InlineInput {
            protocol,
            endpoint,
            auth,
            headers,
            provider_metadata_key: provider.clone(),
        });
        Self { id: id.to_string(), provider, route: Arc::new(MediaRoute::Inline(route)), transport, provider_options }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn streamed<Frame, State>(
        id: &str,
        provider: ProviderId,
        protocol: Arc<dyn StreamedProtocol<ImageRequest, ImageEvent, Frame, State>>,
        endpoint: MediaEndpoint<ImageRequest>,
        auth: Auth,
        headers: Option<StringMap>,
        transport: Option<RequestOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self
    where
        Frame: Send + 'static,
        State: Send + 'static,
    {
        let route = crate::media::route::streamed(StreamedInput {
            protocol,
            endpoint,
            auth,
            headers,
            collect: Arc::new(collect_response),
            provider_metadata_key: provider.clone(),
        });
        Self { id: id.to_string(), provider, route: Arc::new(MediaRoute::Stream(route)), transport, provider_options }
    }
}

pub fn collect_response(events: Vec<ImageEvent>) -> Result<ImageResponse, AiError> {
    let mut images = Vec::new();
    let mut finish = None;
    for event in events {
        match event {
            ImageEvent::Image { image, .. } => images.push(image),
            ImageEvent::Finish { usage, provider_metadata } => {
                finish.get_or_insert((usage, provider_metadata));
            }
            _ => {}
        }
    }
    let Some((usage, provider_metadata)) = finish else {
        return Err(AiError {
            reason: AiErrorReason::ProviderInternal {
                message: "The image stream completed without a finish event".to_string(),
                body: None,
                http: None,
                retry_after_ms: None,
            },
        });
    };
    Ok(ImageResponse { images, usage, provider_metadata })
}

pub fn response_events(response: &ImageResponse) -> Vec<ImageEvent> {
    let mut events = response
        .images
        .iter()
        .enumerate()
        .map(|(index, image)| ImageEvent::Image { index: index as i64, image: image.clone() })
        .collect::<Vec<_>>();
    events.push(ImageEvent::Finish {
        usage: response.usage.clone(),
        provider_metadata: response.provider_metadata.clone(),
    });
    events
}

pub struct ImageClient;

impl ImageClient {
    pub fn generate(
        request: ImageRequest,
        options: Option<AwaitOptions>,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<ImageResponse, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            match route.as_ref() {
                MediaRoute::Inline(route) => route.generate(request, &executor, &context).await,
                MediaRoute::Queued(route) => {
                    let generation = route.start.as_ref()(request, &executor, &context).await?;
                    generation.await_result(&context, options).await
                }
                MediaRoute::Stream(route) => (route.generate)(request, &executor, &context).await,
            }
        })
    }

    pub fn stream(
        request: ImageRequest,
        options: Option<AwaitOptions>,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> Pin<Box<dyn Stream<Item = Result<ImageEvent, AiError>> + Send>> {
        let route = request.model.route.clone();
        match route.as_ref() {
            MediaRoute::Stream(route) => (route.stream)(request, executor, context),
            MediaRoute::Inline(route) => {
                let generated = route.generate(request, executor, context);
                events_from(stream::once(generated))
            }
            MediaRoute::Queued(route) => {
                let started = route.start.as_ref()(request, executor, context);
                let context = context.clone();
                events_from(stream::once(async move {
                    let generation = started.await?;
                    generation.await_result(&context, options).await
                }))
            }
        }
    }

    pub fn start(
        request: ImageRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<ImageJob, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let MediaRoute::Queued(route) = route.as_ref() else {
                return Err(crate::media::client::not_queued("image", route.provider(), route.id(), "start"));
            };
            let generation = route.start.as_ref()(request, &executor, &context).await?;
            let Some(id) = generation.token.as_str() else {
                return Err(AiError {
                    reason: AiErrorReason::InvalidProviderOutput {
                        message: "The image route returned a token that is not a string".to_string(),
                        body: None,
                        http: None,
                        classification: None,
                        route: Some(route.id.clone()),
                    },
                });
            };
            Ok(ImageJob { id: id.to_string() })
        })
    }

    pub fn status(
        model: &ImageModel,
        id: &str,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<ImageStatus, AiError>> {
        let route = model.route.clone();
        let transport = model.transport.clone();
        let executor = executor.clone();
        let context = context.clone();
        let id = id.to_string();
        Box::pin(async move {
            let route = attach(&route, transport.as_ref(), &id, &executor, &context, "status").await?;
            let snapshot = (route.status)().await?;
            Ok(ImageStatus { status: snapshot.status, reason: snapshot.reason, progress: snapshot.progress })
        })
    }

    pub fn result(
        model: &ImageModel,
        id: &str,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<ImageResponse, AiError>> {
        let route = model.route.clone();
        let transport = model.transport.clone();
        let executor = executor.clone();
        let context = context.clone();
        let id = id.to_string();
        Box::pin(async move {
            let route = attach(&route, transport.as_ref(), &id, &executor, &context, "result").await?;
            (route.result)().await
        })
    }

    pub fn cancel(
        model: &ImageModel,
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
            let attached = attach(&route, transport.as_ref(), &id, &executor, &context, "cancel").await?;
            match attached.cancel {
                Some(cancel) => cancel.as_ref()().await,
                None => Err(AiError {
                    reason: AiErrorReason::UnsupportedOperation {
                        message: format!("{provider}/{} has no way to cancel an image generation", route.id()),
                        body: None,
                        http: None,
                        operation: "image.cancel".to_string(),
                        provider: Some(provider.clone()),
                        route: Some(route.id().to_string()),
                    },
                }),
            }
        })
    }
}

async fn attach(
    route: &MediaRoute<ImageRequest, ImageResponse, ImageEvent>,
    transport: Option<&RequestOptions>,
    id: &str,
    executor: &RequestExecutor,
    context: &ExecutionContext,
    operation: &str,
) -> Result<crate::media::generation::GenerationRoute<ImageResponse>, AiError> {
    let MediaRoute::Queued(queued) = route else {
        return Err(crate::media::client::not_queued("image", route.provider(), route.id(), operation));
    };
    (queued.attach)(transport, Value::String(id.to_string()), executor, context).await
}

fn events_from<S>(generated: S) -> Pin<Box<dyn Stream<Item = Result<ImageEvent, AiError>> + Send>>
where
    S: Stream<Item = Result<ImageResponse, AiError>> + Send + 'static,
{
    Box::pin(generated.flat_map(|result| {
        let items: Vec<Result<ImageEvent, AiError>> = match result {
            Ok(response) => response_events(&response).into_iter().map(Ok).collect(),
            Err(error) => vec![Err(error)],
        };
        stream::iter(items)
    }))
}
