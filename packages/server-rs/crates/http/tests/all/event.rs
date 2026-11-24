use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use opencode_bus::feed::{EventFeed, SUBSCRIBER_CAPACITY};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_http::app::{App, Options};
use opencode_http::state::ServerState;
use opencode_session::{ProjectionSettings, SessionProjector, SessionStore};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const CORPUS: &str = include_str!("../../../session/tests/corpus/events.json");

fn state(now: i64, heartbeat: Duration) -> Arc<ServerState> {
    let (db, _) = Database::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the database");
    let db = Arc::new(db);
    let bus = Bus::new(db.clone(), BusOptions { now: Some(Arc::new(move || now)), ..Default::default() });
    let usage = SessionProjector::start(&bus, &db, Arc::new(ProjectionSettings::default));
    let feed = EventFeed::new(&bus, SUBSCRIBER_CAPACITY);
    let store = SessionStore::new(db.clone());
    Arc::new(ServerState { db, bus, feed, store, usage, heartbeat })
}

struct Live {
    port: u16,
    state: Arc<ServerState>,
    task: JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn live(now: i64, heartbeat: Duration) -> Live {
    let state = state(now, heartbeat);
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp"),
        hostname: None,
        urls: Arc::new(Vec::new),
        state: Some(state.clone()),
    });
    app.status().ready();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a listener");
    let port = listener.local_addr().expect("a local address").port();
    let task = tokio::spawn(opencode_http::serve::serve(listener, app, std::future::pending::<()>()));
    Live { port, state, task }
}

async fn connect(live: &Live, target: &str, authorized: bool) -> (Response<Incoming>, JoinHandle<()>) {
    let stream = TcpStream::connect(("127.0.0.1", live.port)).await.expect("a connection");
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.expect("a handshake");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder().uri(target).header("host", "127.0.0.1");
    if authorized {
        builder = builder.header("authorization", crate::basic("opencode", "pw"));
    }
    let request = builder.body(Empty::<Bytes>::new()).expect("a request");
    let response = sender.send_request(request).await.expect("a response");
    (response, task)
}

async fn next_frame(body: &mut Incoming, buffer: &mut Vec<u8>) -> String {
    loop {
        if let Some(position) = buffer.windows(2).position(|pair| pair == b"\n\n") {
            let frame = buffer.drain(..position + 2).collect::<Vec<u8>>();
            return String::from_utf8(frame).expect("a utf-8 frame");
        }
        let frame = body.frame().await.expect("a body frame").expect("a frame");
        if let Some(data) = frame.data_ref() {
            buffer.extend_from_slice(data);
        }
    }
}

#[tokio::test]
async fn headers_and_first_frames_match_the_stream_contract() {
    let live = live(1000, Duration::from_secs(60)).await;
    let (response, _connection) = connect(&live, "/api/event", true).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    assert_eq!(header("content-type"), Some("text/event-stream"));
    assert_eq!(header("cache-control"), Some("no-cache, no-transform"));
    assert_eq!(header("x-accel-buffering"), Some("no"));
    assert_eq!(header("x-content-type-options"), Some("nosniff"));

    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let connected = next_frame(&mut body, &mut buffer).await;
    assert!(connected.contains("\"type\":\"server.connected\""), "{connected}");
    assert!(connected.contains("\"data\":{}"), "{connected}");
    assert!(connected.contains("\"id\":\"evt_"), "{connected}");
    let heartbeat = next_frame(&mut body, &mut buffer).await;
    assert_eq!(heartbeat, ": heartbeat\n\n");
}

#[tokio::test]
async fn heartbeats_repeat() {
    let live = live(1000, Duration::from_millis(50)).await;
    let (response, _connection) = connect(&live, "/api/event", true).await;
    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let _connected = next_frame(&mut body, &mut buffer).await;
    let heartbeat = next_frame(&mut body, &mut buffer).await;
    assert_eq!(heartbeat, ": heartbeat\n\n");
    let repeated =
        timeout(Duration::from_secs(2), next_frame(&mut body, &mut buffer)).await.expect("a repeated heartbeat");
    assert_eq!(repeated, ": heartbeat\n\n");
}

#[tokio::test]
async fn a_published_session_created_arrives_as_the_typescript_frame() {
    let live = live(1000, Duration::from_secs(60)).await;
    live.state
        .db
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            Ok(())
        })
        .expect("seed the project");

    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the events corpus");
    let fixture = corpus["fixtures"]
        .as_array()
        .expect("the corpus carries fixtures")
        .iter()
        .find(|fixture| fixture["name"] == "created-minimal")
        .expect("the created-minimal fixture");
    let payload = fixture["steps"][0]["data"].clone();
    let expected = fixture["frames"]["all"][0].as_str().expect("the expected frame").to_owned();

    let (response, _connection) = connect(&live, "/api/event", true).await;
    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let connected = next_frame(&mut body, &mut buffer).await;
    assert!(connected.contains("\"type\":\"server.connected\""), "{connected}");
    let heartbeat = next_frame(&mut body, &mut buffer).await;
    assert_eq!(heartbeat, ": heartbeat\n\n");

    let definition = Definition::latest_durable("session.created").expect("session.created is durable");
    live.state
        .bus
        .publish(&definition, payload, PublishOptions { id: Some("evt_1".to_owned()), ..Default::default() })
        .expect("publish session.created");

    let frame = next_frame(&mut body, &mut buffer).await;
    assert_eq!(frame, expected);
}

#[tokio::test]
async fn types_filters_the_stream() {
    let live = live(1000, Duration::from_secs(60)).await;
    let (response, _connection) = connect(&live, "/api/event?types=model.updated", true).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let _connected = next_frame(&mut body, &mut buffer).await;
    let heartbeat = next_frame(&mut body, &mut buffer).await;
    assert_eq!(heartbeat, ": heartbeat\n\n");

    for r#type in ["agent.updated", "model.updated"] {
        live.state
            .bus
            .publish(&Definition::ephemeral(r#type), json!({}), PublishOptions::default())
            .expect("publish the feed event");
    }

    let frame = next_frame(&mut body, &mut buffer).await;
    assert!(frame.contains("\"model.updated\""), "{frame}");
    assert!(!frame.contains("\"agent.updated\""), "{frame}");
}

#[tokio::test]
async fn a_bogus_type_is_a_query_error() {
    let live = live(1000, Duration::from_secs(60)).await;
    let (response, _connection) = connect(&live, "/api/event?types=bogus", true).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8");
    assert!(body.starts_with(r#"{"_tag":"InvalidRequestError","message":""#), "{body}");
    assert!(body.ends_with(r#"","kind":"Query"}"#), "{body}");
    let value: Value = serde_json::from_str(&body).expect("a JSON body");
    assert_eq!(value["_tag"], "InvalidRequestError");
    assert_eq!(value["kind"], "Query");
    assert!(value["message"].as_str().is_some_and(|message| !message.is_empty()), "{body}");
}

#[tokio::test]
async fn authentication_is_required() {
    let live = live(1000, Duration::from_secs(60)).await;
    let (response, _connection) = connect(&live, "/api/event", false).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    assert_eq!(String::from_utf8(bytes.to_vec()).expect("utf-8"), crate::UNAUTHORIZED_BODY);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_consumer_overflows_and_only_its_connection_aborts() {
    let live = live(1000, Duration::from_secs(60)).await;
    let (slow_response, _slow_connection) = connect(&live, "/api/event", true).await;
    let (response, _reader_connection) = connect(&live, "/api/event", true).await;
    assert_eq!(slow_response.status(), StatusCode::OK);
    assert_eq!(response.status(), StatusCode::OK);

    let target = SUBSCRIBER_CAPACITY + 64;
    let reader = tokio::spawn(async move {
        let mut body = response.into_body();
        let mut buffer = Vec::new();
        let mut count = 0;
        while count < target {
            let frame = next_frame(&mut body, &mut buffer).await;
            if frame.contains("\"type\":\"agent.updated\"") {
                count += 1;
            }
        }
        count
    });

    let pad = "x".repeat(131072);
    for index in 0..target {
        live.state
            .bus
            .publish(
                &Definition::ephemeral("agent.updated"),
                json!({ "pad": &pad, "index": index }),
                PublishOptions::default(),
            )
            .expect("publish agent.updated");
        if index % 256 == 0 {
            tokio::task::yield_now().await;
        }
    }

    let count = timeout(Duration::from_secs(30), reader)
        .await
        .expect("the reader to see every event")
        .expect("the reader task");
    assert_eq!(count, target, "the second reader must see every published event");

    let mut body = slow_response.into_body();
    let mut head = Vec::new();
    let mut received = 0;
    let aborted = loop {
        match timeout(Duration::from_secs(5), body.frame()).await {
            Ok(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if head.len() < 4096 {
                        let take = (4096 - head.len()).min(data.len());
                        head.extend_from_slice(&data[..take]);
                    }
                    received += data.len();
                }
            }
            Ok(Some(Err(_))) => break true,
            Ok(None) => break false,
            Err(_) => panic!("the slow connection neither aborted nor ended within 5s after {received} bytes"),
        }
    };
    assert!(aborted, "the slow connection ended cleanly instead of aborting after {received} bytes");
    assert!(
        String::from_utf8_lossy(&head).contains("server.connected"),
        "the slow connection received no frames before it aborted"
    );
}
