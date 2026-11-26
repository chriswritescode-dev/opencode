use std::future::Future;

use futures_util::SinkExt;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::protocol::frame::Utf8Bytes;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message, handshake::derive_accept_key};

use crate::response::{self, Response};

pub type Socket = WebSocketStream<TokioIo<Upgraded>>;

const KEY: &str = "sec-websocket-key";
const VERSION: &str = "sec-websocket-version";

pub fn upgrade<B>(request: &mut http::Request<B>) -> Option<OnUpgrade> {
    let requested = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    requested.then(|| hyper::upgrade::on(request))
}

pub fn accept<H, F>(headers: &HeaderMap, on_upgrade: OnUpgrade, handler: H) -> Response
where
    H: FnOnce(Socket) -> F + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let Some(key) = headers.get(KEY).and_then(|value| value.to_str().ok()) else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    if headers.get(VERSION).and_then(|value| value.to_str().ok()) != Some("13") {
        return response::empty(StatusCode::BAD_REQUEST);
    }
    let Ok(accept) = HeaderValue::from_str(&derive_accept_key(key.as_bytes())) else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    let mut response = response::empty(StatusCode::SWITCHING_PROTOCOLS);
    response.headers_mut().insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    response.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    response.headers_mut().insert("sec-websocket-accept", accept);
    tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
            handler(socket).await;
        }
    });
    response
}

pub fn close_frame(code: u16, reason: impl Into<String>) -> CloseFrame {
    CloseFrame { code: CloseCode::from(code), reason: Utf8Bytes::from(reason.into()) }
}

pub async fn close(socket: &mut Socket, code: u16, reason: &str) -> Result<(), tungstenite::Error> {
    socket.send(Message::Close(Some(close_frame(code, reason)))).await
}
