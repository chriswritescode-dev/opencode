use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use http::StatusCode;
use hyper::body::Incoming;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use opencode_http::response::{self, Response};
use opencode_http::websocket;

async fn echo(mut request: http::Request<Incoming>) -> Response {
    let Some(on_upgrade) = websocket::upgrade(&mut request) else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    websocket::accept(request.headers(), on_upgrade, |mut socket| async move {
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Text(text) if text.as_str() == "close" => {
                    let _ = websocket::close(&mut socket, 4404, "session not found").await;
                    break;
                }
                Message::Text(text) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Message::Binary(data) => {
                    if socket.send(Message::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
}

#[tokio::test]
async fn echoes_text_and_binary_and_closes_with_the_handler_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve_with(
        listener,
        async move {
            let _ = receiver.await;
        },
        echo,
    ));
    let (mut socket, response) = connect_async(format!("ws://{address}/echo")).await.expect("a handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket.send(Message::Text("hello".into())).await.expect("send text");
    assert_eq!(next_text(&mut socket).await, "hello");
    socket.send(Message::Binary(vec![1, 2, 3].into())).await.expect("send binary");
    assert_eq!(next_binary(&mut socket).await, vec![1, 2, 3]);
    socket.send(Message::Text("close".into())).await.expect("send close");
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::from(4404));
    assert_eq!(close.reason.as_str(), "session not found");
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

#[tokio::test]
async fn a_handshake_without_a_key_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a listener");
    let address = listener.local_addr().expect("a local address");
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve_with(
        listener,
        async move {
            let _ = receiver.await;
        },
        echo,
    ));
    let mut stream = TcpStream::connect(address).await.expect("a connection");
    let request = "GET /echo HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(request.as_bytes()).await.expect("a request");
    let mut received = Vec::new();
    let mut buffer = [0u8; 1024];
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await.expect("a response");
        if read == 0 {
            break;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    let response = String::from_utf8_lossy(&received);
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    drop(stream);
    sender.send(()).expect("a shutdown");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve to stop").expect("a task");
}

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn next(socket: &mut Client) -> Message {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("a frame")
        .expect("a message")
        .expect("a frame")
}

async fn next_text(socket: &mut Client) -> String {
    match next(socket).await {
        Message::Text(text) => text.to_string(),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

async fn next_binary(socket: &mut Client) -> Vec<u8> {
    match next(socket).await {
        Message::Binary(data) => data.to_vec(),
        other => panic!("expected a binary frame, got {other:?}"),
    }
}

async fn next_close(socket: &mut Client) -> CloseFrame {
    match next(socket).await {
        Message::Close(Some(frame)) => frame,
        other => panic!("expected a close frame, got {other:?}"),
    }
}
