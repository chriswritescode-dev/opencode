use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

pub(crate) type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) struct Live {
    pub(crate) port: u16,
    pub(crate) persistent_root: std::path::PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn live(directory: &Path) -> Live {
    live_with(directory, &crate::temp_directory("pty-home"), environment()).await
}

pub(crate) async fn live_with(directory: &Path, home: &Path, environment: Vec<(String, String)>) -> Live {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let database = Arc::new(database);
    let paths = opencode_config::paths::GlobalPaths::from_vars(|name| {
        (name == "HOME").then(|| home.to_owned().into_os_string())
    })
    .expect("global paths")
    .acquire()
    .expect("acquire global paths");
    let persistent_root = paths.state.join("pty");
    let bus = opencode_bus::Bus::new(database.clone(), opencode_bus::BusOptions::default());
    let services = Arc::new(GlobalServices::new(
        database,
        paths,
        directory.to_string_lossy().into_owned(),
        Arc::new(|_: &str| None),
        environment,
        bus,
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
    ));
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: directory.to_path_buf(),
        hostname: None,
        urls: Arc::new(Vec::new),
        state: None,
        services: Some(services),
    });
    app.status().ready();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a listener");
    let port = listener.local_addr().expect("a local address").port();
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    Live { port, persistent_root, shutdown: Some(shutdown), task }
}

pub(crate) fn environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn directory_query(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("location%5Bdirectory%5D={encoded}")
}

pub(crate) fn json(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes).expect("a JSON body")
}

pub(crate) async fn send(
    live: &Live,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, Bytes, JoinHandle<()>) {
    let stream = TcpStream::connect(("127.0.0.1", live.port)).await.expect("a connection");
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.expect("a handshake");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .method(method)
        .uri(target)
        .header("host", "127.0.0.1")
        .header("authorization", crate::basic("opencode", "pw"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let payload = body.map(|body| Bytes::from(body.to_owned())).unwrap_or_default();
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder.body(Full::new(payload)).expect("a request");
    let response: http::Response<Incoming> = sender.send_request(request).await.expect("a response");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    (status, bytes, task)
}

async fn create(live: &Live, directory: &Path, command: &[&str]) -> (String, u64) {
    let body = serde_json::json!({
        "command": "/bin/sh",
        "args": command,
        "cwd": directory.to_string_lossy(),
    });
    let (status, bytes, _) =
        send(live, "POST", &format!("/api/pty?{}", directory_query(directory)), &[], Some(&body.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let id = body["data"]["id"].as_str().expect("a pty id").to_owned();
    let pid = body["data"]["pid"].as_f64().expect("a pid") as u64;
    (id, pid)
}

async fn token(live: &Live, directory: &Path, id: &str) -> String {
    let (status, bytes, _) = send(
        live,
        "POST",
        &format!("/api/pty/{id}/connect-token?{}", directory_query(directory)),
        &[("x-opencode-ticket", "1")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json(&bytes)["data"]["ticket"].as_str().expect("a ticket").to_owned()
}

async fn handshake(
    live: &Live,
    directory: &Path,
    id: &str,
    ticket: &str,
) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
    let url =
        format!("ws://127.0.0.1:{}/api/pty/{id}/connect?{}&ticket={ticket}", live.port, directory_query(directory));
    let (socket, response) = connect_async(url).await?;
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    Ok(socket)
}

async fn open(live: &Live, directory: &Path, id: &str) -> (Connection, SocketState) {
    let ticket = token(live, directory, id).await;
    let socket = handshake(live, directory, id, &ticket).await.expect("a websocket handshake");
    let (sink, stream) = socket.split();
    let (events, receiver) = mpsc::unbounded_channel();
    tokio::spawn(read_socket(stream, events));
    (Connection { sink, events: receiver }, SocketState::default())
}

pub(crate) async fn read_socket(mut stream: SplitStream<Client>, events: mpsc::UnboundedSender<Event>) {
    while let Some(item) = stream.next().await {
        match item {
            Ok(Message::Text(text)) => {
                if events.send(Event::Text(text.to_string())).is_err() {
                    return;
                }
            }
            Ok(Message::Binary(data)) => {
                if events.send(Event::Binary(data.to_vec())).is_err() {
                    return;
                }
            }
            Ok(Message::Close(frame)) => {
                let (code, reason) =
                    frame.map(|frame| (u16::from(frame.code), frame.reason.to_string())).unwrap_or((0, String::new()));
                let _ = events.send(Event::Closed(code, reason));
                return;
            }
            Ok(_) => {}
            Err(_) => {
                let _ = events.send(Event::Closed(0, String::new()));
                return;
            }
        }
    }
    let _ = events.send(Event::Closed(0, String::new()));
}

pub(crate) struct Connection {
    pub(crate) sink: SplitSink<Client, Message>,
    pub(crate) events: mpsc::UnboundedReceiver<Event>,
}

impl Connection {
    pub(crate) async fn text(&mut self, text: &str) {
        self.sink.send(Message::Text(text.into())).await.expect("send text");
    }

    pub(crate) async fn binary(&mut self, data: &[u8]) {
        self.sink.send(Message::Binary(data.to_vec().into())).await.expect("send binary");
    }

    pub(crate) async fn close(&mut self) {
        let _ = self.sink.close().await;
    }
}

pub(crate) enum Event {
    Text(String),
    Binary(Vec<u8>),
    Closed(u16, String),
}

#[derive(Default)]
pub(crate) struct SocketState {
    pub(crate) output: String,
    pub(crate) frames: Vec<&'static str>,
    pub(crate) closed: bool,
    pub(crate) code: u16,
    pub(crate) reason: String,
}

impl SocketState {
    fn apply(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                self.output.push_str(&text);
                self.frames.push("output");
            }
            Event::Binary(data) => {
                if data.first() == Some(&0) {
                    self.frames.push("cursor");
                } else if let Ok(text) = String::from_utf8(data) {
                    self.output.push_str(&text);
                    self.frames.push("output");
                }
            }
            Event::Closed(code, reason) => {
                self.closed = true;
                self.code = code;
                self.reason = reason;
            }
        }
    }
}

pub(crate) async fn wait(
    connection: &mut Connection,
    state: &mut SocketState,
    predicate: impl Fn(&SocketState) -> bool,
) {
    for _ in 0..500 {
        if predicate(state) {
            return;
        }
        match tokio::time::timeout(Duration::from_millis(20), connection.events.recv()).await {
            Ok(Some(event)) => state.apply(event),
            Ok(None) => panic!(
                "the socket event stream ended unexpectedly: output={:?} frames={:?} closed={} code={} reason={:?}",
                state.output, state.frames, state.closed, state.code, state.reason
            ),
            Err(_) => {}
        }
    }
    panic!(
        "the PTY socket did not reach the expected state: output={:?} frames={:?} closed={} code={} reason={:?}",
        state.output, state.frames, state.closed, state.code, state.reason
    );
}

fn process_alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn pty_websocket_preserves_replay_text_binary_resize_reconnect_and_exit() {
    let directory = crate::temp_directory("pty-transport");
    let live = live(&directory).await;
    let script = r#"stty -echo; printf 'ready\n'; while IFS= read -r line; do case "$line" in size) stty size;; quit) exit 0;; *) printf 'reply:%s\n' "$line";; esac; done"#;
    let (id, _pid) = create(&live, &directory, &["-c", script]).await;

    let (mut first, mut first_state) = open(&live, &directory, &id).await;
    wait(&mut first, &mut first_state, |state| state.frames.contains(&"cursor") || state.closed).await;
    assert!(!first_state.closed);
    wait(&mut first, &mut first_state, |state| state.output.contains("ready")).await;

    first.text("text\n").await;
    first.binary(b"binary\n").await;
    wait(&mut first, &mut first_state, |state| state.output.contains("reply:binary")).await;
    assert!(first_state.output.contains("reply:text"));

    let (status, _, _) = send(
        &live,
        "PUT",
        &format!("/api/pty/{id}?{}", directory_query(&directory)),
        &[],
        Some(r#"{"size":{"rows":31,"cols":93}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    first.text("size\n").await;
    wait(&mut first, &mut first_state, |state| state.output.contains("31 93")).await;

    first.close().await;
    wait(&mut first, &mut first_state, |state| state.closed).await;

    let (mut second, mut second_state) = open(&live, &directory, &id).await;
    wait(&mut second, &mut second_state, |state| state.frames.contains(&"cursor") || state.closed).await;
    assert!(!second_state.closed);
    assert!(second_state.output.contains("reply:text"));
    assert!(second_state.output.contains("reply:binary"));
    assert_eq!(second_state.frames.last(), Some(&"cursor"));

    second.text("reconnected\n").await;
    wait(&mut second, &mut second_state, |state| state.output.contains("reply:reconnected")).await;
    assert!(!first_state.output.contains("reply:reconnected"));

    second.text("quit\n").await;
    wait(&mut second, &mut second_state, |state| state.closed).await;
    assert_eq!(second_state.code, 1000);

    let (status, bytes, _) =
        send(&live, "GET", &format!("/api/pty/{id}?{}", directory_query(&directory)), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["data"]["status"], "exited");

    let (mut exited, mut exited_state) = open(&live, &directory, &id).await;
    wait(&mut exited, &mut exited_state, |state| state.closed).await;
    assert_eq!(exited_state.code, 4404);
    assert_eq!(exited_state.reason, "session exited");
    assert!(exited_state.frames.is_empty());
}

#[tokio::test]
async fn pty_connect_tickets_are_single_use_and_scoped() {
    let directory = crate::temp_directory("pty-tickets");
    let live = live(&directory).await;
    let (first, _) = create(&live, &directory, &["-c", "sleep 30"]).await;
    let (second, _) = create(&live, &directory, &["-c", "sleep 30"]).await;

    let ticket = token(&live, &directory, &first).await;
    let socket = handshake(&live, &directory, &first, &ticket).await.expect("a websocket handshake");
    drop(socket);

    forbidden(handshake(&live, &directory, &first, &ticket).await);

    let scoped = token(&live, &directory, &first).await;
    forbidden(handshake(&live, &directory, &second, &scoped).await);
}

fn forbidden(result: Result<Client, tokio_tungstenite::tungstenite::Error>) {
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        _ => panic!("expected the connection to be forbidden"),
    }
}

#[tokio::test]
async fn pty_connect_to_an_unknown_session_is_an_empty_404() {
    let directory = crate::temp_directory("pty-missing");
    let live = live(&directory).await;
    let (status, body, _) =
        send(&live, "GET", &format!("/api/pty/pty_unknown/connect?{}", directory_query(&directory)), &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn pty_websocket_closes_with_1012_when_its_location_reloads() {
    let directory = crate::temp_directory("pty-reload");
    let live = live(&directory).await;
    let (id, pid) = create(&live, &directory, &["-c", "sleep 30"]).await;

    let (mut connection, mut state) = open(&live, &directory, &id).await;
    let (status, _, _) = send(&live, "POST", "/api/location/reload", &[], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    wait(&mut connection, &mut state, |state| state.code == 1012).await;
    assert_eq!(state.code, 1012);

    for _ in 0..200 {
        if !process_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!process_alive(pid), "the reloaded location left the shell running");

    for _ in 0..200 {
        let (status, body, _) =
            send(&live, "GET", &format!("/api/pty/{id}?{}", directory_query(&directory)), &[], None).await;
        if status == StatusCode::NOT_FOUND && json(&body)["_tag"] == "PtyNotFoundError" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the reloaded location still lists the old PTY");
}
