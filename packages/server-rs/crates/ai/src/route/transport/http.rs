use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;

use crate::json;
use crate::route::auth::{Auth, AuthInput, set_header};
use crate::route::endpoint::{Endpoint, EndpointInput, set_query};
use crate::route::executor::{Middleware, PreparedHttp};
use crate::route::framing::Definition;
use crate::schema::{AiError, AiErrorReason, LlmRequest, RequestEncoding};

pub type BodyEncoder = Arc<dyn Fn(&Value) -> String + Send + Sync>;
pub type HeaderFactory = Arc<dyn Fn(&LlmRequest) -> Vec<(String, String)> + Send + Sync>;

pub struct JsonRequestInput {
    pub endpoint: Endpoint,
    pub request: LlmRequest,
    pub body: Value,
    pub auth: Auth,
    pub encode_body: Option<BodyEncoder>,
    pub headers: Option<HeaderFactory>,
}

pub struct JsonRequestParts {
    pub url: String,
    pub json_body: Value,
    pub body_text: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct HttpPrepared {
    pub request: PreparedHttp,
    pub framing: Definition,
    pub middleware: Option<Middleware>,
}

pub fn json_request_parts(
    input: JsonRequestInput,
    context: &crate::ExecutionContext,
) -> Result<JsonRequestParts, AiError> {
    let JsonRequestInput { endpoint, request, body, auth, encode_body, headers } = input;
    let encode = |value: &Value| match &encode_body {
        Some(encode_body) => encode_body(value),
        None => json::encode_body(value),
    };
    let mut url = endpoint.render(&EndpointInput { request: &request, body: &body })?;
    if let Some(query) = request.http.as_ref().and_then(|http| http.query.as_ref()) {
        for (key, value) in query.iter() {
            set_query(&mut url, key, value);
        }
    }
    let url = url.to_string();
    let (json_body, body_text) = match request.http.as_ref().and_then(|http| http.body.as_ref()) {
        None => (body.clone(), encode(&body)),
        Some(overlay) => {
            if !body.is_object() {
                return Err(invalid_request("http.body can only overlay JSON object request bodies"));
            }
            let overlaid = json::deep_merge(&body, overlay);
            let text = encode(&overlaid);
            (overlaid, text)
        }
    };
    let mut merged: Vec<(String, String)> = headers.map(|headers| headers(&request)).unwrap_or_default();
    if let Some(extra) = &request.headers {
        for (name, value) in extra.iter() {
            merged = set_header(merged, name, value);
        }
    }
    let headers = auth.apply(
        AuthInput {
            method: "POST".to_string(),
            url: url.clone(),
            body: body_text.clone(),
            headers: merged,
            now_ms: context.clock.now_ms(),
        },
        context.env.as_ref(),
    )?;
    Ok(JsonRequestParts { url, json_body, body_text, headers })
}

pub fn json_post(
    url: String,
    body: String,
    headers: Vec<(String, String)>,
    encoding: Option<RequestEncoding>,
) -> Result<PreparedHttp, AiError> {
    let headers = set_header(headers, "content-type", "application/json");
    let body = match encoding {
        None | Some(RequestEncoding::None) => Bytes::from(body),
        Some(RequestEncoding::Zstd) => return Err(unsupported_encoding("zstd")),
        Some(RequestEncoding::Gzip) => return Err(unsupported_encoding("gzip")),
    };
    let headers = set_header(headers, "content-length", body.len().to_string());
    Ok(PreparedHttp::new("POST", url, headers, body))
}

fn invalid_request(message: impl Into<String>) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: message.into(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

fn unsupported_encoding(encoding: &str) -> AiError {
    AiError {
        reason: AiErrorReason::UnsupportedOperation {
            message: format!("{encoding} request encoding is not supported"),
            body: None,
            http: None,
            operation: "http.encoding".to_string(),
            provider: None,
            route: None,
        },
    }
}
