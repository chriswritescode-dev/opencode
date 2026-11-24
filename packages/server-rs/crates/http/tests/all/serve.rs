use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1 as client_http1;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::{UNAUTHORIZED_BODY, ready_app};

const REQUEST: &str = "GET /api/info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn reaps_finished_connections_and_stops_on_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let app = ready_app();
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    for _ in 0..4 {
        let mut stream = TcpStream::connect(address).await.expect("a connection");
        stream.write_all(REQUEST.as_bytes()).await.expect("a request");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("a response");
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
        assert!(response.contains(UNAUTHORIZED_BODY), "{response}");
    }
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

#[tokio::test]
async fn a_post_body_is_collected_on_the_real_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let app = ready_app();
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    let body = r#"{"body":1}"#;
    let request = format!(
        "POST /api/pair HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        crate::encoded("opencode", "pw"),
        body.len(),
    );
    let mut stream = TcpStream::connect(address).await.expect("a connection");
    stream.write_all(request.as_bytes()).await.expect("a request");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("a response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"code\""), "{response}");
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

#[tokio::test]
async fn a_declared_body_above_the_cap_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let app = ready_app();
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    let request = format!(
        "POST /api/pair HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {}\r\nContent-Length: 134217729\r\nConnection: close\r\n\r\n{{}}",
        crate::encoded("opencode", "pw"),
    );
    let mut stream = TcpStream::connect(address).await.expect("a connection");
    stream.write_all(request.as_bytes()).await.expect("a request");
    stream.shutdown().await.expect("a shutdown");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("a response");
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

#[tokio::test]
async fn an_unauthorized_request_is_answered_before_its_body_is_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let app = ready_app();
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    let request = "POST /api/pair HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n";
    let mut stream = TcpStream::connect(address).await.expect("a connection");
    stream.write_all(request.as_bytes()).await.expect("a request");
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
        .await
        .expect("a response without the body")
        .expect("a response");
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(response.contains(UNAUTHORIZED_BODY), "{response}");
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

#[tokio::test]
async fn erroring_stream_aborts_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("a connection");
        let service = service_fn(|_request| async {
            let body = futures_util::stream::unfold(0u8, |state| async move {
                match state {
                    0 => Some((Ok::<_, std::io::Error>(Bytes::from_static(b"data: one\n\n")), 1)),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Some((Err(std::io::Error::other("stream failed")), 2))
                    }
                    _ => None,
                }
            });
            Ok::<_, Infallible>(opencode_http::response::stream(StatusCode::OK, "text/event-stream", &[], body))
        });
        let _ = server_http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
    });
    let stream = TcpStream::connect(address).await.expect("a connection");
    let (mut sender, connection) = client_http1::handshake(TokioIo::new(stream)).await.expect("a handshake");
    tokio::spawn(connection);
    let request =
        Request::builder().uri("/").header("host", "localhost").body(Empty::<Bytes>::new()).expect("a request");
    let response = sender.send_request(request).await.expect("a response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.into_body().collect().await.is_err(), "an erroring stream must abort the response");
    server.await.expect("the server task");
}
