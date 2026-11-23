use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ids::{ProviderId, RouteId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpContext {
    pub url: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRateLimitDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderFailureClassification {
    ContextOverflow,
    PayloadTooLarge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InvalidProviderOutputClassification {
    IncompleteStream,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Http,
    WebSocket,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportOperation {
    Request,
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportPhase {
    Prepare,
    Queue,
    Connect,
    Send,
    Receive,
    Decode,
    Complete,
    Fallback,
    Close,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    NotSent,
    Rejected,
    Ambiguous,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recovery {
    RetryConnect,
    RetryFull,
    RotateAndRetryFull,
    FallbackHttp,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Error)]
#[serde(tag = "_tag")]
pub enum AiErrorReason {
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    InvalidRequest {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classification: Option<ProviderFailureClassification>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    UnsupportedOperation {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<RouteId>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    Authentication {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    RateLimit {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate_limit: Option<HttpRateLimitDetails>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    QuotaExceeded {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    ContentPolicy {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    ProviderInternal {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    Transport {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        transport: TransportKind,
        operation: TransportOperation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<TransportPhase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<Delivery>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<Recovery>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    InvalidProviderOutput {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classification: Option<InvalidProviderOutputClassification>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<String>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    UnknownProvider {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
    },
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    Timeout {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http: Option<HttpContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Error)]
#[error("{reason}")]
pub struct AiError {
    pub reason: AiErrorReason,
}

impl AiError {
    pub fn message(&self) -> &str {
        match &self.reason {
            AiErrorReason::InvalidRequest { message, .. }
            | AiErrorReason::UnsupportedOperation { message, .. }
            | AiErrorReason::Authentication { message, .. }
            | AiErrorReason::RateLimit { message, .. }
            | AiErrorReason::QuotaExceeded { message, .. }
            | AiErrorReason::ContentPolicy { message, .. }
            | AiErrorReason::ProviderInternal { message, .. }
            | AiErrorReason::Transport { message, .. }
            | AiErrorReason::InvalidProviderOutput { message, .. }
            | AiErrorReason::UnknownProvider { message, .. }
            | AiErrorReason::Timeout { message, .. } => message,
        }
    }

    pub fn status(&self) -> Option<u16> {
        self.http_context().map(|http| http.status)
    }

    pub fn body(&self) -> Option<&str> {
        match &self.reason {
            AiErrorReason::InvalidRequest { body, .. }
            | AiErrorReason::UnsupportedOperation { body, .. }
            | AiErrorReason::Authentication { body, .. }
            | AiErrorReason::RateLimit { body, .. }
            | AiErrorReason::QuotaExceeded { body, .. }
            | AiErrorReason::ContentPolicy { body, .. }
            | AiErrorReason::ProviderInternal { body, .. }
            | AiErrorReason::Transport { body, .. }
            | AiErrorReason::InvalidProviderOutput { body, .. }
            | AiErrorReason::UnknownProvider { body, .. }
            | AiErrorReason::Timeout { body, .. } => body.as_deref(),
        }
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match &self.reason {
            AiErrorReason::RateLimit { retry_after_ms, rate_limit, .. } => {
                (*retry_after_ms).or(rate_limit.as_ref().and_then(|rate_limit| rate_limit.retry_after_ms))
            }
            AiErrorReason::QuotaExceeded { retry_after_ms, .. }
            | AiErrorReason::ProviderInternal { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self.http_context().and_then(|http| http.headers.get("x-should-retry")).map(String::as_str) {
            Some("true") => return true,
            Some("false") => return false,
            _ => {}
        }
        match &self.reason {
            AiErrorReason::RateLimit { .. } | AiErrorReason::ProviderInternal { .. } => true,
            AiErrorReason::Transport { delivery, operation, .. } => {
                delivery.as_ref() != Some(&Delivery::Rejected)
                    && (delivery.as_ref() != Some(&Delivery::Accepted) || *operation == TransportOperation::Read)
            }
            AiErrorReason::InvalidProviderOutput { classification, .. } => {
                *classification == Some(InvalidProviderOutputClassification::IncompleteStream)
            }
            AiErrorReason::UnknownProvider { .. } => true,
            AiErrorReason::Authentication { .. }
            | AiErrorReason::QuotaExceeded { .. }
            | AiErrorReason::ContentPolicy { .. }
            | AiErrorReason::InvalidRequest { .. }
            | AiErrorReason::UnsupportedOperation { .. }
            | AiErrorReason::Timeout { .. } => false,
        }
    }

    pub fn attach_http(mut self, http: &HttpContext) -> AiError {
        let slot = match &mut self.reason {
            AiErrorReason::InvalidRequest { http, .. }
            | AiErrorReason::UnsupportedOperation { http, .. }
            | AiErrorReason::Authentication { http, .. }
            | AiErrorReason::RateLimit { http, .. }
            | AiErrorReason::QuotaExceeded { http, .. }
            | AiErrorReason::ContentPolicy { http, .. }
            | AiErrorReason::ProviderInternal { http, .. }
            | AiErrorReason::Transport { http, .. }
            | AiErrorReason::InvalidProviderOutput { http, .. }
            | AiErrorReason::UnknownProvider { http, .. }
            | AiErrorReason::Timeout { http, .. } => http,
        };
        if slot.is_none() {
            *slot = Some(http.clone());
        }
        self
    }

    pub fn http_context(&self) -> Option<&HttpContext> {
        match &self.reason {
            AiErrorReason::InvalidRequest { http, .. }
            | AiErrorReason::UnsupportedOperation { http, .. }
            | AiErrorReason::Authentication { http, .. }
            | AiErrorReason::RateLimit { http, .. }
            | AiErrorReason::QuotaExceeded { http, .. }
            | AiErrorReason::ContentPolicy { http, .. }
            | AiErrorReason::ProviderInternal { http, .. }
            | AiErrorReason::Transport { http, .. }
            | AiErrorReason::InvalidProviderOutput { http, .. }
            | AiErrorReason::UnknownProvider { http, .. }
            | AiErrorReason::Timeout { http, .. } => http.as_ref(),
        }
    }
}
