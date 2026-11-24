use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::media::generation::{Reason, Snapshot, Status};
use crate::schema::{AiError, AiErrorReason, HttpContext, InvalidProviderOutputClassification, ProviderId};

pub type Query = Vec<(String, QueryValue)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryValue {
    One(String),
    Many(Vec<String>),
}

pub enum Body {
    Json { value: Map<String, Value>, query: Option<Query> },
    Multipart(crate::media::multipart::Form),
    Binary { bytes: Vec<u8>, content_type: String, query: Option<Query> },
}

impl Body {
    pub fn json(value: Map<String, Value>, query: Option<Query>) -> Self {
        Self::Json { value, query }
    }

    pub fn binary(bytes: Vec<u8>, content_type: impl Into<String>, query: Option<Query>) -> Self {
        Self::Binary { bytes, content_type: content_type.into(), query }
    }

    pub fn query(&self) -> Option<&Query> {
        match self {
            Body::Json { query, .. } | Body::Binary { query, .. } => query.as_ref(),
            Body::Multipart(_) => None,
        }
    }
}

pub struct DecodeContext<Request> {
    pub request: Request,
    pub provider_metadata_key: String,
    pub mode: crate::media::route::Mode,
    pub now_ms: u64,
}

pub struct TextResponse {
    pub body: String,
    pub bytes: Vec<u8>,
    pub http: HttpContext,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub provider: ProviderId,
}

impl Identity {
    pub fn invalid(&self, message: impl Into<String>, body: Option<String>, http: Option<HttpContext>) -> AiError {
        AiError {
            reason: AiErrorReason::InvalidProviderOutput {
                message: message.into(),
                body,
                http,
                classification: None,
                route: Some(self.id.clone()),
            },
        }
    }

    pub fn decode_json<T: DeserializeOwned>(&self, text: &TextResponse) -> Result<T, AiError> {
        serde_json::from_str(&text.body).map_err(|_| {
            self.invalid(
                format!("{} returned an invalid response", self.name),
                Some(text.body.clone()),
                Some(text.http.clone()),
            )
        })
    }

    pub fn frame_error(&self, message: impl Into<String>, body: Option<String>) -> AiError {
        AiError {
            reason: AiErrorReason::InvalidProviderOutput {
                message: message.into(),
                body,
                http: None,
                classification: None,
                route: Some(self.id.clone()),
            },
        }
    }

    pub fn incomplete(&self) -> AiError {
        AiError {
            reason: AiErrorReason::InvalidProviderOutput {
                message: "The provider response ended unexpectedly.".to_string(),
                body: None,
                http: None,
                classification: Some(InvalidProviderOutputClassification::IncompleteStream),
                route: Some(self.id.clone()),
            },
        }
    }

    pub fn unsupported(&self, operation: impl Into<String>, message: impl Into<String>) -> AiError {
        AiError {
            reason: AiErrorReason::UnsupportedOperation {
                message: message.into(),
                body: None,
                http: None,
                operation: operation.into(),
                provider: Some(self.provider.clone()),
                route: Some(self.id.clone()),
            },
        }
    }

    pub fn output(&self, text: &TextResponse) -> Output {
        Output { identity: self.clone(), body: text.body.clone(), http: text.http.clone() }
    }
}

pub struct Output {
    pub identity: Identity,
    pub body: String,
    pub http: HttpContext,
}

impl Output {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, AiError> {
        serde_json::from_str(&self.body)
            .map_err(|_| self.invalid(format!("{} returned an invalid response", self.identity.name)))
    }

    pub fn invalid(&self, message: impl Into<String>) -> AiError {
        AiError {
            reason: AiErrorReason::InvalidProviderOutput {
                message: message.into(),
                body: Some(self.body.clone()),
                http: Some(self.http.clone()),
                classification: None,
                route: Some(self.identity.id.clone()),
            },
        }
    }

    pub fn ended(&self, reason: Reason, message: impl Into<String>, failure: Failure) -> AiError {
        let message = message.into();
        let body = Some(self.body.clone());
        let http = Some(self.http.clone());
        if reason != Reason::Failed {
            return AiError {
                reason: AiErrorReason::InvalidRequest { message, body, http, parameter: None, classification: None },
            };
        }
        let reason = match failure {
            Failure::InvalidRequest => {
                AiErrorReason::InvalidRequest { message, body, http, parameter: None, classification: None }
            }
            Failure::Authentication => AiErrorReason::Authentication { message, body, http },
            Failure::RateLimit => {
                AiErrorReason::RateLimit { message, body, http, retry_after_ms: None, rate_limit: None }
            }
            Failure::ProviderInternal => AiErrorReason::ProviderInternal { message, body, http, retry_after_ms: None },
        };
        AiError { reason }
    }

    pub fn pending(&self, name: &str, token: &str) -> AiError {
        AiError {
            reason: AiErrorReason::InvalidRequest {
                message: format!("{name} generation {token} has not finished; await it before reading the result"),
                body: Some(self.body.clone()),
                http: Some(self.http.clone()),
                parameter: None,
                classification: None,
            },
        }
    }

    pub fn content_policy(&self, message: impl Into<String>) -> AiError {
        AiError {
            reason: AiErrorReason::ContentPolicy {
                message: message.into(),
                body: Some(self.body.clone()),
                http: Some(self.http.clone()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
}

pub fn state(table: &[(&str, State)], raw: &str) -> State {
    table.iter().find(|(key, _)| *key == raw).map(|(_, state)| *state).unwrap_or(State::Running)
}

pub fn status(table: &[(&str, State)], raw: &str) -> Snapshot {
    match state(table, raw) {
        State::Running => snapshot(Status::Running, None),
        State::Completed => snapshot(Status::Completed, None),
        State::Failed => snapshot(Status::Error, Some(Reason::Failed)),
        State::Cancelled => snapshot(Status::Error, Some(Reason::Cancelled)),
        State::Expired => snapshot(Status::Error, Some(Reason::Expired)),
    }
}

fn snapshot(status: Status, reason: Option<Reason>) -> Snapshot {
    Snapshot { id: String::new(), status, reason, progress: None }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidRequest,
    Authentication,
    RateLimit,
    ProviderInternal,
}

pub fn failure(table: &[(&str, Failure)], code: Option<&Value>) -> Failure {
    let Some(code) = code else { return Failure::ProviderInternal };
    let key = match code {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return Failure::ProviderInternal,
    };
    table
        .iter()
        .find(|(existing, _)| *existing == key)
        .map(|(_, failure)| *failure)
        .unwrap_or(Failure::ProviderInternal)
}
