use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::{WebSocketStream, accept_async, accept_hdr_async};

use opencode_ai::route::framing::FrameStream;
use opencode_ai::route::transport::channel::{
    ChannelCheckpoint, ChannelCreate, ChannelDriver, ChannelExchange, ChannelFallback, ChannelMode, ChannelObservation,
};
use opencode_ai::route::transport::websocket::{
    MAX_FRAME_BYTES, SystemConnector, WebSocketInterceptor, WebSocketRequest, make_direct, open, to_websocket_url,
};
use opencode_ai::schema::{AiError, AiErrorReason, Delivery, TransportKind, TransportOperation, TransportPhase};

#[derive(Clone, Copy)]
enum ScriptedOutcome {
    Complete,
    Continue,
}

struct ScriptedDriver {
    message: String,
    outcome: ScriptedOutcome,
    observed: Arc<Mutex<Vec<String>>>,
}

impl ScriptedDriver {
    fn new(message: &str, outcome: ScriptedOutcome) -> Self {
        Self { message: message.to_string(), outcome, observed: Arc::new(Mutex::new(Vec::new())) }
    }

    fn observed(&self) -> Arc<Mutex<Vec<String>>> {
        self.observed.clone()
    }
}

impl ChannelDriver for ScriptedDriver {
    fn create(&mut self, _checkpoint: Option<&ChannelCheckpoint>) -> Result<ChannelCreate, AiError> {
        Ok(ChannelCreate { message: self.message.clone(), mode: ChannelMode::Full })
    }

    fn observe(&mut self, _create: &ChannelCreate, frame: &str) -> Result<ChannelObservation, AiError> {
        self.observed.lock().unwrap().push(frame.to_string());
        match self.outcome {
            ScriptedOutcome::Complete => {
                Ok(ChannelObservation::Completed { frame: frame.to_string(), checkpoint: None })
            }
            ScriptedOutcome::Continue => Ok(ChannelObservation::Frame { frame: frame.to_string() }),
        }
    }
}

struct TestInterceptor;

impl WebSocketInterceptor for TestInterceptor {
    fn handshake(&self, mut connect: WebSocketRequest) -> WebSocketRequest {
        connect.headers.retain(|(name, _)| !name.eq_ignore_ascii_case("x-test"));
        connect.headers.push(("x-test".to_string(), "rewritten".to_string()));
        connect
    }

    fn send(&self, frame: String) -> String {
        format!("{frame}!")
    }

    fn receive(&self, frame: String) -> String {
        frame.to_uppercase()
    }
}

struct TransportReason {
    message: String,
    code: Option<String>,
    phase: Option<TransportPhase>,
    delivery: Option<Delivery>,
    operation: TransportOperation,
    transport: TransportKind,
}

fn transport_reason(error: AiError) -> TransportReason {
    match error.reason {
        AiErrorReason::Transport { message, code, phase, delivery, operation, transport, .. } => {
            TransportReason { message, code, phase, delivery, operation, transport }
        }
        other => panic!("expected a transport error, got {other:?}"),
    }
}

fn exchange(url: String, headers: Vec<(String, String)>, driver: ScriptedDriver) -> ChannelExchange {
    let fallback: ChannelFallback = Arc::new(|| Box::pin(async { panic!("fallback is not used") }));
    ChannelExchange {
        id: "test".to_string(),
        connect: WebSocketRequest { url, headers },
        rotate_after_ms: None,
        fallback,
        driver: Box::new(driver),
    }
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future).await.expect("timed out waiting for the exchange")
}

async fn collect_frames(frames: FrameStream) -> Vec<Result<String, AiError>> {
    frames.collect().await
}

async fn accept(listener: TcpListener) -> WebSocketStream<TcpStream> {
    let (stream, _) = listener.accept().await.expect("accept");
    accept_async(stream).await.expect("websocket handshake")
}

#[test]
fn to_websocket_url_maps_http_schemes_and_rejects_others() {
    assert_eq!(
        to_websocket_url("https://api.openai.com/v1/responses?a=1").expect("https url"),
        "wss://api.openai.com/v1/responses?a=1"
    );
    assert_eq!(to_websocket_url("http://127.0.0.1:8080/chat").expect("http url"), "ws://127.0.0.1:8080/chat");
    for (url, message) in
        [("ftp://example.com/x", "Unsupported WebSocket URL protocol ftp:"), ("not a url", "Invalid URL")]
    {
        let reason = transport_reason(to_websocket_url(url).expect_err("unsupported url"));
        assert_eq!(reason.message, message);
        assert_eq!(reason.code.as_deref(), Some("invalid-url"));
        assert_eq!(reason.phase, Some(TransportPhase::Prepare));
        assert_eq!(reason.delivery, Some(Delivery::NotSent));
        assert_eq!(reason.operation, TransportOperation::Request);
        assert_eq!(reason.transport, TransportKind::WebSocket);
    }
}

#[tokio::test]
async fn oversized_message_fails_the_stream_and_closes_with_1009() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let mut socket = accept(listener).await;
        socket.send(Message::Text("x".repeat(MAX_FRAME_BYTES + 1).into())).await.expect("send oversized frame");
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|frame| u16::from(frame.code)),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return None,
            }
        }
    });
    let mut connection =
        open(WebSocketRequest { url: format!("ws://{address}"), headers: Vec::new() }).await.expect("open connection");
    let mut messages = connection.messages();
    let error = match within(messages.next()).await {
        Some(Err(error)) => error,
        other => panic!("expected an oversized message error, got {other:?}"),
    };
    let reason = transport_reason(error);
    assert_eq!(reason.code.as_deref(), Some("message-too-large"));
    assert_eq!(reason.phase, Some(TransportPhase::Receive));
    assert_eq!(reason.operation, TransportOperation::Read);
    assert_eq!(reason.transport, TransportKind::WebSocket);
    assert!(within(messages.next()).await.is_none(), "the message stream ends after the failed frame");
    let code = within(server).await.expect("server task");
    assert_eq!(code, Some(1009));
}

#[tokio::test]
async fn interceptor_rewrites_handshake_send_and_receive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let header = Arc::new(Mutex::new(None));
    let recorded = header.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
            *recorded.lock().unwrap() =
                request.headers().get("x-test").and_then(|value| value.to_str().ok()).map(str::to_string);
            Ok(response)
        })
        .await
        .expect("websocket handshake");
        let frame = match socket.next().await {
            Some(Ok(Message::Text(text))) => Some(text.to_string()),
            _ => None,
        };
        socket.send(Message::Text("world".into())).await.expect("send world");
        let _ = socket.next().await;
        frame
    });
    let driver = ScriptedDriver::new("hello", ScriptedOutcome::Complete);
    let observed = driver.observed();
    let executor = make_direct(Arc::new(SystemConnector), Some(Arc::new(TestInterceptor)));
    let execution = within(executor.execute(exchange(
        format!("ws://{address}"),
        vec![("x-test".to_string(), "original".to_string())],
        driver,
    )))
    .await
    .expect("execution");
    let frames = within(collect_frames(execution.frames)).await;
    assert_eq!(frames.len(), 1);
    let frame = match &frames[0] {
        Ok(frame) => frame,
        Err(error) => panic!("expected a completed frame, got {error:?}"),
    };
    assert_eq!(frame, "WORLD");
    within(execution.complete).await;
    let sent = within(server).await.expect("server task");
    assert_eq!(header.lock().unwrap().as_deref(), Some("rewritten"));
    assert_eq!(sent.as_deref(), Some("hello!"));
    assert_eq!(*observed.lock().unwrap(), vec!["WORLD".to_string()]);
}

#[tokio::test]
async fn closed_socket_classifies_delivery() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let _socket = accept(listener).await;
    });
    let executor = make_direct(Arc::new(SystemConnector), None);
    let execution = within(executor.execute(exchange(
        format!("ws://{address}"),
        Vec::new(),
        ScriptedDriver::new("hello", ScriptedOutcome::Continue),
    )))
    .await
    .expect("execution");
    let mut frames = execution.frames;
    let error = match within(frames.next()).await {
        Some(Err(error)) => error,
        other => panic!("expected an ambiguous delivery error, got {other:?}"),
    };
    let reason = transport_reason(error);
    assert_eq!(reason.delivery, Some(Delivery::Ambiguous));
    assert_eq!(reason.operation, TransportOperation::Read);
    within(server).await.expect("server task");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let mut socket = accept(listener).await;
        socket.send(Message::Text("one".into())).await.expect("send one frame");
    });
    let executor = make_direct(Arc::new(SystemConnector), None);
    let execution = within(executor.execute(exchange(
        format!("ws://{address}"),
        Vec::new(),
        ScriptedDriver::new("hello", ScriptedOutcome::Continue),
    )))
    .await
    .expect("execution");
    let mut frames = execution.frames;
    let frame = match within(frames.next()).await {
        Some(Ok(frame)) => frame,
        other => panic!("expected one frame before the close, got {other:?}"),
    };
    assert_eq!(frame, "one");
    let error = match within(frames.next()).await {
        Some(Err(error)) => error,
        other => panic!("expected an accepted delivery error, got {other:?}"),
    };
    let reason = transport_reason(error);
    assert_eq!(reason.delivery, Some(Delivery::Accepted));
    assert_eq!(reason.operation, TransportOperation::Read);
    within(server).await.expect("server task");
}
