use std::ffi::{OsStr, OsString};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use opencode_sidecar::config::inherited;
use opencode_sidecar::frame::{self, DEFAULT_MAX_FRAME};
use opencode_sidecar::protocol::Message;
use opencode_sidecar::spawn::{Spawned, spawn};

use crate::support;

#[tokio::test]
async fn a_spawned_host_answers_ready_and_echoes() {
    let config = support::config();
    let Some(runtime) = support::runtime(&config).await else { return };

    let mut spawned = spawn(&config, &runtime).await.expect("a spawned host");

    assert!(spawned.pid > 0);
    let value = call(&mut spawned, 1, "host.echo", json!({ "hello": "world" })).await;
    assert_eq!(value, json!({ "hello": "world" }));
}

#[tokio::test]
async fn the_host_sees_only_the_allowlisted_environment() {
    let project = opencode_util::temp::tempdir().expect("a directory");
    std::fs::write(project.path().join(".env"), "SIDECAR_DOTENV=leaked\n").expect("a file");
    let mut config = support::config();
    config.directory = project.path().to_owned();
    config.vars.push((OsString::from("OPENCODE_PASSWORD"), OsString::from("hunter2")));
    config.vars.push((OsString::from("OPENCODE_SERVER_PASSWORD"), OsString::from("hunter2")));
    let Some(runtime) = support::runtime(&config).await else { return };

    let mut spawned = spawn(&config, &runtime).await.expect("a spawned host");
    let value = call(&mut spawned, 1, "host.env", Value::Null).await;

    let names = value.as_array().expect("a list").iter().map(|name| name.as_str().expect("a name")).collect::<Vec<_>>();
    assert!(names.contains(&"PATH"), "an allowlisted PATH reaches the host");
    for forbidden in ["OPENCODE_PASSWORD", "OPENCODE_SERVER_PASSWORD", "SIDECAR_DOTENV"] {
        assert!(!names.contains(&forbidden), "{forbidden} must not reach the host");
    }
    for name in names {
        assert!(inherited(OsStr::new(name)), "{name} is not allowlisted");
    }
}

#[tokio::test]
async fn print_output_does_not_break_the_protocol() {
    let config = support::config();
    let Some(runtime) = support::runtime(&config).await else { return };

    let mut spawned = spawn(&config, &runtime).await.expect("a spawned host");
    let printed = call(&mut spawned, 1, "host.print", json!({ "text": "sidecar-print-marker" })).await;
    let echoed = call(&mut spawned, 2, "host.echo", json!({ "after": true })).await;

    assert_eq!(printed, Value::Null);
    assert_eq!(echoed, json!({ "after": true }));
    assert!(wait_for(&spawned, "sidecar-print-marker").await, "the printed line is kept");
}

async fn call(spawned: &mut Spawned, id: u64, method: &str, params: Value) -> Value {
    let message = Message::Call { id, method: method.to_owned(), location: None, parent: None, params, stream: false };
    let bytes = frame::encode(&message, None, DEFAULT_MAX_FRAME).expect("a frame");
    spawned.stream.write_all(&bytes).await.expect("a call");
    let frame = frame::read(&mut spawned.stream, DEFAULT_MAX_FRAME).await.expect("a frame").expect("a reply");
    match frame.message {
        Message::Result { id: got, value } => {
            assert_eq!(got, id, "the reply names the call");
            value
        }
        other => panic!("unexpected reply {other:?}"),
    }
}

async fn wait_for(spawned: &Spawned, needle: &str) -> bool {
    for _ in 0..100 {
        if spawned.stderr().iter().any(|line| line.contains(needle)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}
