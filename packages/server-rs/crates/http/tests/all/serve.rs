use std::time::Duration;

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
