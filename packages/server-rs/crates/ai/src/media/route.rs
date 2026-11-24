#![allow(clippy::type_complexity)]

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use serde_json::Value;
use url::Url;

use crate::ExecutionContext;
use crate::Sleeper;
use crate::json;
use crate::media::generation::{Generation, GenerationRoute, Snapshot};
use crate::media::protocol::{Body, DecodeContext, Query, QueryValue, TextResponse};
use crate::route::auth::{Auth, AuthInput, set_header};
use crate::route::endpoint::{set_query, trim_base_url};
use crate::route::executor::{BoxFuture, ByteStream, HttpResponse, PreparedHttp, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpContext, HttpOptions, ProviderId, ProviderOptions, RequestOptions, RequestTimeout,
    StringMap, merge_provider_options, merge_request_options,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Inline,
    Queued,
    Stream,
}

pub trait MediaRequest: Clone + Send + Sync + 'static {
    fn id(&self) -> &str;
    fn provider(&self) -> &ProviderId;
    fn headers(&self) -> Option<&StringMap>;
    fn timeout(&self) -> Option<&RequestTimeout>;
    fn http(&self) -> Option<&HttpOptions>;
    fn provider_options(&self) -> Option<&ProviderOptions>;
    fn model_transport(&self) -> Option<&RequestOptions>;
    fn model_provider_options(&self) -> Option<&ProviderOptions>;
    fn resolved(
        &self,
        headers: Option<StringMap>,
        timeout: Option<RequestTimeout>,
        http: Option<HttpOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self;
    fn present(&self, _field: &str) -> bool {
        false
    }
}

pub struct MediaEndpoint<Request> {
    pub base_url: Option<String>,
    pub path: Arc<dyn Fn(&Request) -> String + Send + Sync>,
    pub query: StringMap,
}

impl<Request> MediaEndpoint<Request> {
    pub fn path(value: impl Into<String>) -> Self {
        let value = value.into();
        Self { base_url: None, path: Arc::new(move |_| value.clone()), query: StringMap::new() }
    }

    pub fn function(path: Arc<dyn Fn(&Request) -> String + Send + Sync>) -> Self {
        Self { base_url: None, path, query: StringMap::new() }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_query(mut self, query: StringMap) -> Self {
        self.query = query;
        self
    }

    pub fn render(&self, request: &Request) -> Result<Url, AiError> {
        let base = trim_base_url(self.base_url.as_deref().unwrap_or(""));
        let value = format!("{base}{}", (self.path)(request));
        let mut url = Url::parse(&value).map_err(invalid_url)?;
        for (key, value) in self.query.iter() {
            set_query(&mut url, key, value);
        }
        Ok(url)
    }
}

impl<Request> Clone for MediaEndpoint<Request> {
    fn clone(&self) -> Self {
        Self { base_url: self.base_url.clone(), path: self.path.clone(), query: self.query.clone() }
    }
}

pub struct InlineProtocol<Request, Response> {
    pub id: String,
    pub provider: ProviderId,
    pub unsupported: Vec<&'static str>,
    pub body_from: Arc<dyn Fn(&Request) -> Result<Body, AiError> + Send + Sync>,
    pub decode: Arc<dyn Fn(&TextResponse, &DecodeContext<Request>) -> Result<Response, AiError> + Send + Sync>,
}

impl<Request, Response> Clone for InlineProtocol<Request, Response> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            provider: self.provider.clone(),
            unsupported: self.unsupported.clone(),
            body_from: self.body_from.clone(),
            decode: self.decode.clone(),
        }
    }
}

pub struct InlineInput<Request, Response> {
    pub protocol: InlineProtocol<Request, Response>,
    pub endpoint: MediaEndpoint<Request>,
    pub auth: Auth,
    pub headers: Option<StringMap>,
    pub provider_metadata_key: String,
}

pub struct InlineRoute<Request, Response> {
    pub kind: RouteKind,
    pub id: String,
    pub provider: ProviderId,
    pub protocol: String,
    transport: Transport<Request>,
    body_from: Arc<dyn Fn(&Request) -> Result<Body, AiError> + Send + Sync>,
    decode: Arc<dyn Fn(&TextResponse, &DecodeContext<Request>) -> Result<Response, AiError> + Send + Sync>,
    unsupported: Vec<&'static str>,
    provider_metadata_key: String,
}

impl<Request, Response> Clone for InlineRoute<Request, Response> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            id: self.id.clone(),
            provider: self.provider.clone(),
            protocol: self.protocol.clone(),
            transport: self.transport.clone(),
            body_from: self.body_from.clone(),
            decode: self.decode.clone(),
            unsupported: self.unsupported.clone(),
            provider_metadata_key: self.provider_metadata_key.clone(),
        }
    }
}

pub fn inline<Request: MediaRequest, Response: Send + 'static>(
    input: InlineInput<Request, Response>,
) -> InlineRoute<Request, Response> {
    let InlineInput { protocol, endpoint, auth, headers, provider_metadata_key } = input;
    let transport = Transport::new(endpoint, auth, headers, protocol.id.clone(), protocol.provider.clone());
    InlineRoute {
        kind: RouteKind::Inline,
        id: protocol.id.clone(),
        provider: protocol.provider.clone(),
        protocol: protocol.id.clone(),
        transport,
        body_from: protocol.body_from.clone(),
        decode: protocol.decode.clone(),
        unsupported: protocol.unsupported.clone(),
        provider_metadata_key,
    }
}

impl<Request: MediaRequest, Response: Send + 'static> InlineRoute<Request, Response> {
    pub fn generate(
        &self,
        request: Request,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<Response, AiError>> {
        let transport = self.transport.clone();
        let body_from = self.body_from.clone();
        let decode = self.decode.clone();
        let unsupported = self.unsupported.clone();
        let provider_metadata_key = self.provider_metadata_key.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let submitted =
                transport.submit(request, provider_metadata_key, unsupported, body_from, executor, context).await?;
            let Submitted { response, url, context, .. } = submitted;
            let text = read_text(response, url).await?;
            decode(&text, &context)
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Exchange {
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

pub struct Submitted<Request> {
    pub response: HttpResponse,
    pub context: DecodeContext<Request>,
    pub exchange: Exchange,
    pub url: String,
}

pub struct CallResponse {
    pub response: HttpResponse,
    pub url: String,
}

impl<Request> Clone for Transport<Request> {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            auth: self.auth.clone(),
            headers: self.headers.clone(),
            id: self.id.clone(),
            provider: self.provider.clone(),
        }
    }
}

struct Transport<Request> {
    endpoint: MediaEndpoint<Request>,
    auth: Auth,
    headers: Option<StringMap>,
    id: String,
    provider: ProviderId,
}

impl<Request> Transport<Request> {
    fn new(
        endpoint: MediaEndpoint<Request>,
        auth: Auth,
        headers: Option<StringMap>,
        id: String,
        provider: ProviderId,
    ) -> Self {
        Self { endpoint, auth, headers, id, provider }
    }

    fn resolve_url(&self, path: &str) -> Result<Url, AiError> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Url::parse(path).map_err(invalid_url);
        }
        let base = trim_base_url(self.endpoint.base_url.as_deref().unwrap_or(""));
        Url::parse(&format!("{base}{path}")).map_err(invalid_url)
    }
}

impl<Request: MediaRequest> Transport<Request> {
    fn exchange(&self, model: Option<&RequestOptions>) -> Exchange {
        let route = RequestOptions { headers: self.headers.clone(), ..RequestOptions::default() };
        let merged = merge_request_options(&[Some(&route), model]).unwrap_or_default();
        Exchange { headers: merged.headers, timeout: merged.timeout, http: merged.http }
    }

    fn submit(
        &self,
        request: Request,
        provider_metadata_key: String,
        unsupported: Vec<&'static str>,
        body_from: Arc<dyn Fn(&Request) -> Result<Body, AiError> + Send + Sync>,
        executor: RequestExecutor,
        context: ExecutionContext,
    ) -> BoxFuture<Result<Submitted<Request>, AiError>> {
        let transport = self.clone();
        Box::pin(async move {
            transport.submit_inner(request, provider_metadata_key, unsupported, body_from, executor, context).await
        })
    }

    async fn submit_inner(
        &self,
        request: Request,
        provider_metadata_key: String,
        unsupported: Vec<&'static str>,
        body_from: Arc<dyn Fn(&Request) -> Result<Body, AiError> + Send + Sync>,
        executor: RequestExecutor,
        context: ExecutionContext,
    ) -> Result<Submitted<Request>, AiError> {
        let present: Vec<&'static str> = unsupported.into_iter().filter(|field| request.present(field)).collect();
        if let Some(first) = present.first().copied() {
            return Err(AiError {
                reason: AiErrorReason::UnsupportedOperation {
                    message: format!("{}/{} does not support {}", self.provider, self.id, present.join(", ")),
                    body: None,
                    http: None,
                    operation: format!("media.{first}"),
                    provider: Some(self.provider.clone()),
                    route: Some(self.id.clone()),
                },
            });
        }
        let route_options = RequestOptions { headers: self.headers.clone(), ..RequestOptions::default() };
        let request_options = RequestOptions {
            headers: request.headers().cloned(),
            timeout: request.timeout().cloned(),
            http: request.http().cloned(),
            ..RequestOptions::default()
        };
        let merged = merge_request_options(&[Some(&route_options), request.model_transport(), Some(&request_options)])
            .unwrap_or_default();
        let provider_options = merge_provider_options(&[request.model_provider_options(), request.provider_options()]);
        let resolved =
            request.resolved(merged.headers.clone(), merged.timeout.clone(), merged.http.clone(), provider_options);
        let body = body_from(&resolved)?;
        let mut url = self.endpoint.render(&resolved)?;
        if let Some(query) = body.query() {
            apply_query(&mut url, query);
        }
        if let Some(query) = merged.http.as_ref().and_then(|http| http.query.as_ref()) {
            apply_string_map(&mut url, query);
        }
        let mut headers: Vec<(String, String)> = merged
            .headers
            .as_ref()
            .map(|headers| headers.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect())
            .unwrap_or_default();
        let (text, bytes, content_type) = match &body {
            Body::Json { value, .. } => {
                let text = json::encode_body(&Value::Object(value.clone()));
                let bytes = text.as_bytes().to_vec();
                (text, bytes, "application/json".to_string())
            }
            Body::Multipart(form) => {
                headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
                let boundary = format!("----WebKitFormBoundary{}", context.ids.uuid().replace('-', ""));
                let bytes = form.serialize(&boundary);
                ("[multipart/form-data]".to_string(), bytes, format!("multipart/form-data; boundary={boundary}"))
            }
            Body::Binary { bytes, content_type, .. } => {
                (format!("[{content_type}]"), bytes.clone(), content_type.clone())
            }
        };
        let headers = self.auth.apply(
            AuthInput {
                method: "POST".to_string(),
                url: url.to_string(),
                body: text,
                headers,
                now_ms: context.clock.now_ms(),
            },
            context.env.as_ref(),
        )?;
        let headers = set_header(headers, "content-type", content_type);
        let headers = set_header(headers, "content-length", bytes.len().to_string());
        let response = executor
            .with_timeout(merged.timeout.clone())
            .execute(PreparedHttp::new("POST", url.to_string(), headers, Bytes::from(bytes)), None)
            .await?;
        let exchange = Exchange { headers: merged.headers, timeout: merged.timeout, http: merged.http };
        Ok(Submitted {
            response,
            url: url.to_string(),
            context: DecodeContext { request: resolved, provider_metadata_key },
            exchange,
        })
    }

    fn headers(
        &self,
        url: &str,
        exchange: &Exchange,
        _executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> Result<Option<StringMap>, AiError> {
        let Some(base) = self.endpoint.base_url.as_deref().and_then(|base| Url::parse(base).ok()) else {
            return Ok(None);
        };
        let Some(target) = Url::parse(url).ok() else { return Ok(None) };
        if target.origin() != base.origin() {
            return Ok(None);
        }
        let headers: Vec<(String, String)> = exchange
            .headers
            .as_ref()
            .map(|headers| headers.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect())
            .unwrap_or_default();
        let headers = self.auth.apply(
            AuthInput {
                method: "GET".to_string(),
                url: url.to_string(),
                body: String::new(),
                headers,
                now_ms: context.clock.now_ms(),
            },
            context.env.as_ref(),
        )?;
        Ok(Some(StringMap::from_pairs(headers)))
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        exchange: &Exchange,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<CallResponse, AiError>> {
        let transport = self.clone();
        let method = method.to_string();
        let path = path.to_string();
        let exchange = exchange.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let mut url = transport.resolve_url(&path)?;
            if let Some(query) = exchange.http.as_ref().and_then(|http| http.query.as_ref()) {
                apply_string_map(&mut url, query);
            }
            for (key, value) in transport.endpoint.query.iter() {
                set_query(&mut url, key, value);
            }
            let headers: Vec<(String, String)> = exchange
                .headers
                .as_ref()
                .map(|headers| headers.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect())
                .unwrap_or_default();
            let headers = transport.auth.apply(
                AuthInput {
                    method: method.clone(),
                    url: url.to_string(),
                    body: String::new(),
                    headers,
                    now_ms: context.clock.now_ms(),
                },
                context.env.as_ref(),
            )?;
            let response = executor
                .with_timeout(exchange.timeout.clone())
                .execute(PreparedHttp::new(method, url.to_string(), headers, Bytes::new()), None)
                .await?;
            Ok(CallResponse { response, url: url.to_string() })
        })
    }
}

pub fn read_with_retry<T: Send + 'static>(
    sleeper: Arc<dyn Sleeper>,
    attempt: Arc<dyn Fn() -> BoxFuture<Result<T, AiError>> + Send + Sync>,
) -> BoxFuture<Result<T, AiError>> {
    Box::pin(async move {
        let mut retries = 0u32;
        loop {
            match attempt().await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if retries >= 8 || !error.is_retryable() {
                        return Err(error);
                    }
                    let base = 1_000u64.saturating_mul(1u64 << retries);
                    let delay = base.max(error.retry_after_ms().unwrap_or(0)).min(30_000);
                    sleeper.sleep(Duration::from_millis(delay)).await;
                    retries += 1;
                }
            }
        }
    })
}

async fn read_text(response: HttpResponse, url: String) -> Result<TextResponse, AiError> {
    let status = response.status;
    let headers = response.headers;
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(TextResponse {
        body: String::from_utf8_lossy(&bytes).into_owned(),
        http: HttpContext { url, status, headers: headers.into_iter().collect::<BTreeMap<_, _>>() },
    })
}

fn apply_query(url: &mut Url, query: &Query) {
    for (key, value) in query {
        match value {
            QueryValue::One(value) => set_query(url, key, value),
            QueryValue::Many(values) => replace_values(url, key, values),
        }
    }
}

fn apply_string_map(url: &mut Url, query: &StringMap) {
    for (key, value) in query.iter() {
        set_query(url, key, value);
    }
}

fn replace_values(url: &mut Url, key: &str, values: &[String]) {
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(existing, _)| existing != key)
        .map(|(existing, value)| (existing.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    let mut pairs = url.query_pairs_mut();
    for (existing, value) in retained {
        pairs.append_pair(&existing, &value);
    }
    for value in values {
        pairs.append_pair(key, value);
    }
}

fn invalid_url(error: url::ParseError) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: format!("Invalid endpoint URL: {error}"),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

pub struct Started<Token> {
    pub token: Token,
    pub snapshot: Snapshot,
}

#[derive(Clone)]
pub struct PollContext<Token> {
    pub token: Token,
    pub get: Arc<dyn Fn(String) -> BoxFuture<Result<CallResponse, AiError>> + Send + Sync>,
    pub credentials: Arc<dyn Fn(String) -> BoxFuture<Result<Option<StringMap>, AiError>> + Send + Sync>,
}

#[derive(Clone)]
pub struct Cancel<Token> {
    pub method: String,
    pub path: Arc<dyn Fn(&Token) -> String + Send + Sync>,
}

pub trait QueuedProtocol<Request, Response, Token>: Send + Sync {
    fn id(&self) -> &str;
    fn provider(&self) -> &ProviderId;
    fn unsupported(&self) -> &[&'static str] {
        &[]
    }
    fn token_decode(&self, value: Value) -> Result<Token, AiError>;
    fn token_encode(&self, token: &Token) -> Value;
    fn body_from(&self, request: &Request) -> Result<Body, AiError>;
    fn start_decode(
        &self,
        response: &TextResponse,
        context: &DecodeContext<Request>,
    ) -> Result<Started<Token>, AiError>;
    fn status_path(&self, token: &Token) -> String;
    fn status_decode(&self, response: &TextResponse, context: &PollContext<Token>) -> Result<Snapshot, AiError>;
    fn result_path(&self, token: &Token) -> String;
    fn result_decode(&self, response: &TextResponse, context: &PollContext<Token>) -> Result<Response, AiError>;
    fn cancel(&self) -> Option<Cancel<Token>> {
        None
    }
}

pub struct QueuedInput<Request, Response, Token> {
    pub protocol: Arc<dyn QueuedProtocol<Request, Response, Token>>,
    pub endpoint: MediaEndpoint<Request>,
    pub auth: Auth,
    pub headers: Option<StringMap>,
    pub provider_metadata_key: String,
}

pub struct QueuedRoute<Request, Response> {
    pub kind: RouteKind,
    pub id: String,
    pub provider: ProviderId,
    pub protocol: String,
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
    pub attach: Arc<
        dyn Fn(
                &Request,
                Value,
                &RequestExecutor,
                &ExecutionContext,
            ) -> BoxFuture<Result<GenerationRoute<Response>, AiError>>
            + Send
            + Sync,
    >,
}

pub fn queued<Request, Response, Token>(input: QueuedInput<Request, Response, Token>) -> QueuedRoute<Request, Response>
where
    Request: MediaRequest,
    Response: Send + 'static,
    Token: Clone + Send + Sync + 'static,
{
    let QueuedInput { protocol, endpoint, auth, headers, provider_metadata_key } = input;
    let id = protocol.id().to_string();
    let provider = protocol.provider().clone();
    let transport = Transport::new(endpoint, auth, headers, id.clone(), provider.clone());
    let start = {
        let protocol = protocol.clone();
        let transport = transport.clone();
        let provider_metadata_key = provider_metadata_key.clone();
        Arc::new(
            move |request: Request,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Generation<Response>, AiError>> {
                let protocol = protocol.clone();
                let transport = transport.clone();
                let provider_metadata_key = provider_metadata_key.clone();
                let executor = executor.clone();
                let context = context.clone();
                Box::pin(async move {
                    let body_from = {
                        let protocol = protocol.clone();
                        Arc::new(move |request: &Request| protocol.body_from(request))
                    };
                    let submitted = transport
                        .submit(
                            request,
                            provider_metadata_key,
                            protocol.unsupported().to_vec(),
                            body_from,
                            executor.clone(),
                            context.clone(),
                        )
                        .await?;
                    let Submitted { response, url, context: decode_context, exchange } = submitted;
                    let text = read_text(response, url).await?;
                    let started = protocol.start_decode(&text, &decode_context)?;
                    let route = generation_route(
                        transport,
                        protocol.clone(),
                        started.token.clone(),
                        exchange,
                        executor,
                        context,
                    );
                    Ok(Generation::new(route, protocol.token_encode(&started.token), started.snapshot))
                })
            },
        )
    };
    let resume = {
        let protocol = protocol.clone();
        let transport = transport.clone();
        Arc::new(
            move |model: &Request,
                  raw: Value,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Generation<Response>, AiError>> {
                let protocol = protocol.clone();
                let transport = transport.clone();
                let exchange = transport.exchange(model.model_transport());
                let executor = executor.clone();
                let context = context.clone();
                Box::pin(async move {
                    let token = decode_token(&protocol, raw)?;
                    let route =
                        generation_route(transport, protocol.clone(), token.clone(), exchange, executor, context);
                    let snapshot = route.status.as_ref()().await?;
                    Ok(Generation::new(route, protocol.token_encode(&token), snapshot))
                })
            },
        )
    };
    let attach = {
        let protocol = protocol.clone();
        let transport = transport.clone();
        Arc::new(
            move |model: &Request,
                  raw: Value,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<GenerationRoute<Response>, AiError>> {
                let protocol = protocol.clone();
                let transport = transport.clone();
                let exchange = transport.exchange(model.model_transport());
                let executor = executor.clone();
                let context = context.clone();
                Box::pin(async move {
                    let token = decode_token(&protocol, raw)?;
                    Ok(generation_route(transport, protocol, token, exchange, executor, context))
                })
            },
        )
    };
    QueuedRoute { kind: RouteKind::Queued, id, provider, protocol: protocol.id().to_string(), start, resume, attach }
}

fn decode_token<Request, Response, Token>(
    protocol: &Arc<dyn QueuedProtocol<Request, Response, Token>>,
    raw: Value,
) -> Result<Token, AiError> {
    protocol.token_decode(raw).map_err(|_| AiError {
        reason: AiErrorReason::InvalidRequest {
            message: format!("{} cannot resume a generation from this token", protocol.id()),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    })
}

fn generation_route<Request, Response, Token>(
    transport: Transport<Request>,
    protocol: Arc<dyn QueuedProtocol<Request, Response, Token>>,
    token: Token,
    exchange: Exchange,
    executor: RequestExecutor,
    context: ExecutionContext,
) -> GenerationRoute<Response>
where
    Request: MediaRequest,
    Response: Send + 'static,
    Token: Clone + Send + Sync + 'static,
{
    let get: Arc<dyn Fn(String) -> BoxFuture<Result<CallResponse, AiError>> + Send + Sync> = {
        let transport = transport.clone();
        let exchange = exchange.clone();
        let executor = executor.clone();
        let context = context.clone();
        let sleeper = context.sleeper.clone();
        Arc::new(move |path: String| {
            let transport = transport.clone();
            let exchange = exchange.clone();
            let executor = executor.clone();
            let context = context.clone();
            let attempt: Arc<dyn Fn() -> BoxFuture<Result<CallResponse, AiError>> + Send + Sync> =
                Arc::new(move || {
                    let transport = transport.clone();
                    let exchange = exchange.clone();
                    let executor = executor.clone();
                    let context = context.clone();
                    let path = path.clone();
                    Box::pin(async move { transport.call("GET", &path, &exchange, &executor, &context).await })
                });
            read_with_retry(sleeper.clone(), attempt)
        })
    };
    let credentials: Arc<dyn Fn(String) -> BoxFuture<Result<Option<StringMap>, AiError>> + Send + Sync> = {
        let transport = transport.clone();
        let exchange = exchange.clone();
        let executor = executor.clone();
        let context = context.clone();
        Arc::new(move |url: String| {
            let transport = transport.clone();
            let exchange = exchange.clone();
            let executor = executor.clone();
            let context = context.clone();
            Box::pin(async move { transport.headers(&url, &exchange, &executor, &context) })
        })
    };
    let poll_context = PollContext { token: token.clone(), get: get.clone(), credentials };
    let status = {
        let get = get.clone();
        let protocol = protocol.clone();
        let poll_context = poll_context.clone();
        let path = protocol.status_path(&token);
        Arc::new(move || -> BoxFuture<Result<Snapshot, AiError>> {
            let get = get.clone();
            let protocol = protocol.clone();
            let poll_context = poll_context.clone();
            let path = path.clone();
            Box::pin(async move {
                let called = get(path).await?;
                let text = read_text(called.response, called.url).await?;
                protocol.status_decode(&text, &poll_context)
            })
        })
    };
    let result = {
        let get = get.clone();
        let protocol = protocol.clone();
        let poll_context = poll_context.clone();
        let path = protocol.result_path(&token);
        Arc::new(move || -> BoxFuture<Result<Response, AiError>> {
            let get = get.clone();
            let protocol = protocol.clone();
            let poll_context = poll_context.clone();
            let path = path.clone();
            Box::pin(async move {
                let called = get(path).await?;
                let text = read_text(called.response, called.url).await?;
                protocol.result_decode(&text, &poll_context)
            })
        })
    };
    let cancel = protocol.cancel().map(|cancel| {
        let transport = transport.clone();
        let exchange = exchange.clone();
        let executor = executor.clone();
        let context = context.clone();
        let token = token.clone();
        let cancel = cancel.clone();
        let cancel: Arc<dyn Fn() -> BoxFuture<Result<(), AiError>> + Send + Sync> = Arc::new(move || {
            let transport = transport.clone();
            let exchange = exchange.clone();
            let executor = executor.clone();
            let context = context.clone();
            let token = token.clone();
            let cancel = cancel.clone();
            Box::pin(async move {
                let path = (cancel.path)(&token);
                transport.call(&cancel.method, &path, &exchange, &executor, &context).await?;
                Ok(())
            })
        });
        cancel
    });
    GenerationRoute { status, result, cancel }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Generate,
    Stream,
}

pub struct ResponseContext<Request> {
    pub request: Request,
    pub provider_metadata_key: String,
    pub http: HttpContext,
}

pub trait StreamedProtocol<Request, Event, Frame, State>: Send + Sync {
    fn id(&self) -> &str;
    fn provider(&self) -> &ProviderId;
    fn unsupported(&self) -> &[&'static str] {
        &[]
    }
    fn body_from(&self, request: &Request, mode: Mode) -> Result<Body, AiError>;
    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<Request>,
    ) -> Pin<Box<dyn Stream<Item = Result<Frame, AiError>> + Send>>;
    fn initial(&self) -> State;
    fn step(&self, state: State, frame: Frame) -> Result<(State, Vec<Event>), AiError>;
    fn finish(&self, state: &State, context: &ResponseContext<Request>) -> Result<Vec<Event>, AiError>;
}

pub struct StreamedInput<Request, Event, Response, Frame, State> {
    pub protocol: Arc<dyn StreamedProtocol<Request, Event, Frame, State>>,
    pub endpoint: MediaEndpoint<Request>,
    pub auth: Auth,
    pub headers: Option<StringMap>,
    pub collect: Arc<dyn Fn(Vec<Event>) -> Result<Response, AiError> + Send + Sync>,
    pub provider_metadata_key: String,
}

pub struct StreamRoute<Request, Event, Response> {
    pub kind: RouteKind,
    pub id: String,
    pub provider: ProviderId,
    pub protocol: String,
    pub stream: Arc<
        dyn Fn(
                Request,
                &RequestExecutor,
                &ExecutionContext,
            ) -> Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>>
            + Send
            + Sync,
    >,
    pub generate:
        Arc<dyn Fn(Request, &RequestExecutor, &ExecutionContext) -> BoxFuture<Result<Response, AiError>> + Send + Sync>,
}

pub fn streamed<Request, Event, Response, Frame, State>(
    input: StreamedInput<Request, Event, Response, Frame, State>,
) -> StreamRoute<Request, Event, Response>
where
    Request: MediaRequest,
    Event: Send + 'static,
    Response: Send + 'static,
    Frame: Send + 'static,
    State: Send + 'static,
{
    let StreamedInput { protocol, endpoint, auth, headers, collect, provider_metadata_key } = input;
    let id = protocol.id().to_string();
    let provider = protocol.provider().clone();
    let transport = Transport::new(endpoint, auth, headers, id.clone(), provider.clone());
    let stream = {
        let protocol = protocol.clone();
        let transport = transport.clone();
        let provider_metadata_key = provider_metadata_key.clone();
        Arc::new(
            move |request: Request,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>> {
                events(
                    protocol.clone(),
                    transport.clone(),
                    provider_metadata_key.clone(),
                    request,
                    Mode::Stream,
                    executor.clone(),
                    context.clone(),
                )
            },
        )
    };
    let generate = {
        let protocol = protocol.clone();
        let transport = transport.clone();
        let provider_metadata_key = provider_metadata_key.clone();
        Arc::new(
            move |request: Request,
                  executor: &RequestExecutor,
                  context: &ExecutionContext|
                  -> BoxFuture<Result<Response, AiError>> {
                let events = events(
                    protocol.clone(),
                    transport.clone(),
                    provider_metadata_key.clone(),
                    request,
                    Mode::Generate,
                    executor.clone(),
                    context.clone(),
                );
                let collect = collect.clone();
                Box::pin(async move {
                    let mut events = events;
                    let mut collected = Vec::new();
                    while let Some(event) = events.next().await {
                        collected.push(event?);
                    }
                    collect(collected)
                })
            },
        )
    };
    StreamRoute { kind: RouteKind::Stream, id, provider, protocol: protocol.id().to_string(), stream, generate }
}

struct Run<Request, Event, Frame, State> {
    protocol: Arc<dyn StreamedProtocol<Request, Event, Frame, State>>,
    transport: Transport<Request>,
    provider_metadata_key: String,
    request: Option<Request>,
    mode: Mode,
    executor: RequestExecutor,
    context: ExecutionContext,
    active: Option<Active<Request, Frame, State>>,
    queue: VecDeque<Event>,
    done: bool,
}

struct Active<Request, Frame, State> {
    frames: Pin<Box<dyn Stream<Item = Result<Frame, AiError>> + Send>>,
    state: Option<State>,
    http: HttpContext,
    request: Request,
}

fn events<Request, Event, Frame, State>(
    protocol: Arc<dyn StreamedProtocol<Request, Event, Frame, State>>,
    transport: Transport<Request>,
    provider_metadata_key: String,
    request: Request,
    mode: Mode,
    executor: RequestExecutor,
    context: ExecutionContext,
) -> Pin<Box<dyn Stream<Item = Result<Event, AiError>> + Send>>
where
    Request: MediaRequest,
    Event: Send + 'static,
    Frame: Send + 'static,
    State: Send + 'static,
{
    let run = Run {
        protocol,
        transport,
        provider_metadata_key,
        request: Some(request),
        mode,
        executor,
        context,
        active: None,
        queue: VecDeque::new(),
        done: false,
    };
    Box::pin(stream::unfold(Some(run), next_event::<Request, Event, Frame, State>))
}

async fn next_event<Request, Event, Frame, State>(
    state: Option<Run<Request, Event, Frame, State>>,
) -> Option<(Result<Event, AiError>, Option<Run<Request, Event, Frame, State>>)>
where
    Request: MediaRequest,
    Event: Send + 'static,
    Frame: Send + 'static,
    State: Send + 'static,
{
    let mut run = state?;
    loop {
        if let Some(event) = run.queue.pop_front() {
            return Some((Ok(event), Some(run)));
        }
        if run.done {
            return None;
        }
        if run.active.is_some() {
            let next_frame = match run.active.as_mut() {
                Some(active) => active.frames.next().await,
                None => None,
            };
            match next_frame {
                Some(Ok(frame)) => {
                    let previous = run.active.as_mut().and_then(|active| active.state.take());
                    let http = match run.active.as_ref() {
                        Some(active) => active.http.clone(),
                        None => HttpContext { url: String::new(), status: 0, headers: BTreeMap::new() },
                    };
                    let Some(previous) = previous else {
                        run.done = true;
                        continue;
                    };
                    match run.protocol.step(previous, frame) {
                        Ok((next, events)) => {
                            if let Some(active) = run.active.as_mut() {
                                active.state = Some(next);
                            }
                            run.queue.extend(events);
                        }
                        Err(error) => {
                            run.done = true;
                            return Some((Err(error.attach_http(&http)), Some(run)));
                        }
                    }
                }
                Some(Err(error)) => {
                    let http = run.active.as_ref().map(|active| active.http.clone());
                    run.done = true;
                    return Some((
                        Err(match http {
                            Some(http) => error.attach_http(&http),
                            None => error,
                        }),
                        Some(run),
                    ));
                }
                None => {
                    let Some(active) = run.active.take() else {
                        run.done = true;
                        continue;
                    };
                    let Some(state) = active.state else {
                        run.done = true;
                        continue;
                    };
                    let response_context = ResponseContext {
                        request: active.request,
                        provider_metadata_key: run.provider_metadata_key.clone(),
                        http: active.http.clone(),
                    };
                    match run.protocol.finish(&state, &response_context) {
                        Ok(events) => {
                            run.queue.extend(events);
                            run.done = true;
                        }
                        Err(error) => {
                            run.done = true;
                            return Some((Err(error.attach_http(&active.http)), Some(run)));
                        }
                    }
                }
            }
            continue;
        }
        let Some(request) = run.request.take() else {
            run.done = true;
            continue;
        };
        let body_from = {
            let protocol = run.protocol.clone();
            let mode = run.mode;
            Arc::new(move |request: &Request| protocol.body_from(request, mode))
        };
        let submitted = run
            .transport
            .submit(
                request,
                run.provider_metadata_key.clone(),
                run.protocol.unsupported().to_vec(),
                body_from,
                run.executor.clone(),
                run.context.clone(),
            )
            .await;
        match submitted {
            Ok(Submitted { response, url, context: decode_context, .. }) => {
                let http =
                    HttpContext { url, status: response.status, headers: response.headers.iter().cloned().collect() };
                let frames = run.protocol.frames(response.body, &decode_context);
                let state = run.protocol.initial();
                run.active = Some(Active { frames, state: Some(state), http, request: decode_context.request });
            }
            Err(error) => {
                run.done = true;
                return Some((Err(error), Some(run)));
            }
        }
    }
}
