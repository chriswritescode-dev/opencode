use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::clock::Clock;
use crate::provider_error::{HttpFailureInput, http_failure, provider_error_message};
use crate::schema::{
    AiError, AiErrorReason, HttpContext, HttpTimeout, RequestTimeout, TransportKind, TransportOperation,
};

pub const DEFAULT_HTTP_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_CHUNK_TIMEOUT_MS: u64 = 900_000;
const MAX_BODY_CHARS: usize = 2000;
const MAX_ERROR_BODY_BYTES: usize = 1024 * 1024;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, AiError>> + Send>>;
pub type Next = Arc<dyn Fn(PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> + Send + Sync>;
pub type Middleware = Arc<dyn Fn(PreparedHttp, Next) -> BoxFuture<Result<HttpResponse, AiError>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct PreparedHttp {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl PreparedHttp {
    pub fn new(method: impl Into<String>, url: impl Into<String>, headers: Vec<(String, String)>, body: Bytes) -> Self {
        Self { method: method.into(), url: url.into(), headers: lower_headers(headers), body }
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ByteStream,
}

impl HttpResponse {
    pub fn new(status: u16, headers: Vec<(String, String)>, body: ByteStream) -> Self {
        Self { status, headers: lower_headers(headers), body }
    }
}

pub trait HttpSender: Send + Sync {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>>;
}

#[derive(Debug, Clone)]
pub struct ReqwestSender {
    client: reqwest::Client,
}

impl ReqwestSender {
    pub fn new() -> Self {
        let client =
            reqwest::Client::builder().use_preconfigured_tls(tls_config()).build().expect("reqwest client builds");
        Self { client }
    }
}

impl Default for ReqwestSender {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpSender for ReqwestSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| AiError {
                reason: AiErrorReason::InvalidRequest {
                    message: format!("Invalid HTTP method: {error}"),
                    body: None,
                    http: None,
                    parameter: None,
                    classification: None,
                },
            })?;
            let mut builder = client.request(method, &request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            let response = builder
                .body(request.body)
                .send()
                .await
                .map_err(|error| transport_error(&error, TransportOperation::Request, &request.url, None))?;
            let status = response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .filter_map(|(name, value)| Some((name.as_str().to_lowercase(), value.to_str().ok()?.to_string())))
                .collect();
            let http = HttpContext { url: request.url.clone(), status, headers: header_context(&headers) };
            let read_http = http.clone();
            let body = response.bytes_stream().map(move |chunk| {
                chunk.map_err(|error| {
                    transport_error(&error, TransportOperation::Read, &read_http.url, Some(read_http.clone()))
                })
            });
            Ok(HttpResponse { status, headers, body: Box::pin(body) })
        })
    }
}

#[derive(Clone)]
pub struct RequestExecutor {
    sender: Arc<dyn HttpSender>,
    middleware: Option<Middleware>,
    timeout: Option<RequestTimeout>,
    clock: Arc<dyn Clock>,
}

impl RequestExecutor {
    pub fn new(sender: Arc<dyn HttpSender>, middleware: Option<Middleware>, clock: Arc<dyn Clock>) -> Self {
        Self { sender, middleware, timeout: None, clock }
    }

    pub fn with_middleware(&self, middleware: Middleware) -> Self {
        Self {
            sender: self.sender.clone(),
            middleware: compose(self.middleware.clone(), Some(middleware)),
            timeout: self.timeout.clone(),
            clock: self.clock.clone(),
        }
    }

    pub fn with_timeout(&self, timeout: Option<RequestTimeout>) -> Self {
        Self { sender: self.sender.clone(), middleware: self.middleware.clone(), timeout, clock: self.clock.clone() }
    }

    pub fn without_middleware(&self) -> Self {
        Self { sender: self.sender.clone(), middleware: None, timeout: self.timeout.clone(), clock: self.clock.clone() }
    }

    pub fn sender(&self) -> Arc<dyn HttpSender> {
        self.sender.clone()
    }

    pub fn execute(
        &self,
        request: PreparedHttp,
        per_call: Option<Middleware>,
    ) -> BoxFuture<Result<HttpResponse, AiError>> {
        let executor = self.clone();
        Box::pin(async move { executor.execute_once(request, per_call).await })
    }

    async fn execute_once(&self, request: PreparedHttp, per_call: Option<Middleware>) -> Result<HttpResponse, AiError> {
        let started = tokio::time::Instant::now();
        let limits = timeout_config(self.timeout.as_ref());
        let total = duration(limits.total, None);
        let headers_limit = min_duration(duration(limits.headers, Some(DEFAULT_HTTP_TIMEOUT_MS)), total);
        let chunk_limit = duration(limits.chunk, Some(DEFAULT_CHUNK_TIMEOUT_MS));
        let url = request.url.clone();
        let stack = compose(self.middleware.clone(), per_call);
        let send: BoxFuture<Result<HttpResponse, AiError>> = match stack {
            None => {
                let sender = self.sender.clone();
                Box::pin(async move { sender.send(request).await })
            }
            Some(middleware) => {
                let sender = self.sender.clone();
                middleware(request, Arc::new(move |request| sender.send(request)))
            }
        };
        let raw = match headers_limit {
            Some(limit) => tokio::time::timeout(limit, send).await.map_err(|_| {
                timeout_error(&url, TransportOperation::Request, "Timed out waiting for response headers", None)
            })??,
            None => send.await?,
        };
        let deadline = total.map(|total| started + total);
        let status = raw.status;
        let headers = raw.headers;
        let body = with_body_timeouts(raw.body, chunk_limit, deadline, &url, status, &headers);
        let response = HttpResponse { status, headers, body };
        if status < 400 {
            return Ok(response);
        }
        let text = read_body(response.body).await.ok();
        let message = provider_message(status, text.as_deref());
        let url = Some(url);
        let status = Some(status);
        Err(http_failure(
            HttpFailureInput {
                message,
                url,
                status,
                data: None,
                response_headers: header_context(&response.headers),
                response_body: text,
            },
            self.clock.as_ref(),
        ))
    }
}

pub fn compose(outer: Option<Middleware>, inner: Option<Middleware>) -> Option<Middleware> {
    match (outer, inner) {
        (None, inner) => inner,
        (outer, None) => outer,
        (Some(outer), Some(inner)) => Some(Arc::new(move |request, next| {
            let inner = inner.clone();
            outer(request, Arc::new(move |forwarded| inner(forwarded, next.clone())))
        })),
    }
}

#[derive(Default)]
struct TimeoutLimits {
    total: Option<HttpTimeout>,
    headers: Option<HttpTimeout>,
    chunk: Option<HttpTimeout>,
}

fn timeout_config(timeout: Option<&RequestTimeout>) -> TimeoutLimits {
    match timeout {
        None => TimeoutLimits::default(),
        Some(RequestTimeout::Timeout(value)) => {
            TimeoutLimits { total: Some(value.clone()), ..TimeoutLimits::default() }
        }
        Some(RequestTimeout::Config(options)) => TimeoutLimits {
            total: options.total.clone(),
            headers: options.headers.clone(),
            chunk: options.chunk.clone(),
        },
    }
}

fn duration(value: Option<HttpTimeout>, fallback: Option<u64>) -> Option<Duration> {
    match value {
        Some(HttpTimeout::Disabled(_)) => None,
        Some(HttpTimeout::Millis(millis)) => {
            let millis = millis.as_u64().unwrap_or_else(|| millis.as_f64().unwrap_or(0.0) as u64);
            Some(Duration::from_millis(millis))
        }
        None => fallback.map(Duration::from_millis),
    }
}

fn min_duration(first: Option<Duration>, second: Option<Duration>) -> Option<Duration> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (first, None) => first,
        (None, second) => second,
    }
}

fn with_body_timeouts(
    body: ByteStream,
    chunk: Option<Duration>,
    deadline: Option<tokio::time::Instant>,
    url: &str,
    status: u16,
    headers: &[(String, String)],
) -> ByteStream {
    if chunk.is_none() && deadline.is_none() {
        return body;
    }
    let http = HttpContext { url: url.to_string(), status, headers: header_context(headers) };
    Box::pin(stream::unfold(Some((body, chunk, deadline, http)), |state| async move {
        let (mut body, chunk, deadline, http) = state?;
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()));
        let (limit, total) = match (chunk, remaining) {
            (None, None) => (None, false),
            (Some(chunk), None) => (Some(chunk), false),
            (None, Some(remaining)) => (Some(remaining), true),
            (Some(chunk), Some(remaining)) => {
                if remaining <= chunk {
                    (Some(remaining), true)
                } else {
                    (Some(chunk), false)
                }
            }
        };
        let item = match limit {
            None => body.next().await,
            Some(limit) => match tokio::time::timeout(limit, body.next()).await {
                Ok(item) => item,
                Err(_) => Some(Err(timeout_error(
                    &http.url,
                    TransportOperation::Read,
                    if total {
                        "Timed out waiting for the response to complete"
                    } else {
                        "Timed out waiting for response data"
                    },
                    Some(http.clone()),
                ))),
            },
        };
        match item {
            Some(Ok(bytes)) => Some((Ok(bytes), Some((body, chunk, deadline, http)))),
            Some(Err(error)) => Some((Err(error), None)),
            None => None,
        }
    }))
}

async fn read_body(mut body: ByteStream) -> Result<String, AiError> {
    let mut content = Vec::new();
    while content.len() < MAX_ERROR_BODY_BYTES {
        let Some(chunk) = body.next().await else { break };
        let chunk = chunk?;
        let remaining = MAX_ERROR_BODY_BYTES - content.len();
        content.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&content).into_owned())
}

fn provider_message(status: u16, body: Option<&str>) -> String {
    let fallback = format!("Provider request failed with HTTP {status}");
    let text = body.unwrap_or("").trim();
    if let Some(message) = provider_error_message(text) {
        return message;
    }
    if text.is_empty() || is_html(text) {
        return fallback;
    }
    format!("{fallback}: {}", truncate_body(text))
}

fn is_html(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("<!doctype") || lower.starts_with("<html")
}

fn truncate_body(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= MAX_BODY_CHARS {
        return text.to_string();
    }
    let mut truncated = String::from_utf16_lossy(&units[..MAX_BODY_CHARS]);
    truncated.push('…');
    truncated
}

fn header_context(headers: &[(String, String)]) -> BTreeMap<String, String> {
    headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect()
}

fn lower_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers.into_iter().map(|(name, value)| (name.to_lowercase(), value)).collect()
}

fn timeout_error(url: &str, operation: TransportOperation, message: &str, http: Option<HttpContext>) -> AiError {
    AiError {
        reason: AiErrorReason::Transport {
            message: message.to_string(),
            body: None,
            http,
            transport: TransportKind::Http,
            operation,
            code: Some("Timeout".to_string()),
            url: Some(url.to_string()),
            phase: None,
            delivery: None,
            recovery: None,
        },
    }
}

fn transport_error(
    error: &reqwest::Error,
    operation: TransportOperation,
    url: &str,
    http: Option<HttpContext>,
) -> AiError {
    let timeout = error.is_timeout();
    let message = if timeout {
        "HTTP transport timed out".to_string()
    } else if operation == TransportOperation::Read {
        format!("Connection lost while reading the response: {error}")
    } else {
        format!("HTTP transport failed: {error}")
    };
    AiError {
        reason: AiErrorReason::Transport {
            message,
            body: None,
            http,
            transport: TransportKind::Http,
            operation,
            code: Some(if timeout {
                "Timeout".to_string()
            } else {
                native_code(error).unwrap_or_else(|| "Transport".to_string())
            }),
            url: Some(url.to_string()),
            phase: None,
            delivery: None,
            recovery: None,
        },
    }
}

fn native_code(error: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut current = Some(error);
    while let Some(source) = current {
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            return Some(format!("{:?}", io.kind()));
        }
        current = source.source();
    }
    None
}

fn tls_config() -> rustls::ClientConfig {
    static CONFIG: LazyLock<rustls::ClientConfig> = LazyLock::new(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PlatformVerifier { provider, verifier: OnceLock::new() }))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config
    });
    CONFIG.clone()
}

#[derive(Debug)]
struct PlatformVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    verifier: OnceLock<Result<rustls_platform_verifier::Verifier, rustls::Error>>,
}

impl PlatformVerifier {
    fn get(&self) -> Result<&rustls_platform_verifier::Verifier, rustls::Error> {
        self.verifier
            .get_or_init(|| rustls_platform_verifier::Verifier::new(self.provider.clone()))
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl ServerCertVerifier for PlatformVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.get()?.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.get()?.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.get()?.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
