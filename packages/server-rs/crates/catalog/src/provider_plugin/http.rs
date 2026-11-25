use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use opencode_ai::AiErrorReason;

pub use opencode_ai::route::executor::PreparedHttp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HttpError {
    Transport(String),
    Timeout,
    Status(BufferedResponse),
}

impl HttpError {
    pub fn transient(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout => true,
            Self::Status(response) => matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504),
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "HTTP transport error: {message}"),
            Self::Timeout => f.write_str("HTTP request timed out"),
            Self::Status(response) => write!(f, "HTTP status {}", response.status),
        }
    }
}

impl std::error::Error for HttpError {}

pub async fn send(
    http: &dyn opencode_ai::HttpSender,
    request: PreparedHttp,
    timeout: Option<Duration>,
) -> Result<BufferedResponse, HttpError> {
    match timeout {
        Some(duration) => {
            tokio::time::timeout(duration, exchange(http, request)).await.map_err(|_| HttpError::Timeout)?
        }
        None => exchange(http, request).await,
    }
}

pub fn get(url: impl Into<String>, headers: Vec<(String, String)>) -> PreparedHttp {
    PreparedHttp::new("GET", url, headers, Bytes::new())
}

pub fn json(url: impl Into<String>, headers: Vec<(String, String)>, body: &impl serde::Serialize) -> PreparedHttp {
    let mut headers = headers;
    if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("content-type")) {
        headers.push(("content-type".to_owned(), "application/json".to_owned()));
    }
    let body = serde_json::to_vec(body).expect("a provider plugin request body serializes");
    PreparedHttp::new("POST", url, headers, Bytes::from(body))
}

pub fn form(url: impl Into<String>, headers: Vec<(String, String)>, fields: &[(&str, &str)]) -> PreparedHttp {
    let mut headers = headers;
    if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("content-type")) {
        headers.push(("content-type".to_owned(), "application/x-www-form-urlencoded".to_owned()));
    }
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields.iter().copied()).finish();
    PreparedHttp::new("POST", url, headers, Bytes::from(body))
}

async fn exchange(http: &dyn opencode_ai::HttpSender, request: PreparedHttp) -> Result<BufferedResponse, HttpError> {
    let response = http.send(request).await.map_err(|error| map_error(&error.reason))?;
    let mut body = Vec::new();
    let mut stream = response.body;
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.map_err(|error| map_error(&error.reason))?);
    }
    let buffered = BufferedResponse { status: response.status, headers: response.headers, body: Bytes::from(body) };
    match buffered.status {
        200..=299 => Ok(buffered),
        _ => Err(HttpError::Status(buffered)),
    }
}

fn map_error(reason: &AiErrorReason) -> HttpError {
    match reason {
        AiErrorReason::Timeout { .. } => HttpError::Timeout,
        reason => HttpError::Transport(reason.to_string()),
    }
}
