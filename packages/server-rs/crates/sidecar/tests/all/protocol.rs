use std::future::Future;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use rustix::io::fcntl_dupfd_cloexec;
use serde_json::{Value, json};
use tokio::net::UnixStream;

use opencode_sidecar::config::Config;
use opencode_sidecar::connection::{Connection, Handlers, Reply, Request};
use opencode_sidecar::error::SidecarError;
use opencode_sidecar::protocol::{LocationRef, RemoteError};
use opencode_sidecar::spawn::{Spawned, spawn};

use crate::support;

const TIMEOUT: Duration = Duration::from_secs(20);

async fn connect(config: &Config, handlers: Handlers) -> Option<(Spawned, Connection)> {
    let runtime = support::runtime(config).await?;
    let spawned = spawn(config, &runtime).await.expect("a spawned host");
    let copy = fcntl_dupfd_cloexec(spawned.stream.as_fd(), 0).expect("a socket to the host");
    let stream = UnixStream::from_std(std::os::unix::net::UnixStream::from(copy)).expect("a socket to the host");
    Some((spawned, Connection::start(stream, &config.limits, handlers)))
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future).await.expect("the host answered in time")
}

fn remote(error: SidecarError) -> RemoteError {
    RemoteError { tag: "Error".to_owned(), message: error.to_string(), data: None }
}

#[tokio::test]
async fn echo_carries_json_and_a_megabyte_of_bytes() {
    let mut config = support::config();
    config.limits.max_frame = 2 << 20;
    let Some((_host, connection)) = connect(&config, Handlers::new()).await else { return };

    let reply = within(connection.call(Request::new("host.echo", json!({ "hello": "world" })))).await.expect("a reply");
    assert_eq!(reply.value, json!({ "hello": "world" }));
    assert_eq!(reply.bytes, None);

    let bytes = Bytes::from(vec![7u8; 1 << 20]);
    let request = Request::new("host.echo", json!({ "kind": "binary" })).with_bytes(bytes.clone());
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value, json!({ "kind": "binary" }));
    assert_eq!(reply.bytes, Some(bytes));
}

#[tokio::test]
async fn nested_calls_run_while_the_outer_call_waits() {
    let mut handlers = Handlers::new();
    handlers.register("test.inner", |context, request| {
        Box::pin(async move {
            let reply = context.client.call(Request::new("host.echo", request.params)).await.map_err(remote)?;
            Ok(Reply::new(reply.value))
        })
    });
    let config = support::config();
    let Some((_host, connection)) = connect(&config, handlers).await else { return };

    let request = Request::new("host.nested", json!({ "method": "test.inner", "params": { "depth": 3 } }));
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value, json!({ "depth": 3 }));
}

#[tokio::test]
async fn a_reply_over_the_frame_cap_reaches_the_caller_as_an_error() {
    let mut handlers = Handlers::new();
    handlers.register("test.huge", |_context, _request| {
        Box::pin(async move { Ok(Reply { value: Value::Null, bytes: Some(Bytes::from(vec![0u8; 2 << 20])) }) })
    });
    let config = support::config();
    let Some((_host, connection)) = connect(&config, handlers).await else { return };

    let request = Request::new("host.nested", json!({ "method": "test.huge" }));
    match within(connection.call(request)).await {
        Err(SidecarError::Remote(error)) => assert_eq!(error.tag, "TooLarge", "{error:?}"),
        other => panic!("expected a TooLarge error, got {other:?}"),
    }
}

#[tokio::test]
async fn location_and_parent_travel_with_the_call() {
    let mut handlers = Handlers::new();
    handlers.register("test.location", |context, _request| {
        Box::pin(async move { Ok(Reply::new(json!({ "location": context.location, "parent": context.parent }))) })
    });
    let config = support::config();
    let Some((_host, connection)) = connect(&config, handlers).await else { return };
    let location = LocationRef { directory: "/work/here".to_owned(), workspace_id: Some("work-1".to_owned()) };

    let request = Request::new("host.location", Value::Null).with_location(location.clone());
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value, json!(location));

    let request = Request::new("host.nested", json!({ "method": "test.location" })).with_location(location.clone());
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value["location"], json!(location));
    assert_eq!(reply.value["parent"], json!(2));
}

#[tokio::test]
async fn a_dropped_call_cancels_the_javascript_handler() {
    let config = support::config();
    let Some((_host, connection)) = connect(&config, Handlers::new()).await else { return };

    tokio::select! {
        reply = connection.call(Request::new("host.sleep", json!({ "ms": 60_000 }))) => {
            panic!("the sleeping call answered: {reply:?}")
        }
        () = tokio::time::sleep(Duration::from_millis(50)) => {}
    }

    let mut aborts = 0;
    for _ in 0..200 {
        let reply = within(connection.call(Request::new("host.aborts", Value::Null))).await.expect("a reply");
        aborts = reply.value.as_u64().expect("a count");
        if aborts > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(aborts, 1);
}

#[tokio::test]
async fn javascript_aborting_its_call_fires_the_rust_token() {
    let fired = Arc::new(AtomicUsize::new(0));
    let mut handlers = Handlers::new();
    let watched = Arc::clone(&fired);
    handlers.register("test.pending", move |context, _request| {
        let cancel = context.cancel.clone();
        let fired = Arc::clone(&watched);
        Box::pin(async move {
            let _watcher = tokio::spawn(async move {
                cancel.cancelled().await;
                fired.fetch_add(1, Ordering::SeqCst);
            });
            std::future::pending::<()>().await;
            unreachable!()
        })
    });
    let config = support::config();
    let Some((_host, connection)) = connect(&config, handlers).await else { return };

    let request = Request::new("host.abort", json!({ "method": "test.pending", "params": {}, "ms": 50 }));
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value, json!({ "aborted": true }));

    let mut seen = 0;
    for _ in 0..200 {
        seen = fired.load(Ordering::SeqCst);
        if seen > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(seen, 1);
}

#[tokio::test]
async fn a_slow_consumer_keeps_a_javascript_stream_within_its_credit() {
    let mut config = support::config();
    config.limits.window = 4;
    let Some((_host, connection)) = connect(&config, Handlers::new()).await else { return };

    let request = Request::new("host.stream", json!({ "count": 500, "size": 1024 }));
    let mut stream = within(connection.stream(request)).await.expect("a stream");

    let mut indexes = Vec::new();
    while let Some(chunk) = within(stream.next()).await {
        let chunk = chunk.expect("a chunk");
        assert_eq!(chunk.bytes.as_ref().map(Bytes::len), Some(1024));
        indexes.push(chunk.value["index"].as_u64().expect("an index"));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(indexes, (0..500).collect::<Vec<u64>>());

    let reply = within(connection.call(Request::new("host.streamStats", Value::Null))).await.expect("a reply");
    let inflight = reply.value["maxInflight"].as_u64().expect("a count");
    assert!((1..=4).contains(&inflight), "a max in flight of {inflight} stays within the window");
}

#[tokio::test]
async fn a_chunk_over_the_frame_cap_keeps_its_credit() {
    let config = support::config();
    let window = config.limits.window;
    let mut handlers = Handlers::new();
    handlers.register_stream("test.oversized", move |_context, _request, sink| {
        Box::pin(async move {
            for _ in 0..=window {
                let huge = Reply { value: Value::Null, bytes: Some(Bytes::from(vec![0u8; 2 << 20])) };
                match sink.send(huge).await {
                    Err(SidecarError::TooLarge { .. }) => {}
                    other => panic!("expected TooLarge, got {other:?}"),
                }
            }
            sink.send(Reply::new(json!({ "index": 0 }))).await.map_err(remote)
        })
    });
    let Some((_host, connection)) = connect(&config, handlers).await else { return };

    let request = Request::new("host.consume", json!({ "method": "test.oversized" }));
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value["count"], json!(1));
}

#[tokio::test]
async fn a_javascript_consumer_drains_a_rust_stream() {
    let mut handlers = Handlers::new();
    handlers.register_stream("test.many", |_context, request, sink| {
        Box::pin(async move {
            let count = request.params["count"].as_u64().unwrap_or(0);
            for index in 0..count {
                sink.send(Reply::new(json!({ "index": index }))).await.map_err(remote)?;
            }
            Ok(())
        })
    });
    let config = support::config();
    let window = u64::from(config.limits.window);
    let Some((_host, connection)) = connect(&config, handlers).await else { return };

    let request = Request::new(
        "host.consume",
        json!({
            "method": "test.many",
            "params": { "count": 200 },
            "delayMs": 1,
        }),
    );
    let reply = within(connection.call(request)).await.expect("a reply");
    assert_eq!(reply.value["count"], json!(200));
    let buffered = reply.value["maxBuffered"].as_u64().expect("a count");
    assert!(buffered <= window, "a max buffered of {buffered} stays within the window of {window}");
}

#[tokio::test]
async fn oversize_frames_fail_in_both_directions() {
    let mut config = support::config();
    config.limits.max_frame = 64 << 10;
    let Some((_outgoing, outgoing)) = connect(&config, Handlers::new()).await else { return };

    let big = Request::new("host.echo", json!({ "big": "x".repeat(128 << 10) }));
    let error = within(outgoing.call(big)).await.expect_err("a frame over the cap");
    assert!(matches!(error, SidecarError::TooLarge { .. }), "{error:?}");
    let reply = within(outgoing.call(Request::new("host.echo", json!({ "small": true })))).await.expect("a reply");
    assert_eq!(reply.value, json!({ "small": true }));

    let config = support::config();
    let Some((_incoming, incoming)) = connect(&config, Handlers::new()).await else { return };

    let error = within(incoming.call(Request::new("host.oversize", Value::Null))).await.expect_err("an oversize frame");
    assert!(matches!(error, SidecarError::ProtocolBroken(_)), "{error:?}");
}
