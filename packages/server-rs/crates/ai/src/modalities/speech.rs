use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Number;

use crate::ExecutionContext;
use crate::media::client::MediaRoute;
use crate::media::route::{MediaEndpoint, MediaRequest, Mode, StreamedInput, StreamedProtocol};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ProviderId, ProviderMetadata, ProviderOptions, RequestOptions,
    RequestTimeout, StringMap,
};

pub use crate::modalities::image::{MediaUsage, token_usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpeechVoice {
    Name(String),
    Id { id: String },
}

impl SpeechVoice {
    pub fn id(&self) -> &str {
        match self {
            SpeechVoice::Name(name) => name,
            SpeechVoice::Id { id } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechTimestamp {
    pub text: String,
    pub start_seconds: Number,
    pub end_seconds: Number,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechResponse {
    pub audio: MediaAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<Vec<SpeechTimestamp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MediaUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum SpeechEvent {
    #[serde(rename_all = "camelCase")]
    GenerationProgress {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    AudioDelta { chunk: String },
    #[serde(rename_all = "camelCase")]
    Timestamps { items: Vec<SpeechTimestamp> },
    #[serde(rename_all = "camelCase")]
    Finish {
        audio: MediaAsset,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<MediaUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
}

#[derive(Clone)]
pub struct SpeechModel {
    pub id: String,
    pub provider: ProviderId,
    pub route: Arc<MediaRoute<SpeechRequest, SpeechResponse, SpeechEvent>>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct SpeechRequest {
    pub model: SpeechModel,
    pub text: String,
    pub voice: Option<SpeechVoice>,
    pub format: Option<String>,
    pub speed: Option<Number>,
    pub language: Option<String>,
    pub instructions: Option<String>,
    pub timestamps: Option<bool>,
    pub mode: Mode,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechRequestInput {
    pub text: String,
    #[serde(default)]
    pub voice: Option<SpeechVoice>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub speed: Option<Number>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub timestamps: Option<bool>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl SpeechRequestInput {
    pub fn with_model(self, model: SpeechModel) -> SpeechRequest {
        SpeechRequest {
            model,
            text: self.text,
            voice: self.voice,
            format: self.format,
            speed: self.speed,
            language: self.language,
            instructions: self.instructions,
            timestamps: self.timestamps,
            mode: Mode::Generate,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for SpeechRequest {
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
            "text" => !self.text.is_empty(),
            "voice" => self.voice.is_some(),
            "format" => self.format.is_some(),
            "speed" => self.speed.is_some(),
            "language" => self.language.is_some(),
            "instructions" => self.instructions.is_some(),
            "timestamps" => self.timestamps == Some(true),
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
            text: self.text.clone(),
            voice: self.voice.clone(),
            format: self.format.clone(),
            speed: self.speed.clone(),
            language: self.language.clone(),
            instructions: self.instructions.clone(),
            timestamps: self.timestamps,
            mode: self.mode,
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

impl SpeechModel {
    #[allow(clippy::too_many_arguments)]
    pub fn streamed<Frame, State>(
        id: &str,
        provider: ProviderId,
        protocol: Arc<dyn StreamedProtocol<SpeechRequest, SpeechEvent, Frame, State>>,
        endpoint: MediaEndpoint<SpeechRequest>,
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

pub fn collect_response(events: Vec<SpeechEvent>) -> Result<SpeechResponse, AiError> {
    let mut timestamps = Vec::new();
    let mut finish = None;
    for event in events {
        match event {
            SpeechEvent::Timestamps { items } => timestamps.extend(items),
            SpeechEvent::Finish { audio, usage, provider_metadata } => {
                finish.get_or_insert((audio, usage, provider_metadata));
            }
            _ => {}
        }
    }
    let Some((audio, usage, provider_metadata)) = finish else {
        return Err(AiError {
            reason: AiErrorReason::ProviderInternal {
                message: "The speech stream completed without a finish event".to_string(),
                body: None,
                http: None,
                retry_after_ms: None,
            },
        });
    };
    Ok(SpeechResponse {
        audio,
        timestamps: if timestamps.is_empty() { None } else { Some(timestamps) },
        usage,
        provider_metadata,
    })
}

pub fn response_events(response: &SpeechResponse) -> Vec<SpeechEvent> {
    let mut events = response
        .timestamps
        .as_ref()
        .map(|items| vec![SpeechEvent::Timestamps { items: items.clone() }])
        .unwrap_or_default();
    events.push(SpeechEvent::Finish {
        audio: response.audio.clone(),
        usage: response.usage.clone(),
        provider_metadata: response.provider_metadata.clone(),
    });
    events
}

pub struct SpeechClient;

impl SpeechClient {
    pub fn generate(
        mut request: SpeechRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<SpeechResponse, AiError>> {
        request.mode = Mode::Generate;
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            match route.as_ref() {
                MediaRoute::Inline(route) => route.generate(request, &executor, &context).await,
                MediaRoute::Queued(route) => {
                    let generation = route.start.as_ref()(request, &executor, &context).await?;
                    generation.await_result(&context, None).await
                }
                MediaRoute::Stream(route) => (route.generate)(request, &executor, &context).await,
            }
        })
    }

    pub fn stream(
        mut request: SpeechRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> Pin<Box<dyn Stream<Item = Result<SpeechEvent, AiError>> + Send>> {
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
                    generation.await_result(&context, None).await
                }))
            }
        }
    }
}

fn events_from<S>(generated: S) -> Pin<Box<dyn Stream<Item = Result<SpeechEvent, AiError>> + Send>>
where
    S: Stream<Item = Result<SpeechResponse, AiError>> + Send + 'static,
{
    Box::pin(generated.flat_map(|result| {
        let items: Vec<Result<SpeechEvent, AiError>> = match result {
            Ok(response) => response_events(&response).into_iter().map(Ok).collect(),
            Err(error) => vec![Err(error)],
        };
        stream::iter(items)
    }))
}
