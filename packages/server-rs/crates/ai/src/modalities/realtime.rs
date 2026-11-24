use bytes::Bytes;
use opencode_api_types::js::encode_component;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::clock::utc_datetime;
use crate::json::encode_body;
use crate::media::multipart::Form;
use crate::media::protocol::TextResponse;
use crate::media::route::read_text;
use crate::route::auth::set_header;
use crate::route::executor::{BoxFuture, PreparedHttp, RequestExecutor};
use crate::schema::{AiError, AiErrorReason, RequestTimeout, StringMap};

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const XAI_BASE_URL: &str = "https://api.x.ai/v1";
const GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const GEMINI_WEBSOCKET_URL: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContentConstrained";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeEngine {
    pub id: String,
    pub provider: String,
    pub api: String,
    pub transports: Vec<String>,
}

pub fn engines() -> [RealtimeEngine; 4] {
    let engine = |id: &str, provider: &str, api: &str, transports: &[&str]| RealtimeEngine {
        id: id.to_string(),
        provider: provider.to_string(),
        api: api.to_string(),
        transports: transports.iter().map(|transport| transport.to_string()).collect(),
    };
    [
        engine("openai-live", "openai", "live", &["webrtc"]),
        engine("openai-realtime", "openai", "realtime", &["webrtc", "websocket"]),
        engine("xai-realtime", "xai", "realtime", &["websocket"]),
        engine("gemini-live", "google", "bidi-generate-content", &["websocket"]),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RealtimeTransportInput {
    #[serde(rename_all = "camelCase")]
    Webrtc {
        sdp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dcid: Option<u64>,
    },
    Websocket,
}

impl RealtimeTransportInput {
    pub fn kind(&self) -> &'static str {
        match self {
            RealtimeTransportInput::Webrtc { .. } => "webrtc",
            RealtimeTransportInput::Websocket => "websocket",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RealtimeTransport {
    Webrtc {
        sdp: String,
    },
    #[serde(rename_all = "camelCase")]
    Websocket {
        url: String,
        token: String,
        expires_at: u64,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeConnectInput {
    pub engine: RealtimeEngine,
    pub model: String,
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub session: Option<Map<String, Value>>,
    pub transport: RealtimeTransportInput,
}

pub fn connect(
    input: RealtimeConnectInput,
    executor: &RequestExecutor,
    context: &ExecutionContext,
) -> BoxFuture<Result<RealtimeTransport, AiError>> {
    let executor = executor.clone();
    let context = context.clone();
    Box::pin(async move { connect_inner(input, &executor, &context).await })
}

async fn connect_inner(
    input: RealtimeConnectInput,
    executor: &RequestExecutor,
    context: &ExecutionContext,
) -> Result<RealtimeTransport, AiError> {
    let engine = &input.engine;
    if !engine.transports.iter().any(|transport| transport == input.transport.kind()) {
        return Err(AiError {
            reason: AiErrorReason::InvalidRequest {
                message: format!("{} only supports {} transport", input.model, engine.transports.join(", ")),
                body: None,
                http: None,
                parameter: None,
                classification: None,
            },
        });
    }
    let mut session = input.session.clone().unwrap_or_default();
    session.insert("model".to_string(), Value::String(input.model.clone()));

    match engine.id.as_str() {
        "openai-live" => {
            let mut body = Map::new();
            body.insert("session".to_string(), Value::Object(session));
            body.insert("transport".to_string(), serde_json::to_value(&input.transport).unwrap_or(Value::Null));
            let base_url = input.base_url.as_deref().unwrap_or(OPENAI_BASE_URL);
            let text = post_json(
                JsonPost {
                    url: format!("{base_url}/live/sessions"),
                    api_key: &input.api_key,
                    auth_header: None,
                    headers: input.headers.as_ref(),
                    body,
                    timeout: input.timeout,
                },
                executor,
            )
            .await?;
            let data: LiveSessionResponse = decode("openai-live", &text)?;
            Ok(RealtimeTransport::Webrtc { sdp: data.transport.sdp })
        }
        "openai-realtime" if input.transport.kind() == "websocket" => {
            let mut expires_after = Map::new();
            expires_after.insert("anchor".to_string(), Value::String("created_at".to_string()));
            expires_after.insert("seconds".to_string(), Value::from(60));
            let mut body = Map::new();
            body.insert("expires_after".to_string(), Value::Object(expires_after));
            body.insert("session".to_string(), Value::Object(session));
            client_secret(
                "openai-realtime",
                OPENAI_BASE_URL,
                &input.model,
                &input.api_key,
                body,
                input.timeout,
                executor,
            )
            .await
        }
        "openai-realtime" => {
            let RealtimeTransportInput::Webrtc { sdp, dcid } = &input.transport else {
                return Err(unreachable_transport(&input));
            };
            let mut form = Form::new();
            form.append("sdp", sdp.clone());
            form.append("session", encode_body(&Value::Object(session)));
            let boundary = format!("----WebKitFormBoundary{}", context.ids.uuid().replace('-', ""));
            let query = dcid.map_or_else(String::new, |dcid| format!("?dcid={dcid}"));
            let url = format!("{OPENAI_BASE_URL}/realtime/calls{query}");
            let headers = set_header(Vec::new(), "authorization", format!("Bearer {}", input.api_key));
            let headers = set_header(headers, "content-type", format!("multipart/form-data; boundary={boundary}"));
            let response = executor
                .with_timeout(input.timeout)
                .execute(PreparedHttp::new("POST", url.clone(), headers, Bytes::from(form.serialize(&boundary))), None)
                .await?;
            let text = read_text(response, url).await?;
            Ok(RealtimeTransport::Webrtc { sdp: text.body })
        }
        "xai-realtime" => {
            let mut expires_after = Map::new();
            expires_after.insert("seconds".to_string(), Value::from(60));
            let mut body = Map::new();
            body.insert("expires_after".to_string(), Value::Object(expires_after));
            client_secret("xai-realtime", XAI_BASE_URL, &input.model, &input.api_key, body, input.timeout, executor)
                .await
        }
        "gemini-live" => {
            let now = context.clock.now_ms();
            let expires_at = now + 60_000;
            let mut setup = session;
            setup.insert("model".to_string(), Value::String(format!("models/{}", input.model)));
            let mut body = Map::new();
            body.insert("uses".to_string(), Value::from(1));
            body.insert("expireTime".to_string(), Value::String(utc_datetime((now + 30 * 60_000) as i64).iso_millis()));
            body.insert(
                "newSessionExpireTime".to_string(),
                Value::String(utc_datetime(expires_at as i64).iso_millis()),
            );
            body.insert("bidiGenerateContentSetup".to_string(), Value::Object(setup));
            let text = post_json(
                JsonPost {
                    url: format!("{GOOGLE_BASE_URL}/auth_tokens"),
                    api_key: &input.api_key,
                    auth_header: Some("x-goog-api-key"),
                    headers: None,
                    body,
                    timeout: input.timeout,
                },
                executor,
            )
            .await?;
            let data: AuthTokenResponse = decode("gemini-live", &text)?;
            Ok(RealtimeTransport::Websocket {
                url: GEMINI_WEBSOCKET_URL.to_string(),
                token: data.name,
                expires_at: expires_at / 1000,
            })
        }
        _ => Err(AiError {
            reason: AiErrorReason::InvalidRequest {
                message: format!("Unsupported realtime engine: {}", engine.id),
                body: None,
                http: None,
                parameter: None,
                classification: None,
            },
        }),
    }
}

fn unreachable_transport(input: &RealtimeConnectInput) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: format!("{} does not support the {} transport", input.model, input.transport.kind()),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

struct JsonPost<'a> {
    url: String,
    api_key: &'a str,
    auth_header: Option<&'a str>,
    headers: Option<&'a StringMap>,
    body: Map<String, Value>,
    timeout: Option<RequestTimeout>,
}

async fn post_json(post: JsonPost<'_>, executor: &RequestExecutor) -> Result<TextResponse, AiError> {
    let headers: Vec<(String, String)> = post
        .headers
        .map(|headers| headers.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect())
        .unwrap_or_default();
    let headers = match post.auth_header {
        Some(name) => set_header(headers, name, post.api_key),
        None => set_header(headers, "authorization", format!("Bearer {}", post.api_key)),
    };
    let body = encode_body(&Value::Object(post.body));
    let headers = set_header(headers, "content-type", "application/json");
    let headers = set_header(headers, "content-length", body.len().to_string());
    let url = post.url;
    let response = executor
        .with_timeout(post.timeout)
        .execute(PreparedHttp::new("POST", url.clone(), headers, Bytes::from(body)), None)
        .await?;
    read_text(response, url).await
}

async fn client_secret(
    route: &str,
    base_url: &str,
    model: &str,
    api_key: &str,
    body: Map<String, Value>,
    timeout: Option<RequestTimeout>,
    executor: &RequestExecutor,
) -> Result<RealtimeTransport, AiError> {
    let text = post_json(
        JsonPost {
            url: format!("{base_url}/realtime/client_secrets"),
            api_key,
            auth_header: None,
            headers: None,
            body,
            timeout,
        },
        executor,
    )
    .await?;
    let data: ClientSecretResponse = decode(route, &text)?;
    Ok(RealtimeTransport::Websocket {
        url: format!("{}/realtime?model={}", websocket_base(base_url), encode_component(model)),
        token: data.value,
        expires_at: data.expires_at,
    })
}

fn websocket_base(base_url: &str) -> String {
    match base_url.strip_prefix("http") {
        Some(rest) => format!("ws{rest}"),
        None => base_url.to_string(),
    }
}

fn decode<T: DeserializeOwned>(route: &str, text: &TextResponse) -> Result<T, AiError> {
    serde_json::from_str(&text.body).map_err(|_| AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            route: Some(route.to_string()),
            message: format!("{route} returned an invalid response"),
            body: Some(text.body.clone()),
            http: Some(text.http.clone()),
            classification: None,
        },
    })
}

#[derive(Deserialize)]
struct LiveSessionResponse {
    transport: LiveSessionTransport,
}

#[derive(Deserialize)]
struct LiveSessionTransport {
    sdp: String,
}

#[derive(Deserialize)]
struct ClientSecretResponse {
    value: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct AuthTokenResponse {
    name: String,
}
