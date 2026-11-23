use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, Stream, StreamExt, stream};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, Uri};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

use crate::route::executor::BoxFuture;
use crate::route::framing::FrameStream;
use crate::route::transport::channel::{
    ChannelCreate, ChannelDriver, ChannelExchange, ChannelExecution, ChannelExecutor, ChannelObservation,
};
use crate::schema::{AiError, AiErrorReason, Delivery, HttpContext, TransportKind, TransportOperation, TransportPhase};

pub use crate::route::transport::channel::WebSocketInterceptor;

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WebSocketRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

pub type MessageStream = Pin<Box<dyn Stream<Item = Result<Message, AiError>> + Send>>;

type SocketSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, tungstenite::Message>;

pub struct WebSocketConnection {
    pub http: Option<HttpContext>,
    messages: std::sync::Mutex<Option<MessageStream>>,
    sink: Arc<Mutex<SocketSink>>,
    closed: Arc<AtomicBool>,
    url: String,
}

impl WebSocketConnection {
    pub fn messages(&mut self) -> MessageStream {
        self.messages
            .lock()
            .expect("WebSocket messages lock poisoned")
            .take()
            .expect("WebSocket messages already taken")
    }

    pub async fn send_text(&self, message: String) -> Result<(), AiError> {
        let url = self.url.clone();
        let closed = self.closed.clone();
        let sink = self.sink.clone();
        if closed.load(Ordering::SeqCst) {
            return Err(not_open_error(&url));
        }
        let mut sink = sink.lock().await;
        sink.send(tungstenite::Message::Text(message.into())).await.map_err(|error| {
            websocket_error(
                error.to_string(),
                Some(url),
                TransportOperation::Write,
                None,
                Some(TransportPhase::Send),
                Some(Delivery::NotSent),
                None,
            )
        })
    }

    pub async fn close(&self) {
        let sink = self.sink.clone();
        let closed = self.closed.clone();
        close_sink(&sink, &closed, 1000, "").await;
    }

    pub async fn close_with(&self, code: u16, reason: &str) {
        let sink = self.sink.clone();
        let closed = self.closed.clone();
        close_sink(&sink, &closed, code, reason).await;
    }
}

pub trait WebSocketConnector: Send + Sync {
    fn open(&self, request: WebSocketRequest) -> BoxFuture<Result<WebSocketConnection, AiError>>;
}

pub struct SystemConnector;

impl WebSocketConnector for SystemConnector {
    fn open(&self, request: WebSocketRequest) -> BoxFuture<Result<WebSocketConnection, AiError>> {
        open(request)
    }
}

pub fn to_websocket_url(value: &str) -> Result<String, AiError> {
    let mut url = match url::Url::parse(value) {
        Ok(url) => url,
        Err(_) => return Err(invalid_url_error("Invalid URL", value)),
    };
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        scheme => return Err(invalid_url_error(format!("Unsupported WebSocket URL protocol {scheme}:"), value)),
    };
    if url.set_scheme(scheme).is_err() {
        return Err(invalid_url_error("Invalid URL", value));
    }
    Ok(url.to_string())
}

pub fn open(request: WebSocketRequest) -> BoxFuture<Result<WebSocketConnection, AiError>> {
    Box::pin(async move {
        let target = url::Url::parse(&request.url).map_err(|error| construct_error(&request.url, error))?;
        let uri = target.as_str().parse::<Uri>().map_err(|error| construct_error(&request.url, error))?;
        let mut handshake = uri.into_client_request().map_err(|error| construct_error(&request.url, error))?;
        for (name, value) in &request.headers {
            let name = HeaderName::try_from(name.as_str()).map_err(|error| construct_error(&request.url, error))?;
            let value = HeaderValue::from_str(value).map_err(|error| construct_error(&request.url, error))?;
            handshake.headers_mut().append(name, value);
        }
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(4 * MAX_FRAME_BYTES))
            .max_frame_size(Some(4 * MAX_FRAME_BYTES));
        let (socket, _) = connect_async_with_config(handshake, Some(config), true)
            .await
            .map_err(|error| connect_error(&request.url, error))?;
        let (sink, source) = socket.split();
        let closed = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(Mutex::new(sink));
        let messages = read_stream(source, request.url.clone(), sink.clone(), closed.clone());
        Ok(WebSocketConnection {
            http: None,
            messages: std::sync::Mutex::new(Some(messages)),
            sink,
            closed,
            url: request.url,
        })
    })
}

pub fn message_text(message: Message) -> String {
    match message {
        Message::Text(text) => text,
        Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
    }
}

pub fn make_direct(
    connector: Arc<dyn WebSocketConnector>,
    interceptor: Option<Arc<dyn WebSocketInterceptor>>,
) -> Arc<dyn ChannelExecutor> {
    Arc::new(DirectExecutor { connector, interceptor })
}

struct DirectExecutor {
    connector: Arc<dyn WebSocketConnector>,
    interceptor: Option<Arc<dyn WebSocketInterceptor>>,
}

impl ChannelExecutor for DirectExecutor {
    fn execute(&self, exchange: ChannelExchange) -> BoxFuture<Result<ChannelExecution, AiError>> {
        let connector = self.connector.clone();
        let interceptor = self.interceptor.clone();
        Box::pin(async move { direct_execution(connector, interceptor, exchange).await })
    }
}

async fn direct_execution(
    connector: Arc<dyn WebSocketConnector>,
    interceptor: Option<Arc<dyn WebSocketInterceptor>>,
    exchange: ChannelExchange,
) -> Result<ChannelExecution, AiError> {
    let connect = match &interceptor {
        Some(interceptor) => interceptor.handshake(exchange.connect),
        None => exchange.connect,
    };
    let mut connection =
        connector.open(connect).await.map_err(|error| annotate(error, TransportPhase::Connect, Delivery::NotSent))?;
    let mut driver = exchange.driver;
    let create = match driver.create(None) {
        Ok(create) => create,
        Err(error) => {
            connection.close().await;
            return Err(error);
        }
    };
    let message = match &interceptor {
        Some(interceptor) => interceptor.send(create.message.clone()),
        None => create.message.clone(),
    };
    if let Err(error) = connection.send_text(message).await {
        connection.close().await;
        return Err(attach_http(error, &connection.http));
    }
    let http = connection.http.clone();
    let messages = connection.messages();
    let frames = channel_frames(connection, messages, driver, interceptor, create);
    Ok(ChannelExecution { frames, http, complete: Box::pin(async {}) })
}

struct DirectState {
    connection: WebSocketConnection,
    messages: MessageStream,
    driver: Box<dyn ChannelDriver>,
    interceptor: Option<Arc<dyn WebSocketInterceptor>>,
    create: ChannelCreate,
    observed: bool,
}

fn channel_frames(
    connection: WebSocketConnection,
    messages: MessageStream,
    driver: Box<dyn ChannelDriver>,
    interceptor: Option<Arc<dyn WebSocketInterceptor>>,
    create: ChannelCreate,
) -> FrameStream {
    let state = DirectState { connection, messages, driver, interceptor, create, observed: false };
    Box::pin(stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        let frame = match state.messages.next().await {
            Some(Ok(message)) => {
                state.observed = true;
                match &state.interceptor {
                    Some(interceptor) => interceptor.receive(message_text(message)),
                    None => message_text(message),
                }
            }
            Some(Err(error)) => {
                let error = attach_http(annotate_receive(error, state.observed), &state.connection.http);
                state.connection.close().await;
                return Some((Err(error), None));
            }
            None => {
                state.connection.close().await;
                return None;
            }
        };
        match state.driver.observe(&state.create, &frame) {
            Err(error) => {
                let error = attach_http(with_body(error, &frame), &state.connection.http);
                state.connection.close().await;
                Some((Err(error), None))
            }
            Ok(ChannelObservation::Frame { frame }) => Some((Ok(frame), Some(state))),
            Ok(ChannelObservation::Completed { frame, .. }) | Ok(ChannelObservation::Incomplete { frame }) => {
                state.connection.close().await;
                Some((Ok(frame), None))
            }
            Ok(ChannelObservation::ProviderFailure { error }) | Ok(ChannelObservation::Rejected { error, .. }) => {
                let error = attach_http(with_body(error, &frame), &state.connection.http);
                state.connection.close().await;
                Some((Err(error), None))
            }
        }
    }))
}

fn read_stream(
    source: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    url: String,
    sink: Arc<Mutex<SocketSink>>,
    closed: Arc<AtomicBool>,
) -> MessageStream {
    Box::pin(stream::unfold(Some(source), move |state| {
        let url = url.clone();
        let sink = sink.clone();
        let closed = closed.clone();
        async move {
            let mut source = state?;
            loop {
                let message = match source.next().await {
                    Some(Ok(tungstenite::Message::Text(text))) => Message::Text(text.to_string()),
                    Some(Ok(tungstenite::Message::Binary(bytes))) => Message::Binary(bytes.to_vec()),
                    Some(Ok(
                        tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) | tungstenite::Message::Frame(_),
                    )) => continue,
                    Some(Ok(tungstenite::Message::Close(frame))) => {
                        let (code, body) = match frame {
                            Some(frame) => (u16::from(frame.code), Some(frame.reason.to_string())),
                            None => (1005, None),
                        };
                        return Some((Err(closed_error(&url, code, body)), None));
                    }
                    Some(Err(_)) | None => return Some((Err(closed_error(&url, 1006, None)), None)),
                };
                if let Some(error) = oversized_error(&message, &url) {
                    close_sink(&sink, &closed, 1009, "Message too large").await;
                    return Some((Err(error), None));
                }
                return Some((Ok(message), Some(source)));
            }
        }
    }))
}

async fn close_sink(sink: &Mutex<SocketSink>, closed: &AtomicBool, code: u16, reason: &str) {
    if closed.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut sink = sink.lock().await;
    let _ = sink
        .send(tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame {
            code: tungstenite::protocol::frame::coding::CloseCode::from(code),
            reason: reason.into(),
        })))
        .await;
}

fn oversized_error(message: &Message, url: &str) -> Option<AiError> {
    let (length, body) = match message {
        Message::Text(text) => (text.len(), text.clone()),
        Message::Binary(bytes) => (bytes.len(), String::from_utf8_lossy(bytes).into_owned()),
    };
    if length <= MAX_FRAME_BYTES {
        return None;
    }
    Some(websocket_error(
        "WebSocket message exceeds the 16 MiB limit".to_string(),
        Some(url.to_string()),
        TransportOperation::Read,
        Some("message-too-large".to_string()),
        Some(TransportPhase::Receive),
        None,
        Some(body),
    ))
}

fn websocket_error(
    message: String,
    url: Option<String>,
    operation: TransportOperation,
    code: Option<String>,
    phase: Option<TransportPhase>,
    delivery: Option<Delivery>,
    body: Option<String>,
) -> AiError {
    AiError {
        reason: AiErrorReason::Transport {
            message,
            body,
            http: None,
            transport: TransportKind::WebSocket,
            operation,
            code,
            url,
            phase,
            delivery,
            recovery: None,
        },
    }
}

fn invalid_url_error(message: impl Into<String>, value: &str) -> AiError {
    websocket_error(
        message.into(),
        Some(value.to_string()),
        TransportOperation::Request,
        Some("invalid-url".to_string()),
        Some(TransportPhase::Prepare),
        Some(Delivery::NotSent),
        None,
    )
}

fn construct_error(url: &str, error: impl std::fmt::Display) -> AiError {
    websocket_error(
        format!("Failed to construct WebSocket: {error}"),
        Some(url.to_string()),
        TransportOperation::Request,
        None,
        Some(TransportPhase::Connect),
        Some(Delivery::NotSent),
        None,
    )
}

fn connect_error(url: &str, error: impl std::fmt::Display) -> AiError {
    websocket_error(
        format!("Failed to open WebSocket: {error}"),
        Some(url.to_string()),
        TransportOperation::Request,
        None,
        Some(TransportPhase::Connect),
        Some(Delivery::NotSent),
        None,
    )
}

fn closed_error(url: &str, code: u16, body: Option<String>) -> AiError {
    websocket_error(
        format!("WebSocket closed with code {code}"),
        Some(url.to_string()),
        TransportOperation::Read,
        Some(code.to_string()),
        Some(TransportPhase::Close),
        None,
        body,
    )
}

fn not_open_error(url: &str) -> AiError {
    websocket_error(
        "WebSocket is not open".to_string(),
        Some(url.to_string()),
        TransportOperation::Write,
        None,
        Some(TransportPhase::Send),
        Some(Delivery::NotSent),
        None,
    )
}

fn annotate(error: AiError, phase: TransportPhase, delivery: Delivery) -> AiError {
    let mut error = error;
    if let AiErrorReason::Transport { phase: current, delivery: slot, .. } = &mut error.reason {
        *current = Some(phase);
        *slot = Some(delivery);
    }
    error
}

fn annotate_receive(error: AiError, observed: bool) -> AiError {
    let mut error = error;
    if let AiErrorReason::Transport { phase, delivery, .. } = &mut error.reason {
        let next = if phase.as_ref() == Some(&TransportPhase::Close) {
            TransportPhase::Close
        } else {
            TransportPhase::Receive
        };
        *phase = Some(next);
        *delivery = Some(if observed { Delivery::Accepted } else { Delivery::Ambiguous });
    }
    error
}

fn attach_http(error: AiError, http: &Option<HttpContext>) -> AiError {
    match http {
        Some(http) => error.attach_http(http),
        None => error,
    }
}

fn with_body(error: AiError, body: &str) -> AiError {
    let mut error = error;
    let slot = match &mut error.reason {
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
        | AiErrorReason::Timeout { body, .. } => body,
    };
    *slot = Some(body.to_string());
    error
}
