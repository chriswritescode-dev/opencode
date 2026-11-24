#![allow(clippy::type_complexity)]

use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::ExecutionContext;
use crate::media::generation::{AwaitOptions, Generation};
use crate::media::route::{InlineRoute, QueuedRoute, RouteKind, StreamRoute};
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{AiError, AiErrorReason, ProviderId};

#[allow(clippy::large_enum_variant)]
pub enum MediaRoute<Request, Response, Event> {
    Inline(InlineRoute<Request, Response>),
    Queued(QueuedRoute<Request, Response>),
    Stream(StreamRoute<Request, Event, Response>),
}

impl<Request, Response, Event> MediaRoute<Request, Response, Event> {
    pub fn kind(&self) -> RouteKind {
        match self {
            MediaRoute::Inline(_) => RouteKind::Inline,
            MediaRoute::Queued(_) => RouteKind::Queued,
            MediaRoute::Stream(_) => RouteKind::Stream,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            MediaRoute::Inline(route) => &route.id,
            MediaRoute::Queued(route) => &route.id,
            MediaRoute::Stream(route) => &route.id,
        }
    }

    pub fn provider(&self) -> &ProviderId {
        match self {
            MediaRoute::Inline(route) => &route.provider,
            MediaRoute::Queued(route) => &route.provider,
            MediaRoute::Stream(route) => &route.provider,
        }
    }
}

pub struct MediaOperations<Request, Response, Event> {
    pub generate: Arc<
        dyn Fn(
                Request,
                Option<AwaitOptions>,
                &RequestExecutor,
                &ExecutionContext,
            ) -> BoxFuture<Result<Response, AiError>>
            + Send
            + Sync,
    >,
    pub stream: Arc<
        dyn Fn(
                Request,
                Option<AwaitOptions>,
                &RequestExecutor,
                &ExecutionContext,
            ) -> Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>>
            + Send
            + Sync,
    >,
    pub start: Arc<
        dyn Fn(Request, &RequestExecutor, &ExecutionContext) -> BoxFuture<Result<Generation<Response>, AiError>>
            + Send
            + Sync,
    >,
    pub resume: Arc<
        dyn Fn(&Request, Value, &RequestExecutor, &ExecutionContext) -> BoxFuture<Result<Generation<Response>, AiError>>
            + Send
            + Sync,
    >,
}

pub fn not_queued(modality: &str, provider: &ProviderId, route: &str, operation: &str) -> AiError {
    AiError {
        reason: AiErrorReason::UnsupportedOperation {
            message: format!("{provider}/{route} is not a queued route; use generate or stream"),
            body: None,
            http: None,
            operation: format!("{modality}.{operation}"),
            provider: Some(provider.clone()),
            route: Some(route.to_string()),
        },
    }
}

pub fn operations<Request, Response, Event>(
    route: MediaRoute<Request, Response, Event>,
    modality: impl Into<String>,
    response_events: Arc<dyn Fn(&Response) -> Vec<Event> + Send + Sync>,
) -> MediaOperations<Request, Response, Event>
where
    Request: crate::media::route::MediaRequest,
    Response: Send + 'static,
    Event: Send + 'static,
{
    let modality = modality.into();
    let route = Arc::new(route);
    let generate = {
        let route = route.clone();
        Arc::new(
            move |request: Request,
                  options: Option<AwaitOptions>,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Response, AiError>> {
                let route = route.clone();
                let executor = executor.clone();
                let context = context.clone();
                Box::pin(async move {
                    match route.as_ref() {
                        MediaRoute::Inline(route) => route.generate(request, &executor, &context).await,
                        MediaRoute::Queued(route) => {
                            let generation = route.start.as_ref()(request, &executor, &context).await?;
                            generation.await_result(&context, options).await
                        }
                        MediaRoute::Stream(route) => route.generate.as_ref()(request, &executor, &context).await,
                    }
                })
            },
        )
    };
    let stream = {
        let route = route.clone();
        let response_events = response_events.clone();
        Arc::new(
            move |request: Request,
                  options: Option<AwaitOptions>,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>> {
                let executor = executor.clone();
                let context = context.clone();
                match route.as_ref() {
                    MediaRoute::Stream(route) => route.stream.as_ref()(request, &executor, &context),
                    MediaRoute::Inline(route) => {
                        let generated = route.generate(request, &executor, &context);
                        let response_events = response_events.clone();
                        let events: Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>> =
                            Box::pin(stream::once(generated).flat_map(move |result| {
                                let items: Vec<Result<Event, AiError>> = match result {
                                    Ok(response) => response_events(&response).into_iter().map(Ok).collect(),
                                    Err(error) => vec![Err(error)],
                                };
                                stream::iter(items)
                            }));
                        events
                    }
                    MediaRoute::Queued(route) => {
                        let started = route.start.as_ref()(request, &executor, &context);
                        let response_events = response_events.clone();
                        let events: Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>> = Box::pin(
                            stream::once(async move {
                                let generation = started.await?;
                                generation.await_result(&context, options).await
                            })
                            .flat_map(move |result| {
                                let items: Vec<Result<Event, AiError>> = match result {
                                    Ok(response) => response_events(&response).into_iter().map(Ok).collect(),
                                    Err(error) => vec![Err(error)],
                                };
                                stream::iter(items)
                            }),
                        );
                        events
                    }
                }
            },
        )
    };
    let start = {
        let route = route.clone();
        let modality = modality.clone();
        Arc::new(
            move |request: Request,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Generation<Response>, AiError>> {
                let route = route.clone();
                let modality = modality.clone();
                let executor = executor.clone();
                let context = context.clone();
                Box::pin(async move {
                    match route.as_ref() {
                        MediaRoute::Queued(route) => route.start.as_ref()(request, &executor, &context).await,
                        other => Err(not_queued(&modality, other.provider(), other.id(), "start")),
                    }
                })
            },
        )
    };
    let resume = {
        let modality = modality.clone();
        Arc::new(
            move |model: &Request,
                  token: Value,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Generation<Response>, AiError>> {
                let route = route.clone();
                let modality = modality.clone();
                let executor = executor.clone();
                let context = context.clone();
                match route.as_ref() {
                    MediaRoute::Queued(route) => route.resume.as_ref()(model, token, &executor, &context),
                    other => {
                        let error = not_queued(&modality, other.provider(), other.id(), "resume");
                        Box::pin(async move { Err(error) })
                    }
                }
            },
        )
    };
    MediaOperations { generate, stream, start, resume }
}
