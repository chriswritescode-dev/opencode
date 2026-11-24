use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::ExecutionContext;
use crate::media::client::MediaRoute;
use crate::media::generation::{AwaitOptions, Generation};
use crate::media::route::{
    InlineInput, InlineProtocol, MediaEndpoint, MediaRequest, Mode, QueuedInput, QueuedProtocol, StreamedInput,
    StreamedProtocol,
};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ProviderId, ProviderMetadata, ProviderOptions, RequestOptions,
    RequestTimeout, StringMap,
};

pub use crate::modalities::image::MediaUsage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_seconds: Number,
    pub end_seconds: Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionWord {
    pub text: String,
    pub start_seconds: Number,
    pub end_seconds: Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Number>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResponse {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<TranscriptionSegment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<TranscriptionWord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MediaUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum TranscriptionEvent {
    #[serde(rename_all = "camelCase")]
    GenerationProgress {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    TextDelta { delta: String },
    #[serde(rename_all = "camelCase")]
    Segment { segment: TranscriptionSegment },
    #[serde(rename_all = "camelCase")]
    Finish {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<Vec<TranscriptionSegment>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        words: Option<Vec<TranscriptionWord>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<MediaUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
}

#[derive(Clone)]
pub struct TranscriptionModel {
    pub id: String,
    pub provider: ProviderId,
    pub route: Arc<MediaRoute<TranscriptionRequest, TranscriptionResponse, TranscriptionEvent>>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct TranscriptionRequest {
    pub model: TranscriptionModel,
    pub audio: MediaAsset,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub timestamps: Option<String>,
    pub diarize: Option<bool>,
    pub speakers: Option<i64>,
    pub mode: Mode,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionRequestInput {
    pub audio: MediaAsset,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub timestamps: Option<String>,
    #[serde(default)]
    pub diarize: Option<bool>,
    #[serde(default)]
    pub speakers: Option<i64>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl TranscriptionRequestInput {
    pub fn with_model(self, model: TranscriptionModel) -> TranscriptionRequest {
        TranscriptionRequest {
            model,
            audio: self.audio,
            language: self.language,
            prompt: self.prompt,
            timestamps: self.timestamps,
            diarize: self.diarize,
            speakers: self.speakers,
            mode: Mode::Generate,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for TranscriptionRequest {
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
            "language" => self.language.is_some(),
            "prompt" => self.prompt.is_some(),
            "timestamps" => self.timestamps.is_some(),
            "diarize" => self.diarize.is_some(),
            "speakers" => self.speakers.is_some(),
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
            audio: self.audio.clone(),
            language: self.language.clone(),
            prompt: self.prompt.clone(),
            timestamps: self.timestamps.clone(),
            diarize: self.diarize,
            speakers: self.speakers,
            mode: self.mode,
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

impl TranscriptionModel {
    #[allow(clippy::too_many_arguments)]
    pub fn inline(
        id: &str,
        provider: ProviderId,
        protocol: InlineProtocol<TranscriptionRequest, TranscriptionResponse>,
        endpoint: MediaEndpoint<TranscriptionRequest>,
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
        protocol: Arc<dyn StreamedProtocol<TranscriptionRequest, TranscriptionEvent, Frame, State>>,
        endpoint: MediaEndpoint<TranscriptionRequest>,
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

    #[allow(clippy::too_many_arguments)]
    pub fn queued<Token>(
        id: &str,
        provider: ProviderId,
        protocol: Arc<dyn QueuedProtocol<TranscriptionRequest, TranscriptionResponse, Token>>,
        endpoint: MediaEndpoint<TranscriptionRequest>,
        auth: Auth,
        headers: Option<StringMap>,
        transport: Option<RequestOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self
    where
        Token: Clone + Send + Sync + 'static,
    {
        let route = crate::media::route::queued(QueuedInput {
            protocol,
            endpoint,
            auth,
            headers,
            provider_metadata_key: provider.clone(),
        });
        Self { id: id.to_string(), provider, route: Arc::new(MediaRoute::Queued(route)), transport, provider_options }
    }
}

pub fn collect_response(events: Vec<TranscriptionEvent>) -> Result<TranscriptionResponse, AiError> {
    let finish = events.into_iter().find_map(|event| match event {
        TranscriptionEvent::Finish { text, segments, words, language, duration_seconds, usage, provider_metadata } => {
            Some(TranscriptionResponse { text, segments, words, language, duration_seconds, usage, provider_metadata })
        }
        _ => None,
    });
    finish.ok_or_else(|| AiError {
        reason: AiErrorReason::ProviderInternal {
            message: "The transcription stream completed without a finish event".to_string(),
            body: None,
            http: None,
            retry_after_ms: None,
        },
    })
}

pub fn response_events(response: &TranscriptionResponse) -> Vec<TranscriptionEvent> {
    vec![TranscriptionEvent::Finish {
        text: response.text.clone(),
        segments: response.segments.clone(),
        words: response.words.clone(),
        language: response.language.clone(),
        duration_seconds: response.duration_seconds.clone(),
        usage: response.usage.clone(),
        provider_metadata: response.provider_metadata.clone(),
    }]
}

pub struct TranscriptionClient;

impl TranscriptionClient {
    pub fn generate(
        mut request: TranscriptionRequest,
        options: Option<AwaitOptions>,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<TranscriptionResponse, AiError>> {
        request.mode = Mode::Generate;
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
        mut request: TranscriptionRequest,
        options: Option<AwaitOptions>,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> Pin<Box<dyn Stream<Item = Result<TranscriptionEvent, AiError>> + Send>> {
        request.mode = Mode::Stream;
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
        mut request: TranscriptionRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<Generation<TranscriptionResponse>, AiError>> {
        request.mode = Mode::Generate;
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let MediaRoute::Queued(route) = route.as_ref() else {
                return Err(crate::media::client::not_queued("transcription", route.provider(), route.id(), "start"));
            };
            route.start.as_ref()(request, &executor, &context).await
        })
    }

    pub fn resume(
        request: &TranscriptionRequest,
        token: Value,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<Generation<TranscriptionResponse>, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        match route.as_ref() {
            MediaRoute::Queued(route) => route.resume.as_ref()(request, token, &executor, &context),
            other => {
                let error = crate::media::client::not_queued("transcription", other.provider(), other.id(), "resume");
                Box::pin(async move { Err(error) })
            }
        }
    }
}

fn events_from<S>(generated: S) -> Pin<Box<dyn Stream<Item = Result<TranscriptionEvent, AiError>> + Send>>
where
    S: Stream<Item = Result<TranscriptionResponse, AiError>> + Send + 'static,
{
    Box::pin(generated.flat_map(|result| {
        let items: Vec<Result<TranscriptionEvent, AiError>> = match result {
            Ok(response) => response_events(&response).into_iter().map(Ok).collect(),
            Err(error) => vec![Err(error)],
        };
        stream::iter(items)
    }))
}
