use std::path::Path;
use std::time::Duration;

use futures_util::SinkExt;
use futures_util::StreamExt;
use http::StatusCode;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::pty::{Connection, Event, Live, environment, json, live_with, read_socket, send};

fn binary_available() -> bool {
    std::env::var_os("OPENCODE_PTY_BIN").is_some_and(|value| !value.is_empty())
        || std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|directory| directory.join("opencode-pty").is_file())
        })
}

async fn live(directory: &Path) -> Live {
    let home = crate::temp_directory("persistent-pty-home");
    let mut env = environment();
    env.retain(|(name, _)| name != "SHELL");
    env.push(("SHELL".to_owned(), "/bin/sh".to_owned()));
    live_with(directory, &home, env).await
}

async fn create(live: &Live, session: &str, body: Value) -> Value {
    let (status, bytes, _) =
        send(live, "POST", &format!("/api/experimental/session/{session}/terminal"), &[], Some(&body.to_string()))
            .await;
    assert_eq!(status, StatusCode::OK, "create failed: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["data"].clone()
}

async fn read(live: &Live, session: &str, lines: Option<u32>) -> Value {
    let url = match lines {
        Some(lines) => format!("/api/experimental/session/{session}/terminal/read?lines={lines}"),
        None => format!("/api/experimental/session/{session}/terminal/read"),
    };
    let (status, bytes, _) = send(live, "GET", &url, &[], None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["data"].clone()
}

async fn get(live: &Live, id: &str) -> Value {
    let (status, bytes, _) = send(live, "GET", &format!("/api/experimental/persistent-pty/{id}"), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    json(&bytes)["data"].clone()
}

async fn snapshot(live: &Live, id: &str) -> Value {
    let (status, bytes, _) =
        send(live, "GET", &format!("/api/experimental/persistent-pty/{id}/snapshot"), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    json(&bytes)["data"].clone()
}

async fn update(live: &Live, id: &str, body: Value) -> (StatusCode, Value) {
    let (status, bytes, _) =
        send(live, "PUT", &format!("/api/experimental/persistent-pty/{id}"), &[], Some(&body.to_string())).await;
    (status, if bytes.is_empty() { Value::Null } else { json(&bytes) })
}

async fn remove(live: &Live, id: &str) -> StatusCode {
    send(live, "DELETE", &format!("/api/experimental/persistent-pty/{id}"), &[], None).await.0
}

async fn shutdown(live: &Live) {
    let (status, _, _) = send(live, "POST", "/api/experimental/persistent-pty/shutdown", &[], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

async fn wait_for_text(live: &Live, id: &str, expected: &str) {
    for _ in 0..80 {
        let text = snapshot(live, id).await["text"].as_str().unwrap_or_default().to_owned();
        if text.contains(expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the persistent PTY snapshot did not contain {expected}");
}

async fn wait_for_read(live: &Live, session: &str, id: &str, expected: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let value = read(live, session, None).await;
        if value["ptyID"] == json!(id) && expected(&value) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the persistent PTY {id} did not become current for {session}");
}

async fn wait_for_status(live: &Live, id: &str, status: &str) {
    for _ in 0..80 {
        if get(live, id).await["status"] == json!(status) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the persistent PTY {id} did not reach {status}");
}

async fn token(live: &Live, id: &str) -> String {
    let (status, bytes, _) = send(
        live,
        "POST",
        &format!("/api/experimental/persistent-pty/{id}/connect-token"),
        &[("x-opencode-ticket", "1")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json(&bytes)["data"]["ticket"].as_str().expect("a ticket").to_owned()
}

#[derive(Default)]
struct Remote {
    output: String,
    events: Vec<String>,
    closed: Option<(u16, String)>,
    attached: bool,
    replay_complete: bool,
    size: Option<(u16, u16)>,
    controller: Option<String>,
    foreground: Option<String>,
    resizes: usize,
}

impl Remote {
    fn apply(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                let value: Value = serde_json::from_str(&text).expect("a persistent PTY event");
                let kind = value["type"].as_str().unwrap_or_default().to_owned();
                self.events.push(kind.clone());
                match kind.as_str() {
                    "attached" => {
                        self.attached = true;
                        self.input_protocol(value["inputProtocol"].as_u64());
                        self.foreground = value["info"]["foregroundProcess"].as_str().map(str::to_owned);
                    }
                    "replay_complete" => self.replay_complete = true,
                    "resized" => {
                        self.resizes += 1;
                        if let (Some(cols), Some(rows)) = (value["cols"].as_u64(), value["rows"].as_u64()) {
                            self.size = Some((cols as u16, rows as u16));
                        }
                        assert!(value["checkpoint"].is_string(), "a resize carries its checkpoint");
                    }
                    "controller_changed" => {
                        self.controller = value["attachmentID"].as_str().map(str::to_owned);
                    }
                    "foreground_process_changed" => {
                        self.foreground = value["process"].as_str().map(str::to_owned);
                    }
                    _ => {}
                }
            }
            Event::Binary(data) => self.output.push_str(&String::from_utf8_lossy(&data)),
            Event::Closed(code, reason) => self.closed = Some((code, reason)),
        }
    }

    fn input_protocol(&mut self, value: Option<u64>) {
        assert_eq!(value, Some(1), "the socket negotiated framed input");
    }
}

async fn open(live: &Live, id: &str, attachment: &str, role: &str) -> (Connection, Remote) {
    let ticket = token(live, id).await;
    let url = format!(
        "ws://127.0.0.1:{}/api/experimental/persistent-pty/{id}/connect?ticket={ticket}&attachment_id={attachment}&role={role}&takeover=true&input_protocol=1",
        live.port
    );
    let (socket, response) = connect_async(url).await.expect("a persistent PTY websocket handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let (sink, stream) = socket.split();
    let (events, receiver) = mpsc::unbounded_channel();
    tokio::spawn(read_socket(stream, events));
    let mut connection = Connection { sink, events: receiver };
    let mut state = Remote::default();
    pump(&mut connection, &mut state, |state| state.replay_complete).await;
    assert!(state.attached);
    (connection, state)
}

async fn pump(connection: &mut Connection, state: &mut Remote, predicate: impl Fn(&Remote) -> bool) {
    for _ in 0..500 {
        if predicate(state) {
            return;
        }
        match tokio::time::timeout(Duration::from_millis(20), connection.events.recv()).await {
            Ok(Some(event)) => state.apply(event),
            Ok(None) => panic!("the persistent PTY socket ended unexpectedly"),
            Err(_) => {}
        }
    }
    panic!("the persistent PTY socket did not reach the expected state: {:?}", state.events);
}

fn control_frame(cols: u16, rows: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5);
    frame.push(0);
    frame.extend_from_slice(&cols.to_be_bytes());
    frame.extend_from_slice(&rows.to_be_bytes());
    frame
}

fn input_frame(cols: u16, rows: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = control_frame(cols, rows);
    frame[0] = 1;
    frame.extend_from_slice(data);
    frame
}

async fn send_binary(connection: &mut Connection, data: &[u8]) {
    connection.sink.send(Message::Binary(data.to_vec().into())).await.expect("send a binary frame");
}

#[tokio::test]
async fn persistent_pty_reads_the_latest_controlled_terminal_and_shares_its_control() {
    if !binary_available() {
        eprintln!("skipping: opencode-pty was not found");
        return;
    }
    let directory = crate::temp_directory("persistent-pty-read");
    let live = live(&directory).await;
    let session = "ses_terminal_read";

    assert_eq!(read(&live, session, None).await, Value::Null);
    assert!(!live.persistent_root.exists());
    for value in ["0", "-1", "1.5", "65536", "nope"] {
        let (status, bytes, _) =
            send(&live, "GET", &format!("/api/experimental/session/{session}/terminal/read?lines={value}"), &[], None)
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "lines={value}: {}", String::from_utf8_lossy(&bytes));
    }
    assert!(!live.persistent_root.exists());

    let first = create(
        &live,
        session,
        json!({
            "command": "/bin/sh",
            "args": ["-c", "stty -echo; seq 1 20; exec cat"],
            "cwd": directory.to_string_lossy(),
            "title": "first",
            "env": {},
            "size": { "cols": 40, "rows": 4 },
        }),
    )
    .await;
    let second = create(
        &live,
        session,
        json!({
            "command": "/bin/sh",
            "args": ["-c", "printf second; exec cat"],
            "cwd": directory.to_string_lossy(),
            "title": "second",
            "env": {},
        }),
    )
    .await;
    wait_for_text(&live, first["id"].as_str().expect("a first id"), "20").await;
    assert_eq!(read(&live, session, None).await, Value::Null);

    let first_id = first["id"].as_str().expect("a first id").to_owned();
    let second_id = second["id"].as_str().expect("a second id").to_owned();
    let (mut controller, _controller_state) = open(&live, &first_id, "read-first", "controller").await;
    let (mut observer, _observer_state) = open(&live, &second_id, "read-second", "observer").await;

    let current = wait_for_read(&live, session, &first_id, |_| true).await;
    assert_eq!(current["title"], json!("first"));
    assert_eq!(current["cwd"], json!(directory.to_string_lossy()));
    assert_eq!(current["screen"]["cols"], json!(40));
    assert_eq!(current["screen"]["rows"], json!(4));
    assert_eq!(current["screen"]["text"].as_str().expect("text"), "18\n19\n20\n");
    assert_eq!(current["screen"]["cursor"], json!({ "x": 0, "y": 3 }));
    assert!(current["foregroundProcess"].is_null() || current["foregroundProcess"].is_string());
    assert_eq!(read(&live, "ses_other", None).await, Value::Null);
    snapshot(&live, &second_id).await;

    for value in [2, 6, 65535] {
        let read = read(&live, session, Some(value)).await;
        assert_eq!(read["ptyID"], json!(first_id));
        assert_eq!(read["screen"]["rows"], json!(4));
        let mut expected: Vec<String> = (1..=20).map(|index| index.to_string()).collect();
        expected.push(String::new());
        let expected = expected[expected.len().saturating_sub(value as usize)..].to_vec();
        assert_eq!(
            read["screen"]["text"].as_str().expect("text").split('\n').map(str::to_owned).collect::<Vec<_>>(),
            expected
        );
    }

    send_binary(&mut observer, &control_frame(30, 3)).await;
    assert_eq!(wait_for_read(&live, session, &second_id, |_| true).await["screen"]["rows"], json!(3));
    let (status, body) =
        update(&live, &first_id, json!({ "attachmentID": "read-first", "size": { "cols": 42, "rows": 5 } })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["size"], json!({ "cols": 42, "rows": 5 }));
    assert_eq!(read(&live, session, None).await["ptyID"], json!(first_id));
    send_binary(&mut observer, &control_frame(30, 3)).await;
    wait_for_read(&live, session, &second_id, |_| true).await;
    send_binary(&mut controller, &input_frame(40, 4, b"typed\n")).await;
    let mut controller_state = _controller_state;
    pump(&mut controller, &mut controller_state, |state| state.output.contains("typed")).await;
    let typed = wait_for_read(&live, session, &first_id, |value| {
        value["screen"]["text"].as_str().is_some_and(|text| text.contains("typed"))
    })
    .await;
    assert!(typed["screen"]["text"].as_str().expect("text").contains("typed"));

    let (takeover, _) = open(&live, &second_id, "read-takeover", "controller").await;
    assert_eq!(read(&live, session, None).await["ptyID"], json!(second_id));
    drop(takeover);
    assert_eq!(remove(&live, &first_id).await, StatusCode::NO_CONTENT);
    assert_eq!(read(&live, session, None).await["ptyID"], json!(second_id));
    assert_eq!(remove(&live, &second_id).await, StatusCode::NO_CONTENT);
    assert_eq!(read(&live, session, None).await, Value::Null);
    shutdown(&live).await;
    assert_eq!(read(&live, session, None).await, Value::Null);
}

#[tokio::test]
async fn persistent_pty_renames_and_resizes_a_terminal_independently() {
    if !binary_available() {
        eprintln!("skipping: opencode-pty was not found");
        return;
    }
    let directory = crate::temp_directory("persistent-pty-rename");
    let live = live(&directory).await;
    let session = "ses_persistent_pty_rename";
    let terminal = create(
        &live,
        session,
        json!({
            "command": "/bin/sh",
            "args": ["-c", "stty -echo; printf '\\033]2;from-shell\\007ready'; exec cat"],
            "cwd": directory.to_string_lossy(),
            "title": "created",
            "env": {},
            "size": { "cols": 40, "rows": 10 },
        }),
    )
    .await;
    let id = terminal["id"].as_str().expect("an id").to_owned();
    assert!(terminal["userTitle"].is_null());
    wait_for_text(&live, &id, "ready").await;
    for _ in 0..100 {
        if get(&live, &id).await["title"] == json!("from-shell") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(get(&live, &id).await["title"], json!("from-shell"));

    let (status, renamed) = update(&live, &id, json!({ "title": "build" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["data"]["title"], json!("build"));
    assert_eq!(renamed["data"]["userTitle"], json!("build"));
    assert_eq!(renamed["data"]["size"], json!({ "cols": 40, "rows": 10 }));
    let listed = send(&live, "GET", &format!("/api/experimental/session/{session}/terminal"), &[], None).await;
    assert_eq!(listed.0, StatusCode::OK);
    assert_eq!(json(&listed.1)["data"][0]["title"], json!("build"));

    let (status, resized) = update(&live, &id, json!({ "size": { "cols": 50, "rows": 12 } })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resized["data"]["size"], json!({ "cols": 50, "rows": 12 }));

    let (status, cleared) = update(&live, &id, json!({ "title": null })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["data"]["title"], json!("from-shell"));
    assert!(cleared["data"]["userTitle"].is_null());

    let (status, _) = update(&live, "pty_persistent_1", json!({ "title": "nope" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    shutdown(&live).await;
}

#[tokio::test]
async fn persistent_pty_rejects_a_cwd_that_is_missing_or_not_a_directory() {
    if !binary_available() {
        eprintln!("skipping: opencode-pty was not found");
        return;
    }
    let directory = crate::temp_directory("persistent-pty-cwd");
    let live = live(&directory).await;
    let create_path = "/api/experimental/session/ses_invalid_cwd/terminal";
    let file = directory.join("file.txt");
    std::fs::write(&file, "").expect("write the file");
    for cwd in [directory.join("missing"), file.clone()] {
        let body = json!({
            "command": "/bin/sh",
            "args": [],
            "cwd": cwd.to_string_lossy(),
            "title": "invalid",
            "env": {},
        });
        let (status, bytes, _) = send(&live, "POST", create_path, &[], Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            json(&bytes),
            json!({
                "_tag": "InvalidRequestError",
                "message": format!("cwd is not a directory: {}", cwd.to_string_lossy()),
                "kind": "invalid_cwd",
                "field": "cwd",
            })
        );
    }
    assert!(!live.persistent_root.exists());
    let (status, bytes, _) = send(&live, "GET", create_path, &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["data"], json!([]));

    let created = create(
        &live,
        "ses_invalid_cwd",
        json!({
            "command": "/bin/sh",
            "args": [],
            "cwd": directory.to_string_lossy(),
            "title": "valid",
            "env": {},
        }),
    )
    .await;
    assert_eq!(created["cwd"], json!(directory.to_string_lossy()));
    shutdown(&live).await;
}

#[tokio::test]
async fn persistent_pty_lists_an_exited_unattended_terminal_until_it_is_removed() {
    if !binary_available() {
        eprintln!("skipping: opencode-pty was not found");
        return;
    }
    let directory = crate::temp_directory("persistent-pty-exit");
    let live = live(&directory).await;
    let session = "ses_persistent_exit";
    let terminal = create(
        &live,
        session,
        json!({
            "command": "/bin/sh",
            "args": ["-c", "exit 7"],
            "cwd": directory.to_string_lossy(),
            "title": "unattended",
            "env": {},
        }),
    )
    .await;
    let id = terminal["id"].as_str().expect("an id").to_owned();
    wait_for_status(&live, &id, "exited").await;
    let (status, bytes, _) =
        send(&live, "GET", &format!("/api/experimental/session/{session}/terminal"), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let listed = json(&bytes);
    assert_eq!(listed["data"].as_array().expect("a list").len(), 1);
    assert_eq!(listed["data"][0]["id"], json!(id));
    assert_eq!(listed["data"][0]["status"], json!("exited"));
    assert_eq!(listed["data"][0]["exitCode"], json!(7));
    assert_eq!(remove(&live, &id).await, StatusCode::NO_CONTENT);
    let (_, bytes, _) = send(&live, "GET", &format!("/api/experimental/session/{session}/terminal"), &[], None).await;
    assert_eq!(json(&bytes)["data"], json!([]));
    shutdown(&live).await;
}

#[tokio::test]
async fn persistent_pty_connect_to_an_unknown_terminal_closes_with_4404() {
    let directory = crate::temp_directory("persistent-pty-missing");
    let live = live(&directory).await;
    let (status, bytes, _) = send(
        &live,
        "POST",
        "/api/experimental/persistent-pty/pty_persistent_99/connect-token",
        &[("x-opencode-ticket", "1")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["_tag"], json!("PtyNotFoundError"));

    let ticket = send(
        &live,
        "POST",
        "/api/experimental/persistent-pty/pty_persistent_99/connect-token",
        &[("x-opencode-ticket", "1")],
        None,
    )
    .await;
    assert_eq!(ticket.0, StatusCode::NOT_FOUND);
    assert!(!live.persistent_root.exists());
}

#[tokio::test]
async fn persistent_pty_connect_to_a_removed_terminal_closes_with_4404() {
    if !binary_available() {
        eprintln!("skipping: opencode-pty was not found");
        return;
    }
    let directory = crate::temp_directory("persistent-pty-removed");
    let live = live(&directory).await;
    let terminal = create(
        &live,
        "ses_removed",
        json!({
            "command": "/bin/sh",
            "args": ["-c", "sleep 30"],
            "cwd": directory.to_string_lossy(),
            "title": "removed",
            "env": {},
        }),
    )
    .await;
    let id = terminal["id"].as_str().expect("an id").to_owned();
    let ticket = token(&live, &id).await;
    assert_eq!(remove(&live, &id).await, StatusCode::NO_CONTENT);

    let url = format!(
        "ws://127.0.0.1:{}/api/experimental/persistent-pty/{id}/connect?ticket={ticket}&attachment_id=removed",
        live.port
    );
    let (socket, response) = connect_async(url).await.expect("a websocket handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let (sink, stream) = socket.split();
    let (events, receiver) = mpsc::unbounded_channel();
    tokio::spawn(read_socket(stream, events));
    let mut connection = Connection { sink, events: receiver };
    let mut state = Remote::default();
    pump(&mut connection, &mut state, |state| state.closed.is_some()).await;
    assert_eq!(state.closed, Some((4404, "terminal unavailable".to_owned())));
    shutdown(&live).await;
}

#[tokio::test]
async fn unticketed_persistent_pty_connect_is_forbidden() {
    let directory = crate::temp_directory("persistent-pty-forbidden");
    let live = live(&directory).await;
    let result = connect_async(format!(
        "ws://127.0.0.1:{}/api/experimental/persistent-pty/pty_persistent_1/connect?ticket=invalid",
        live.port
    ))
    .await;
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        _ => panic!("expected the connection to be forbidden"),
    }
}
