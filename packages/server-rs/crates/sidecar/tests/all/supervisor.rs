use std::future::Future;
use std::os::fd::AsFd;
use std::time::Duration;

use rustix::io::fcntl_dupfd_cloexec;
use rustix::process::{Pid, test_kill_process};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use opencode_sidecar::connection::{Handlers, Request};
use opencode_sidecar::error::SidecarError;
use opencode_sidecar::frame::{self, DEFAULT_MAX_FRAME};
use opencode_sidecar::protocol::Message;
use opencode_sidecar::spawn::spawn;
use opencode_sidecar::supervisor::{Sidecar, State};

use crate::support;

const TIMEOUT: Duration = Duration::from_secs(30);

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future).await.expect("in time")
}

fn alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else { return false };
    let Some(pid) = Pid::from_raw(raw) else { return false };
    test_kill_process(pid).is_ok()
}

async fn wait_gone(pid: u32, budget: Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline, "the process {pid} is still alive");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_state(state: &mut watch::Receiver<State>, wanted: impl Fn(&State) -> bool) -> State {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let current = state.borrow().clone();
        if wanted(&current) {
            return current;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "the state never matched, last was {current:?}");
        tokio::time::timeout(left, state.changed())
            .await
            .expect("a state change in time")
            .expect("the state channel stays open");
    }
}

fn echo() -> Request {
    Request::new("host.echo", json!({ "hello": true }))
}

#[tokio::test]
async fn nothing_starts_before_the_first_call() {
    let config = support::config();
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    assert_eq!(sidecar.pid(), None);
    assert!(matches!(*sidecar.state().borrow(), State::Idle), "nothing is running yet");

    let reply = within(sidecar.call(echo())).await.expect("a reply");
    assert_eq!(reply.value, json!({ "hello": true }));
    assert!(sidecar.pid().is_some(), "the first call started the host");
    let State::Ready { generation, .. } = sidecar.state().borrow().clone() else { panic!("not ready") };
    assert_eq!(generation, 1, "one start");

    sidecar.shutdown().await;
}

#[tokio::test]
async fn concurrent_first_calls_share_one_start() {
    let config = support::config();
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    let calls = (0..20)
        .map(|_| {
            let sidecar = sidecar.clone();
            tokio::spawn(async move { sidecar.call(echo()).await })
        })
        .collect::<Vec<_>>();
    for call in calls {
        let reply = within(call).await.expect("the call task").expect("a reply");
        assert_eq!(reply.value, json!({ "hello": true }));
    }

    let State::Ready { generation, pid } = sidecar.state().borrow().clone() else { panic!("not ready") };
    assert_eq!(generation, 1, "a second start would have bumped the generation");
    assert_eq!(sidecar.pid(), Some(pid));

    sidecar.shutdown().await;
}

#[tokio::test]
async fn a_crash_fails_the_calls_in_flight_and_restarts_after_the_backoff() {
    let mut config = support::config();
    config.limits.backoff_initial = Duration::from_millis(150);
    config.limits.backoff_max = Duration::from_millis(600);
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");
    let State::Ready { generation, pid } = sidecar.state().borrow().clone() else { panic!("not ready") };

    let sleeps = (0..2)
        .map(|_| {
            let sidecar = sidecar.clone();
            tokio::spawn(async move { sidecar.call(Request::new("host.sleep", json!({ "ms": 60_000 }))).await })
        })
        .collect::<Vec<_>>();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let crash = within(sidecar.call(Request::new("host.crash", json!({ "code": 7 })))).await;
    assert!(matches!(crash, Err(SidecarError::Exited { .. })), "{crash:?}");
    for sleep in sleeps {
        let result = within(sleep).await.expect("the call task");
        assert!(matches!(result, Err(SidecarError::Exited { .. })), "{result:?}");
    }

    let mut state = sidecar.state();
    wait_state(&mut state, |state| matches!(state, State::Exited { generation: got, .. } if *got == generation)).await;

    let error = sidecar.call(echo()).await.expect_err("a backoff");
    let SidecarError::Unavailable { retry_in } = error else { panic!("{error:?}") };
    assert!(retry_in > Duration::ZERO);
    tokio::time::sleep(retry_in + Duration::from_millis(100)).await;

    within(sidecar.call(echo())).await.expect("a restart");
    let State::Ready { generation: restarted, pid: restarted_pid } = state.borrow().clone() else {
        panic!("not ready")
    };
    assert_eq!(restarted, generation + 1);
    assert_ne!(restarted_pid, pid);

    sidecar.shutdown().await;
}

#[tokio::test]
async fn a_blocked_host_is_killed_for_liveness() {
    let mut config = support::config();
    config.limits.heartbeat = Duration::from_millis(100);
    config.limits.liveness = Duration::from_millis(500);
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    let error =
        within(sidecar.call(Request::new("host.block", json!({ "ms": 4000 })))).await.expect_err("a blocked host");
    assert!(matches!(error, SidecarError::Unresponsive), "{error:?}");

    let mut state = sidecar.state();
    wait_state(&mut state, |state| matches!(state, State::Exited { reason: SidecarError::Unresponsive, .. })).await;

    sidecar.shutdown().await;
}

#[tokio::test]
async fn an_oversized_host_is_killed_for_its_memory() {
    let mut config = support::config();
    config.limits.heartbeat = Duration::from_millis(100);
    config.limits.memory = 128 << 20;
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");

    let sleeping = tokio::spawn({
        let sidecar = sidecar.clone();
        async move { sidecar.call(Request::new("host.sleep", json!({ "ms": 10_000 }))).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = within(sidecar.call(Request::new("host.allocate", json!({ "mb": 256 })))).await;

    let result = within(sleeping).await.expect("the call task");
    let error = result.expect_err("the sleeping call");
    let SidecarError::MemoryExceeded { rss } = error else { panic!("{error:?}") };
    assert!(rss > 128u64 << 20, "{rss} is over the ceiling");

    let mut state = sidecar.state();
    wait_state(&mut state, |state| matches!(state, State::Exited { reason: SidecarError::MemoryExceeded { .. }, .. }))
        .await;

    sidecar.shutdown().await;
}

#[tokio::test]
async fn max_outgoing_bounds_the_calls_in_flight() {
    let mut config = support::config();
    config.limits.max_outgoing = 2;
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");

    let calls = (0..3)
        .map(|_| {
            let sidecar = sidecar.clone();
            tokio::spawn(async move { sidecar.call(Request::new("host.sleep", json!({ "ms": 400 }))).await })
        })
        .collect::<Vec<_>>();
    let mut succeeded = 0;
    let mut busy = 0;
    for call in calls {
        match within(call).await.expect("the call task") {
            Ok(_) => succeeded += 1,
            Err(SidecarError::Busy) => busy += 1,
            Err(other) => panic!("an unexpected error: {other:?}"),
        }
    }
    assert_eq!(succeeded, 2, "two calls fit within max_outgoing");
    assert_eq!(busy, 1, "the third is turned away");

    sidecar.shutdown().await;
}

#[tokio::test]
async fn crashes_back_off_and_then_disable_the_sidecar() {
    let mut config = support::config();
    config.limits.backoff_initial = Duration::from_millis(50);
    config.limits.backoff_max = Duration::from_millis(400);
    config.limits.crash_limit = 3;
    config.limits.crash_window = Duration::from_secs(60);
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");
    let mut state = sidecar.state();

    within(sidecar.call(Request::new("host.crash", json!({ "code": 1 })))).await.expect_err("a crash");
    wait_state(&mut state, |state| !matches!(state, State::Ready { .. })).await;

    let error = sidecar.call(echo()).await.expect_err("a backoff");
    let SidecarError::Unavailable { retry_in: first } = error else { panic!("{error:?}") };
    assert!(first > Duration::ZERO);
    tokio::time::sleep(first + Duration::from_millis(100)).await;

    within(sidecar.call(echo())).await.expect("generation 2");
    let State::Ready { generation, .. } = state.borrow().clone() else { panic!("not ready") };
    assert_eq!(generation, 2);

    within(sidecar.call(Request::new("host.crash", json!({ "code": 2 })))).await.expect_err("a crash");
    wait_state(&mut state, |state| !matches!(state, State::Ready { .. })).await;

    let error = sidecar.call(echo()).await.expect_err("a backoff");
    let SidecarError::Unavailable { retry_in: second } = error else { panic!("{error:?}") };
    assert!(second > first, "the second backoff of {second:?} grows past {first:?}");
    tokio::time::sleep(second + Duration::from_millis(100)).await;

    within(sidecar.call(echo())).await.expect("generation 3");
    let State::Ready { generation, .. } = state.borrow().clone() else { panic!("not ready") };
    assert_eq!(generation, 3);

    within(sidecar.call(Request::new("host.crash", json!({ "code": 3 })))).await.expect_err("a crash");
    wait_state(&mut state, |state| matches!(state, State::Disabled)).await;

    let error = sidecar.call(echo()).await.expect_err("a disabled sidecar");
    let SidecarError::Disabled { crashes } = error else { panic!("{error:?}") };
    assert_eq!(crashes, 3);
    assert!(matches!(*state.borrow(), State::Disabled));

    sidecar.shutdown().await;
}

#[tokio::test]
async fn shutdown_ends_the_host_and_its_children() {
    let config = support::config();
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");
    let pid = sidecar.pid().expect("a running host");
    let reply = within(sidecar.call(Request::new("host.children", Value::Null))).await.expect("a child");
    let child = u32::try_from(reply.value["pid"].as_u64().expect("a child pid")).expect("a child pid");

    within(sidecar.shutdown()).await;
    wait_gone(pid, Duration::from_secs(2)).await;
    wait_gone(child, Duration::from_secs(2)).await;

    let error = within(sidecar.call(echo())).await.expect_err("a stopped supervisor");
    assert!(matches!(error, SidecarError::ShuttingDown), "{error:?}");
}

#[tokio::test]
async fn shutdown_kills_a_host_that_does_not_read_its_end() {
    let config = support::config();
    let Some(_runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    within(sidecar.call(echo())).await.expect("a start");
    let pid = sidecar.pid().expect("a running host");

    let blocked = tokio::spawn({
        let sidecar = sidecar.clone();
        async move { sidecar.call(Request::new("host.block", json!({ "ms": 60_000 }))).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    tokio::time::timeout(Duration::from_secs(5), sidecar.shutdown()).await.expect("a shutdown in time");
    wait_gone(pid, Duration::from_secs(2)).await;
    assert!(within(blocked).await.expect("the call task").is_err(), "the blocked call does not answer");
}

#[tokio::test]
async fn end_of_input_ends_a_host_without_shutdown() {
    let config = support::config();
    let Some(runtime) = support::runtime(&config).await else { return };

    let mut spawned = spawn(&config, &runtime).await.expect("a spawned host");
    let message = Message::Call {
        id: 1,
        method: "host.children".to_owned(),
        location: None,
        parent: None,
        params: Value::Null,
        stream: false,
    };
    let bytes = frame::encode(&message, None, DEFAULT_MAX_FRAME).expect("a frame");
    spawned.stream.write_all(&bytes).await.expect("a call");
    let reply = frame::read(&mut spawned.stream, DEFAULT_MAX_FRAME).await.expect("a frame").expect("a reply");
    let child = match reply.message {
        Message::Result { value, .. } => {
            u32::try_from(value["pid"].as_u64().expect("a child pid")).expect("a child pid")
        }
        other => panic!("unexpected reply {other:?}"),
    };

    let copy = fcntl_dupfd_cloexec(spawned.stream.as_fd(), 0).expect("a socket to the host");
    let end = std::os::unix::net::UnixStream::from(copy);
    end.shutdown(std::net::Shutdown::Write).expect("the end of the host's input");

    tokio::time::timeout(Duration::from_secs(2), spawned.child.wait())
        .await
        .expect("the host exits on its own")
        .expect("the host exited");
    wait_gone(child, Duration::from_secs(2)).await;
}
