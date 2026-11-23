use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use opencode_ai::clock::{Clock, FixedClock};
use opencode_ai::route::executor::{
    BoxFuture, ByteStream, HttpResponse, HttpSender, Middleware, Next, PreparedHttp, RequestExecutor, ReqwestSender,
};
use opencode_ai::schema::{AiError, AiErrorReason, HttpTimeout, RequestTimeout, TimeoutOptions, TransportOperation};

const FIXED_EPOCH_MS: u64 = 1_767_323_045_000;

struct FnSender(Arc<dyn Fn(PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> + Send + Sync>);

impl FnSender {
    fn new(send: impl Fn(PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }
}

impl HttpSender for FnSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        (self.0)(request)
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::new(FIXED_EPOCH_MS))
}

fn executor(sender: FnSender) -> RequestExecutor {
    RequestExecutor::new(Arc::new(sender), None, clock())
}

fn request(url: &str) -> PreparedHttp {
    PreparedHttp::new("POST", url, Vec::new(), Bytes::from_static(b"{}"))
}

fn response(status: u16, headers: Vec<(&str, &str)>, chunks: Vec<&'static [u8]>) -> HttpResponse {
    let headers = headers.into_iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
    let body: ByteStream = Box::pin(stream::iter(chunks.into_iter().map(|chunk| Ok(Bytes::from_static(chunk)))));
    HttpResponse::new(status, headers, body)
}

fn millis(value: u64) -> HttpTimeout {
    HttpTimeout::Millis(serde_json::Number::from(value))
}

fn headers_only(value: u64) -> RequestTimeout {
    RequestTimeout::Config(TimeoutOptions { total: None, headers: Some(millis(value)), chunk: None })
}

fn chunk_only(value: u64) -> RequestTimeout {
    RequestTimeout::Config(TimeoutOptions { total: None, headers: None, chunk: Some(millis(value)) })
}

fn total_only(value: u64) -> RequestTimeout {
    RequestTimeout::Timeout(millis(value))
}

fn recording_middleware(name: &'static str, events: Arc<Mutex<Vec<String>>>) -> Middleware {
    Arc::new(move |mut request: PreparedHttp, next: Next| {
        let entered = events.clone();
        let exited = events.clone();
        Box::pin(async move {
            entered.lock().unwrap().push(format!("{name}:in"));
            request.headers.push((format!("x-{name}"), "1".to_string()));
            let response = next(request).await?;
            exited.lock().unwrap().push(format!("{name}:out"));
            Ok(response)
        })
    })
}

#[tokio::test]
async fn middleware_composes_outermost_first_and_forwards_the_request() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(None));
    let sender = FnSender::new({
        let seen = seen.clone();
        move |request| {
            *seen.lock().unwrap() = Some(request.clone());
            Box::pin(async { Ok(response(200, Vec::new(), vec![b"body"])) })
        }
    });
    let executor =
        RequestExecutor::new(Arc::new(sender), Some(recording_middleware("executor", events.clone())), clock())
            .with_middleware(recording_middleware("scoped", events.clone()));
    let response = executor
        .execute(request("https://api.example.test/original"), Some(recording_middleware("per-call", events.clone())))
        .await
        .expect("response");
    assert_eq!(
        *events.lock().unwrap(),
        vec!["executor:in", "scoped:in", "per-call:in", "per-call:out", "scoped:out", "executor:out"]
    );
    let seen = seen.lock().unwrap().take().expect("sender was called");
    assert_eq!(seen.url, "https://api.example.test/original");
    for name in ["executor", "scoped", "per-call"] {
        assert!(seen.headers.iter().any(|(header, _)| header == &format!("x-{name}")), "sender did not see x-{name}");
    }
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn middleware_can_replace_the_response_without_reaching_the_sender() {
    let calls = Arc::new(Mutex::new(0usize));
    let sender = FnSender::new({
        let calls = calls.clone();
        move |_| {
            *calls.lock().unwrap() += 1;
            Box::pin(async { Ok(response(200, Vec::new(), vec![b"original"])) })
        }
    });
    let replace: Middleware = Arc::new(|_request, _next| {
        Box::pin(async move { Ok(response(299, vec![("x-replaced", "1")], vec![b"replaced"])) })
    });
    let response =
        executor(sender).execute(request("https://api.example.test/original"), Some(replace)).await.expect("response");
    assert_eq!(response.status, 299);
    assert!(response.headers.iter().any(|(name, value)| name == "x-replaced" && value == "1"));
    let mut body = response.body;
    let chunk = body.next().await.expect("body chunk").expect("body chunk ok");
    assert_eq!(&chunk[..], b"replaced");
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn headers_timeout_is_a_transport_timeout() {
    let sender = FnSender::new(|_| Box::pin(std::future::pending()));
    let error = match executor(sender)
        .with_timeout(Some(headers_only(100)))
        .execute(request("https://api.example.test/timeout"), None)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("expected a headers timeout"),
    };
    match error.reason {
        AiErrorReason::Transport { message, operation, code, url, .. } => {
            assert_eq!(message, "Timed out waiting for response headers");
            assert_eq!(operation, TransportOperation::Request);
            assert_eq!(code.as_deref(), Some("Timeout"));
            assert_eq!(url.as_deref(), Some("https://api.example.test/timeout"));
        }
        other => panic!("expected transport timeout, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn chunk_idle_timeout_is_a_transport_timeout() {
    let sender = FnSender::new(|_| {
        Box::pin(async {
            let body: ByteStream = Box::pin(stream::pending());
            Ok(HttpResponse::new(200, Vec::new(), body))
        })
    });
    let response = executor(sender)
        .with_timeout(Some(chunk_only(100)))
        .execute(request("https://api.example.test/chunk"), None)
        .await
        .expect("response");
    let mut body = response.body;
    let error = match body.next().await {
        Some(Err(error)) => error,
        _ => panic!("expected a chunk timeout"),
    };
    match error.reason {
        AiErrorReason::Transport { message, operation, http, .. } => {
            assert_eq!(message, "Timed out waiting for response data");
            assert_eq!(operation, TransportOperation::Read);
            assert!(http.is_some(), "chunk timeout carries the response context");
        }
        other => panic!("expected transport timeout, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn total_timeout_is_a_transport_timeout() {
    let sender = FnSender::new(|_| {
        Box::pin(async {
            let body: ByteStream = Box::pin(stream::pending());
            Ok(HttpResponse::new(200, Vec::new(), body))
        })
    });
    let response = executor(sender)
        .with_timeout(Some(total_only(100)))
        .execute(request("https://api.example.test/total"), None)
        .await
        .expect("response");
    let mut body = response.body;
    let error = match body.next().await {
        Some(Err(error)) => error,
        _ => panic!("expected a total timeout"),
    };
    match error.reason {
        AiErrorReason::Transport { message, operation, .. } => {
            assert_eq!(message, "Timed out waiting for the response to complete");
            assert_eq!(operation, TransportOperation::Read);
        }
        other => panic!("expected transport timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn status_errors_classify_rate_limits() {
    let sender = FnSender::new(|_| {
        Box::pin(async { Ok(response(429, vec![("Retry-After", "2")], vec![b"rate limit exceeded"])) })
    });
    let error = match executor(sender).execute(request("https://api.example.test/rate-limited"), None).await {
        Err(error) => error,
        Ok(_) => panic!("expected a rate limit error"),
    };
    match error.reason {
        AiErrorReason::RateLimit { message, retry_after_ms, rate_limit, .. } => {
            assert_eq!(message, "Provider request failed with HTTP 429: rate limit exceeded");
            assert_eq!(retry_after_ms, Some(2000));
            assert_eq!(rate_limit.and_then(|details| details.retry_after_ms), Some(2000));
        }
        other => panic!("expected rate limit, got {other:?}"),
    }
}

#[tokio::test]
async fn reqwest_sender_streams_chunked_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n5\r\nhello\r\n",
            )
            .await
            .expect("write first chunk");
        socket.flush().await.expect("flush first chunk");
        let _ = wait.await;
        socket.write_all(b"6\r\n world\r\n0\r\n\r\n").await.expect("write rest");
        socket.flush().await.expect("flush rest");
    });
    let executor = RequestExecutor::new(Arc::new(ReqwestSender::new()), None, clock());
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let response = executor.execute(request(&format!("http://{address}/stream")), None).await.expect("response");
        assert_eq!(response.status, 200);
        let mut body = response.body;
        let first = body.next().await.expect("first chunk").expect("first chunk ok");
        assert_eq!(&first[..], b"hello");
        release.send(()).expect("release the rest of the body");
        let mut rest = Vec::new();
        while let Some(chunk) = body.next().await {
            rest.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(rest, b" world");
    })
    .await;
    assert!(result.is_ok(), "streaming a chunked body timed out");
    server.await.expect("server task");
}
