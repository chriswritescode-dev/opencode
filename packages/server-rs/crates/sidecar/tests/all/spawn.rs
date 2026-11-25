use std::ffi::{OsStr, OsString};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use opencode_sidecar::bun::Runtime;
use opencode_sidecar::config::inherited;
use opencode_sidecar::error::SidecarError;
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
async fn a_runtime_that_exits_before_ready_fails_without_waiting_out_the_handshake() {
    let dir = opencode_util::temp::tempdir().expect("a directory");
    let stub = support::script(dir.path(), "bun", "echo gone >&2\nexit 2");
    let mut config = support::config();
    config.limits.handshake = Duration::from_secs(30);
    let runtime = Runtime { bun: stub, opencode: false, version: None };

    let result = tokio::time::timeout(Duration::from_secs(10), spawn(&config, &runtime))
        .await
        .expect("the start failed before the handshake timeout");

    assert!(matches!(result, Err(SidecarError::Handshake(_))), "{:?}", result.err());
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
    for name in names.into_iter().filter(|name| !(runtime.opencode && *name == "BUN_BE_BUN")) {
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
    for marker in ["sidecar-print-marker:stdout", "sidecar-print-marker:stderr"] {
        assert!(wait_for(&spawned, marker).await, "{marker} is kept");
    }
}

#[tokio::test]
async fn the_plugin_bundle_is_present_but_not_loaded_until_used() {
    let config = support::config();
    let Some(runtime) = support::runtime(&config).await else { return };

    let mut spawned = spawn(&config, &runtime).await.expect("a spawned host");
    for name in ["host.js", "plugins.js", "bunfig.toml"] {
        let path = spawned.directory().join(name);
        assert!(std::fs::metadata(&path).expect("the file exists").is_file(), "{name} is a regular file");
    }

    let echoed = call(&mut spawned, 1, "host.echo", json!({ "warm": true })).await;
    assert_eq!(echoed, json!({ "warm": true }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = rss(spawned.pid);

    let located = call(&mut spawned, 2, "host.location", Value::Null).await;
    assert_eq!(located, Value::Null);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = rss(spawned.pid);
    assert!(after.saturating_sub(before) < 5 * 1024 * 1024, "host.location loaded plugins.js");
}

fn rss(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("the process status");
    let line = status.lines().find(|line| line.starts_with("VmRSS:")).expect("VmRSS");
    let kib = line.split_whitespace().nth(1).expect("a value").parse::<u64>().expect("kilobytes");
    kib * 1024
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
