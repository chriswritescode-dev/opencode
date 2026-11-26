use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opencode_terminal::daemon::protocol::{AttachmentRole, ProcessMode, Request, Response};
use opencode_terminal::daemon::{Daemon, DaemonOptions, ErrorKind, Handoff, StreamEvent, SubscribeInput};
use opencode_util::temp::TempDir;
use tokio::time::{sleep, timeout};

use crate::fake_daemon::{Action, Connection, Handler, frame, listen, terminal, write_registration};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Runtime {
    _dir: TempDir,
    root: PathBuf,
    directory: PathBuf,
}

fn runtime() -> Runtime {
    let dir = TempDir::new().expect("a temporary directory");
    let root = dir.path().join("root");
    let directory = root.join("daemon");
    std::fs::create_dir_all(&directory).expect("create the runtime directory");
    Runtime { _dir: dir, root, directory }
}

fn options(root: &Path) -> DaemonOptions {
    DaemonOptions { root: root.to_owned(), id: "daemon".to_owned(), inherited: None, env: Vec::new() }
}

fn create_request() -> Request {
    Request::CreatePty {
        program: "/bin/true".to_owned(),
        args: Vec::new(),
        cwd: PathBuf::from("/tmp"),
        env: HashMap::new(),
        group_id: "ses_test".to_owned(),
        title: "test".to_owned(),
        cols: 80,
        rows: 24,
    }
}

#[derive(Clone)]
struct Rotation {
    token: String,
    instance: String,
}

fn pong(instance: &str) -> Response {
    Response::Pong { instance_id: instance.to_owned(), pid: std::process::id(), protocol: 9, features: Vec::new() }
}

#[tokio::test]
async fn rediscovers_a_same_protocol_daemon_after_its_registration_rotates() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let rotation =
        Arc::new(Mutex::new(Rotation { token: "old-token".to_owned(), instance: "old-instance".to_owned() }));
    let creates = Arc::new(AtomicU64::new(0));
    let handler: Handler = {
        let rotation = rotation.clone();
        let creates = creates.clone();
        Arc::new(move |request, token, _connection| {
            let current = rotation.lock().expect("rotation").clone();
            if token != current.token {
                return Action::Reply(Response::Error { message: "authentication failed".to_owned() });
            }
            match request {
                Request::Ping => Action::Reply(pong(&current.instance)),
                Request::List { .. } => Action::Reply(Response::Processes { processes: Vec::new() }),
                Request::CreatePty { .. } => {
                    creates.fetch_add(1, Ordering::SeqCst);
                    Action::Reply(Response::Ok)
                }
                _ => Action::Reply(Response::Ok),
            }
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "old-instance", "old-token").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    daemon.request(&Request::List { mode: ProcessMode::Pty }, false).await.expect("first list");

    *rotation.lock().expect("rotation") =
        Rotation { token: "new-token".to_owned(), instance: "new-instance".to_owned() };
    write_registration(&runtime.directory, &socket, "new-instance", "new-token").await;
    let running = daemon.request_if_running(&Request::List { mode: ProcessMode::Pty }).await.expect("discover");
    assert!(matches!(running, Some(Response::Processes { processes }) if processes.is_empty()));

    *rotation.lock().expect("rotation") =
        Rotation { token: "newest-token".to_owned(), instance: "newest-instance".to_owned() };
    write_registration(&runtime.directory, &socket, "newest-instance", "newest-token").await;
    daemon.request(&create_request(), true).await.expect("create");
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rediscovers_a_rotated_registration_when_acquiring_a_subscription() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let rotation =
        Arc::new(Mutex::new(Rotation { token: "old-token".to_owned(), instance: "old-instance".to_owned() }));
    let subscriptions = Arc::new(AtomicU64::new(0));
    let handler: Handler = {
        let rotation = rotation.clone();
        let subscriptions = subscriptions.clone();
        Arc::new(move |request, token, _connection| {
            let current = rotation.lock().expect("rotation").clone();
            if token != current.token {
                return Action::Reply(Response::Error { message: "authentication failed".to_owned() });
            }
            match request {
                Request::Ping => Action::Reply(pong(&current.instance)),
                Request::Subscribe { .. } => {
                    subscriptions.fetch_add(1, Ordering::SeqCst);
                    Action::Reply(Response::Attached {
                        process: terminal(1),
                        role: AttachmentRole::Observer,
                        generation: 1,
                        requested_offset: 0,
                        available_offset: 0,
                        end_offset: 0,
                        truncated: false,
                        replay_base64: String::new(),
                    })
                }
                _ => Action::Reply(Response::Processes { processes: Vec::new() }),
            }
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "old-instance", "old-token").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    daemon.request(&Request::List { mode: ProcessMode::Pty }, false).await.expect("list");

    *rotation.lock().expect("rotation") =
        Rotation { token: "new-token".to_owned(), instance: "new-instance".to_owned() };
    write_registration(&runtime.directory, &socket, "new-instance", "new-token").await;

    let attachment = daemon.subscribe(1, input()).await.expect("subscribe");
    assert_eq!(attachment.terminal.id, 1);
    assert_eq!(subscriptions.load(Ordering::SeqCst), 1);
    attachment.detach();
}

#[tokio::test]
async fn waits_on_its_own_connection_until_the_process_exits() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let waiting = Arc::new(Mutex::new(Vec::<Connection>::new()));
    let handler: Handler = {
        let waiting = waiting.clone();
        Arc::new(move |request, _token, connection| match request {
            Request::Ping => Action::Reply(pong("test")),
            Request::Wait { id } if *id == 1 => {
                waiting.lock().expect("waiting").push(connection);
                Action::Hold
            }
            Request::Wait { id } => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
            _ => Action::Reply(Response::Ok),
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let daemon = Arc::new(Daemon::open(options(&runtime.root)).await);

    let missing = daemon.wait(2).await.expect_err("missing terminal");
    assert_eq!(missing.kind, ErrorKind::Request);
    assert_eq!(missing.message, "terminal 2 not found");

    let waiter = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.wait(1).await }
    });
    wait_until(|| !waiting.lock().expect("waiting").is_empty()).await;
    waiting.lock().expect("waiting")[0].send(frame(&Response::Exited {
        exit_code: None,
        signal: Some("SIGTERM".to_owned()),
        final_offset: 3,
        output_lines: Some(1),
        output_error: None,
        exited_at: 7,
    }));
    let response = waiter.await.expect("wait task").expect("exited");
    match response {
        Response::Exited { exit_code, signal, final_offset, output_lines, exited_at, .. } => {
            assert_eq!(exit_code, None);
            assert_eq!(signal.as_deref(), Some("SIGTERM"));
            assert_eq!(final_offset, 3);
            assert_eq!(output_lines, Some(1));
            assert_eq!(exited_at, 7);
        }
        other => panic!("expected exited, got {other:?}"),
    }
}

#[tokio::test]
async fn closes_the_wait_connection_when_the_wait_is_interrupted() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let waiting = Arc::new(Mutex::new(Vec::<Connection>::new()));
    let handler: Handler = {
        let waiting = waiting.clone();
        Arc::new(move |request, _token, connection| match request {
            Request::Ping => Action::Reply(pong("test")),
            Request::Wait { .. } => {
                waiting.lock().expect("waiting").push(connection);
                Action::Hold
            }
            _ => Action::Reply(Response::Ok),
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let daemon = Arc::new(Daemon::open(options(&runtime.root)).await);

    let waiter = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.wait(1).await }
    });
    wait_until(|| !waiting.lock().expect("waiting").is_empty()).await;
    let connection = waiting.lock().expect("waiting")[0].clone();
    waiter.abort();
    let _ = waiter.await;
    timeout(TIMEOUT, connection.wait_closed()).await.expect("the wait connection closes");
}

#[tokio::test]
async fn reports_a_daemon_that_goes_away_during_a_wait_as_unreachable() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let handler: Handler = Arc::new(|request, _token, _connection| match request {
        Request::Ping => Action::Reply(pong("test")),
        _ => Action::Close,
    });
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    assert_eq!(daemon.wait(1).await.expect_err("wait must fail").kind, ErrorKind::Connect);
}

#[tokio::test]
async fn retries_a_start_required_request_when_connection_fails_before_dispatch() {
    let runtime = runtime();
    let first_socket = runtime.directory.join("first.sock");
    let second_socket = runtime.directory.join("second.sock");
    let creates = Arc::new(AtomicU64::new(0));
    let handler: Handler = {
        let creates = creates.clone();
        Arc::new(move |request, _token, _connection| match request {
            Request::Ping => Action::Reply(pong("test")),
            Request::CreatePty { .. } => {
                creates.fetch_add(1, Ordering::SeqCst);
                Action::Reply(Response::Ok)
            }
            _ => Action::Reply(Response::Processes { processes: Vec::new() }),
        })
    };
    let first = listen(&first_socket, handler.clone()).await;
    write_registration(&runtime.directory, &first_socket, "test", "test").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    daemon.request(&Request::List { mode: ProcessMode::Pty }, false).await.expect("list");
    first.close().await;

    let _second = listen(&second_socket, handler).await;
    write_registration(&runtime.directory, &second_socket, "test", "test").await;
    daemon.request(&create_request(), true).await.expect("create");
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_replay_a_dispatched_mutating_request_when_its_response_is_lost() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let creates = Arc::new(AtomicU64::new(0));
    let handler: Handler = {
        let creates = creates.clone();
        Arc::new(move |request, _token, _connection| match request {
            Request::Ping => Action::Reply(pong("test")),
            Request::CreatePty { .. } => {
                creates.fetch_add(1, Ordering::SeqCst);
                Action::Close
            }
            _ => Action::Reply(Response::Ok),
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    let error = daemon.request(&create_request(), true).await.expect_err("lost response");
    assert_eq!(error.kind, ErrorKind::Response);
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_incompatible_daemons_without_replacing_or_killing_them() {
    let runtime = runtime();
    let mut existing = std::process::Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the fixture process");
    let pid = existing.id();
    let registration = serde_json::json!({
        "instance_id": "old",
        "pid": pid,
        "protocol": 7,
        "socket": "/unused",
        "token": "old",
        "features": [],
    });
    std::fs::write(runtime.directory.join("service.json"), registration.to_string()).expect("write registration");
    let daemon = Daemon::open(DaemonOptions {
        root: runtime.root.clone(),
        id: "daemon".to_owned(),
        inherited: None,
        env: vec![("OPENCODE_PTY_BIN".to_owned(), "/missing/opencode-pty".to_owned())],
    })
    .await;

    let optional =
        daemon.request_if_running(&Request::List { mode: ProcessMode::Pty }).await.expect_err("incompatible protocol");
    assert_eq!(optional.kind, ErrorKind::Protocol);
    assert_eq!(optional.message, "opencode-pty protocol mismatch: daemon=7, client=9");
    assert_eq!(optional.pid, Some(pid));

    let starting = daemon.request(&create_request(), true).await.expect_err("incompatible protocol");
    assert_eq!(starting, optional);
    assert!(existing.try_wait().expect("poll").is_none(), "the existing daemon keeps running");
    let _ = existing.kill();
    let _ = existing.wait();
}

#[tokio::test]
async fn starts_fresh_instead_of_failing_when_an_inherited_registration_is_gone_or_expired() {
    let dir = TempDir::new().expect("a temporary directory");
    let legacy = dir.path().join("legacy");
    std::fs::create_dir_all(&legacy).expect("create the legacy directory");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock").as_millis() as u64;
    for expires_at in [now + 60_000, now - 1] {
        let daemon = Daemon::open(DaemonOptions {
            root: dir.path().join("root"),
            id: "fresh".to_owned(),
            inherited: Some(Handoff {
                directory: legacy.clone(),
                instance_id: "test".to_owned(),
                ticket: "ticket".to_owned(),
                expires_at,
            }),
            env: Vec::new(),
        })
        .await;
        assert!(daemon.handoff().await.expect("handoff").is_none());
        assert!(
            daemon
                .request_if_running(&Request::List { mode: ProcessMode::Pty })
                .await
                .expect("optional request")
                .is_none()
        );
    }
}

#[tokio::test]
async fn handoff_is_prepared_until_it_expires() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let handler: Handler = Arc::new(|request, _token, _connection| match request {
        Request::Ping => Action::Reply(pong("test")),
        Request::PrepareHandoff => {
            Action::Reply(Response::Handoff { ticket: "next".to_owned(), expires_at: now_ms() + 120 })
        }
        _ => Action::Reply(Response::Processes { processes: Vec::new() }),
    });
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let daemon = Daemon::open(options(&runtime.root)).await;
    daemon.request(&Request::List { mode: ProcessMode::Pty }, false).await.expect("list");

    let handoff = daemon.handoff().await.expect("handoff").expect("a prepared handoff");
    assert_eq!(handoff.directory, runtime.root.join("daemon"));
    assert_eq!(handoff.instance_id, "test");
    assert_eq!(handoff.ticket, "next");
    assert!(daemon.handoff_prepared().await);
    sleep(Duration::from_millis(200)).await;
    assert!(!daemon.handoff_prepared().await);
}

#[tokio::test]
async fn stops_a_discovered_inherited_daemon_when_adoption_fails() {
    let runtime = runtime();
    let socket = runtime.directory.join("daemon.sock");
    let shutdowns = Arc::new(AtomicU64::new(0));
    let handler: Handler = {
        let shutdowns = shutdowns.clone();
        Arc::new(move |request, _token, _connection| match request {
            Request::Ping => Action::Reply(pong("test")),
            Request::Shutdown => {
                shutdowns.fetch_add(1, Ordering::SeqCst);
                Action::Reply(Response::Ok)
            }
            _ => Action::Reply(Response::Ok),
        })
    };
    let _server = listen(&socket, handler).await;
    write_registration(&runtime.directory, &socket, "test", "test").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock").as_millis() as u64;
    let _daemon = Daemon::open(DaemonOptions {
        root: runtime.root.clone(),
        id: "fresh".to_owned(),
        inherited: Some(Handoff {
            directory: runtime.directory.clone(),
            instance_id: "replaced".to_owned(),
            ticket: "ticket".to_owned(),
            expires_at: now + 60_000,
        }),
        env: Vec::new(),
    })
    .await;
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gives_up_at_once_on_a_daemon_binary_that_speaks_another_protocol() {
    let dir = TempDir::new().expect("a temporary directory");
    let runtime = dir.path().join("runtime");
    std::fs::create_dir_all(&runtime).expect("create the runtime root");
    let spawns = dir.path().join("spawns");
    let binary = dir.path().join("old-opencode-pty");
    let script = format!(
        "#!/bin/sh\necho spawn >> {spawns}\nmkdir -p \"$3/$5\"\nprintf '{{\"instance_id\":\"old\",\"pid\":%s,\"protocol\":7,\"socket\":\"none\",\"token\":\"t\",\"features\":[]}}' $$ > \"$3/$5/service.json\"\nexec sleep 30\n",
        spawns = shell_quote(&spawns),
    );
    std::fs::write(&binary, script).expect("write the old binary");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    }
    let daemon = Daemon::open(DaemonOptions {
        root: runtime,
        id: "daemon".to_owned(),
        inherited: None,
        env: vec![("OPENCODE_PTY_BIN".to_owned(), binary.to_string_lossy().into_owned())],
    })
    .await;

    let started = Instant::now();
    let first =
        daemon.request(&Request::List { mode: ProcessMode::Pipe }, true).await.expect_err("incompatible protocol");
    assert_eq!(first.kind, ErrorKind::Protocol);
    let second =
        daemon.request(&Request::List { mode: ProcessMode::Pipe }, true).await.expect_err("incompatible protocol");
    assert_eq!(second.kind, ErrorKind::Protocol);
    assert!(started.elapsed() < Duration::from_secs(4), "the daemon was not waited on again");
    let count = std::fs::read_to_string(&spawns).expect("spawn log");
    assert_eq!(count.lines().count(), 1);
}

#[tokio::test]
async fn runs_create_subscribe_handoff_and_adopt_against_the_real_daemon() {
    if std::env::var("OPENCODE_PTY_BIN").is_err() {
        eprintln!("OPENCODE_PTY_BIN is not set; skipping the real-daemon test");
        return;
    }
    timeout(Duration::from_secs(40), real_daemon()).await.expect("the real daemon test finishes");
}

async fn real_daemon() {
    let dir = TempDir::new().expect("a temporary directory");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("create the runtime root");
    let env: Vec<(String, String)> = std::env::vars().collect();
    let first = Daemon::open(DaemonOptions {
        root: root.clone(),
        id: "server-a".to_owned(),
        inherited: None,
        env: env.clone(),
    })
    .await;
    let create = Request::CreatePty {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "sleep 0.3; printf 'ready\\n'; exec cat".to_owned()],
        cwd: dir.path().to_owned(),
        env: HashMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned()), ("TERM".to_owned(), "xterm".to_owned())]),
        group_id: "ses_test".to_owned(),
        title: "persistent".to_owned(),
        cols: 80,
        rows: 24,
    };
    let created = first.request(&create, true).await.expect("create a real PTY");
    let Response::Created { process } = created else { panic!("expected created, got {created:?}") };
    assert_eq!(process.mode, ProcessMode::Pty);

    let output = Arc::new(Mutex::new(String::new()));
    let attachment = first
        .subscribe(
            process.id,
            SubscribeInput {
                cursor: 0,
                attachment_id: "attachment".to_owned(),
                role: AttachmentRole::Observer,
                takeover: false,
                on_event: {
                    let output = output.clone();
                    Arc::new(move |event| {
                        if let StreamEvent::Output { data, .. } = event {
                            output.lock().expect("output").push_str(&String::from_utf8_lossy(&data));
                        }
                    })
                },
                on_end: Arc::new(|| {}),
            },
        )
        .await
        .expect("subscribe to the real PTY");
    assert_eq!(attachment.terminal.id, process.id);
    attachment.activate();
    let replay = String::from_utf8_lossy(&attachment.replay.data).into_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if replay.contains("ready") || output.lock().expect("output").contains("ready") {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(
        replay.contains("ready") || output.lock().expect("output").contains("ready"),
        "the real daemon produced no output: replay={replay:?}"
    );
    attachment.detach();

    let handoff = first.handoff().await.expect("prepare a handoff").expect("a prepared handoff");
    assert_eq!(handoff.instance_id, first.registration().await.expect("registration").instance_id);
    assert!(handoff.expires_at > 0);
    assert!(first.handoff_prepared().await);

    let second =
        Daemon::open(DaemonOptions { root, id: "server-b".to_owned(), inherited: Some(handoff), env: env.clone() })
            .await;
    let listed = second
        .request(&Request::List { mode: ProcessMode::Pty }, false)
        .await
        .expect("list through the adopted daemon");
    let Response::Processes { processes } = listed else { panic!("expected processes, got {listed:?}") };
    assert!(processes.iter().any(|item| item.id == process.id));

    second.shutdown().await.expect("shut the daemon down");
    assert!(!second.handoff_prepared().await);
    assert!(
        second.request_if_running(&Request::List { mode: ProcessMode::Pty }).await.expect("optional list").is_none()
    );
}

fn input() -> SubscribeInput {
    SubscribeInput {
        cursor: 0,
        attachment_id: "attachment".to_owned(),
        role: AttachmentRole::Observer,
        takeover: false,
        on_event: Arc::new(|_| {}),
        on_end: Arc::new(|| {}),
    }
}

async fn wait_until(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !ready() {
        assert!(Instant::now() < deadline, "condition was not met in time");
        sleep(Duration::from_millis(10)).await;
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock").as_millis() as u64
}
