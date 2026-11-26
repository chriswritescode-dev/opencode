use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_storage::{Database, DatabaseLocation};
use opencode_terminal::daemon::Handoff;
use opencode_terminal::daemon::protocol::{
    AttachmentRole, PipeInfo as DaemonPipeInfo, PipeMetadata, ProcessInfo, ProcessMode, ProcessSignal, Request,
    Response, TerminalLifecycle,
};
use opencode_terminal::persistent::{AttachInput, CreateInput, Options, PipeLifecycle};
use opencode_terminal::persistent::{CreatePipeInput, PersistentPty, PersistentPtyError};
use opencode_util::temp::TempDir;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::fake_daemon::{Action, Connection, FakeServer, Handler, listen, terminal, write_registration};

struct State {
    processes: Vec<ProcessInfo>,
    next_id: u64,
    waiters: HashMap<u64, Vec<Connection>>,
    fail_terminate: HashSet<String>,
    handoff_ttl: u64,
    ops: Vec<String>,
}

impl State {
    fn find(&self, id: u64) -> Option<&ProcessInfo> {
        self.processes.iter().find(|process| process.id == id)
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut ProcessInfo> {
        self.processes.iter_mut().find(|process| process.id == id)
    }

    fn exit(&mut self, id: u64, exit_code: Option<u32>, signal: Option<String>) {
        let Some(process) = self.find_mut(id) else { return };
        if process.lifecycle != TerminalLifecycle::Running {
            return;
        }
        process.lifecycle = TerminalLifecycle::Exited { exit_code, signal };
        process.exited_at = Some(now_ms());
        let reply = exited(process);
        if let Some(waiters) = self.waiters.remove(&id) {
            for waiter in waiters {
                waiter.send(crate::fake_daemon::frame(&reply));
            }
        }
    }
}

struct Harness {
    _dir: TempDir,
    _server: FakeServer,
    service: Arc<PersistentPty>,
    bus: Arc<Bus>,
    state: Arc<Mutex<State>>,
    root: PathBuf,
}

async fn harness() -> Harness {
    let dir = TempDir::new().expect("a temporary directory");
    let root = dir.path().join("root");
    let directory = root.join("inherited");
    let socket = dir.path().join("daemon.sock");
    std::fs::create_dir_all(&directory).expect("create the daemon directory");
    let state = Arc::new(Mutex::new(State {
        processes: Vec::new(),
        next_id: 1,
        waiters: HashMap::new(),
        fail_terminate: HashSet::new(),
        handoff_ttl: 60_000,
        ops: Vec::new(),
    }));
    let handler: Handler = {
        let state = state.clone();
        let directory = directory.clone();
        Arc::new(move |request, _token, connection| handle(&state, &directory, request, connection))
    };
    let server = listen(&socket, handler).await;
    write_registration(&directory, &socket, "test", "test").await;
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let bus = Bus::new(Arc::new(database), BusOptions::default());
    let service = PersistentPty::new(
        Options {
            root: root.clone(),
            id: "inherited".to_owned(),
            env: Vec::new(),
            inherited: Some(Handoff {
                directory: directory.clone(),
                instance_id: "test".to_owned(),
                ticket: "ticket".to_owned(),
                expires_at: now_ms() + 60_000,
            }),
            default_command: "/bin/sh".to_owned(),
        },
        bus.clone(),
    );
    Harness { _dir: dir, _server: server, service, bus, state, root }
}

fn handle(state: &Arc<Mutex<State>>, directory: &Path, request: &Request, connection: Connection) -> Action {
    let mut state = state.lock().expect("the fake daemon state is never poisoned");
    state.ops.push(request.op().to_owned());
    match request {
        Request::Ping => Action::Reply(pong()),
        Request::List { mode } => Action::Reply(Response::Processes {
            processes: state.processes.iter().filter(|process| process.mode == *mode).cloned().collect(),
        }),
        Request::Get { id } => match state.find(*id) {
            Some(process) => Action::Reply(Response::Process { process: process.clone() }),
            None => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::CreatePty { group_id, title, cols, rows, .. } => {
            let mut process = terminal(state.next_id);
            state.next_id += 1;
            process.group_id = group_id.clone();
            if let Some(pty) = &mut process.pty {
                pty.title = title.clone();
                pty.cols = *cols;
                pty.rows = *rows;
            }
            state.processes.push(process.clone());
            Action::Reply(Response::Created { process })
        }
        Request::CreatePipe { program, args, cwd, group_id, output_path, metadata, .. } => {
            if !cwd.is_absolute() {
                return Action::Reply(Response::Error { message: "cwd must be absolute".to_owned() });
            }
            let mut command = vec![program.clone()];
            command.extend(args.iter().cloned());
            let process = ProcessInfo {
                id: state.next_id,
                mode: ProcessMode::Pipe,
                pid: Some(1_000 + state.next_id as u32),
                command,
                cwd: cwd.clone(),
                group_id: group_id.clone(),
                lifecycle: TerminalLifecycle::Running,
                output_head: 0,
                output_tail: 0,
                started_at: now_ms(),
                exited_at: None,
                pty: None,
                pipe: Some(DaemonPipeInfo {
                    output_path: output_path.clone(),
                    output_lines: 0,
                    output_error: None,
                    metadata: metadata.clone(),
                }),
            };
            state.next_id += 1;
            let _ = std::fs::write(output_path, "");
            state.processes.push(process.clone());
            Action::Reply(Response::Created { process })
        }
        Request::Control { .. } => Action::Reply(Response::Ok),
        Request::Subscribe { id, role, .. } => match state.find(*id) {
            Some(process) if process.mode == ProcessMode::Pty => Action::Reply(Response::Attached {
                process: process.clone(),
                role: *role,
                generation: 1,
                requested_offset: 0,
                available_offset: 0,
                end_offset: 0,
                truncated: false,
                replay_base64: String::new(),
            }),
            _ => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::Shutdown => {
            let _ = std::fs::remove_file(directory.join("service.json"));
            Action::Reply(Response::Ok)
        }
        Request::PrepareHandoff => {
            Action::Reply(Response::Handoff { ticket: "next".to_owned(), expires_at: now_ms() + state.handoff_ttl })
        }
        Request::Rename { id, title } => match state.find_mut(*id) {
            Some(process) => {
                if let Some(pty) = &mut process.pty {
                    pty.user_title = title.clone().filter(|title| !title.is_empty());
                    pty.title = pty.user_title.clone().unwrap_or_else(|| "test".to_owned());
                }
                Action::Reply(Response::Ok)
            }
            None => Action::Reply(Response::Error { message: "terminal not found".to_owned() }),
        },
        Request::Wait { id } => match state.find(*id) {
            Some(process) if process.mode == ProcessMode::Pipe => {
                if process.lifecycle == TerminalLifecycle::Running {
                    state.waiters.entry(*id).or_default().push(connection);
                    Action::Hold
                } else {
                    Action::Reply(exited(process))
                }
            }
            _ => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::Signal { id, signal } => match state.find(*id) {
            Some(process) if process.mode == ProcessMode::Pipe => {
                if *signal == ProcessSignal::Term {
                    state.exit(*id, None, Some("SIGTERM".to_owned()));
                }
                Action::Reply(Response::Ok)
            }
            _ => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::SetMetadata { id, metadata } => match state.find_mut(*id) {
            Some(process) if process.pipe.is_some() => {
                if let Some(pipe) = &mut process.pipe {
                    pipe.metadata = metadata.clone();
                }
                Action::Reply(Response::Ok)
            }
            _ => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::Terminate { id } => {
            if state.fail_terminate.contains(&format!("pty_persistent_{id}")) {
                return Action::Reply(Response::Error { message: "terminate failed".to_owned() });
            }
            let index = state.processes.iter().position(|process| process.id == *id);
            match index {
                Some(index) => {
                    state.processes.remove(index);
                    if let Some(waiters) = state.waiters.remove(id) {
                        for waiter in waiters {
                            waiter
                                .send(crate::fake_daemon::frame(&Response::Error { message: "terminated".to_owned() }));
                        }
                    }
                    Action::Reply(Response::Ok)
                }
                None => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
            }
        }
        _ => Action::Reply(Response::Error { message: format!("unsupported request: {}", request.op()) }),
    }
}

fn pong() -> Response {
    Response::Pong { instance_id: "test".to_owned(), pid: std::process::id(), protocol: 9, features: Vec::new() }
}

fn exited(process: &ProcessInfo) -> Response {
    let (exit_code, signal) = match &process.lifecycle {
        TerminalLifecycle::Exited { exit_code, signal } => (*exit_code, signal.clone()),
        _ => (None, None),
    };
    Response::Exited {
        exit_code,
        signal,
        final_offset: process.output_tail,
        output_lines: process.pipe.as_ref().map(|pipe| pipe.output_lines),
        output_error: process.pipe.as_ref().and_then(|pipe| pipe.output_error.clone()),
        exited_at: process.exited_at.unwrap_or_else(now_ms),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("the condition did not become true");
}

fn create_input(directory: &Path) -> CreateInput {
    CreateInput {
        command: Some("/bin/sh".to_owned()),
        args: Vec::new(),
        cwd: Some(directory.to_string_lossy().into_owned()),
        title: "test".to_owned(),
        env: HashMap::new(),
        cols: 80,
        rows: 24,
    }
}

fn pipe_input(directory: &Path) -> CreatePipeInput {
    CreatePipeInput {
        program: "/usr/bin/env".to_owned(),
        args: vec!["--version".to_owned()],
        cwd: directory.to_owned(),
        env: HashMap::from([("TEST".to_owned(), "1".to_owned())]),
        group_id: "proj_test".to_owned(),
        output_path: directory.join("sh_test.out"),
        metadata: metadata(&[("id", json!("sh_test"))]),
    }
}

fn metadata(entries: &[(&str, Value)]) -> PipeMetadata {
    entries.iter().map(|(key, value)| ((*key).to_owned(), value.clone())).collect()
}

fn publish_deleted(bus: &Bus, session_id: &str) {
    bus.publish(
        &Definition::durable("session.deleted", 2, "sessionID"),
        json!({ "sessionID": session_id }),
        PublishOptions::default(),
    )
    .expect("publish session.deleted");
}

#[tokio::test]
async fn removes_a_deleted_sessions_terminals_and_leaves_the_others() {
    let harness = harness().await;
    let directory = harness.root.join("work");
    std::fs::create_dir_all(&directory).expect("create the terminal directory");
    let deleted = harness.service.create("ses_deleted", create_input(&directory)).await.expect("deleted");
    let other = harness.service.create("ses_kept", create_input(&directory)).await.expect("kept");
    let mut removed = harness.bus.subscribe(&["persistent-pty.removed"], None);

    publish_deleted(&harness.bus, "ses_deleted");

    let event =
        tokio::time::timeout(Duration::from_secs(5), removed.recv()).await.expect("removed event").expect("event");
    assert_eq!(event.data()["sessionID"], json!("ses_deleted"));
    assert_eq!(event.data()["ptyID"], json!(deleted.id.0));
    assert!(harness.service.list(Some("ses_deleted")).await.expect("list").is_empty());
    let listed = harness.service.list(None).await.expect("list");
    assert_eq!(listed.into_iter().map(|terminal| terminal.id.0).collect::<Vec<_>>(), [other.id.0]);
}

#[tokio::test]
async fn keeps_cleaning_later_deletions_after_a_terminal_fails_to_stop() {
    let harness = harness().await;
    let directory = harness.root.join("work");
    std::fs::create_dir_all(&directory).expect("create the terminal directory");
    let first = harness.service.create("ses_first", create_input(&directory)).await.expect("first");
    let second = harness.service.create("ses_second", create_input(&directory)).await.expect("second");
    harness.service.control(&first.id.0, "att", 80, 24).await.expect("select");
    harness.state.lock().expect("state").fail_terminate.insert(first.id.0.clone());
    let mut removed = harness.bus.subscribe(&["persistent-pty.removed"], None);

    publish_deleted(&harness.bus, "ses_first");
    publish_deleted(&harness.bus, "ses_second");

    let event =
        tokio::time::timeout(Duration::from_secs(5), removed.recv()).await.expect("removed event").expect("event");
    assert_eq!(event.data()["ptyID"], json!(second.id.0));
    let listed = harness.service.list(None).await.expect("list");
    assert_eq!(listed.into_iter().map(|terminal| terminal.id.0).collect::<Vec<_>>(), [first.id.0]);
    assert_eq!(harness.service.read("ses_first", None).await.expect("read"), None);
}

#[tokio::test]
async fn renames_a_terminal_and_publishes_the_update() {
    let harness = harness().await;
    let directory = harness.root.join("work");
    std::fs::create_dir_all(&directory).expect("create the terminal directory");
    let terminal = harness.service.create("ses_rename", create_input(&directory)).await.expect("create");
    assert_eq!(terminal.user_title, None);
    let mut updated = harness.bus.subscribe(&["persistent-pty.updated"], None);

    let renamed = harness.service.rename(&terminal.id.0, Some("build")).await.expect("rename");
    assert_eq!(renamed.id, terminal.id);
    assert_eq!(renamed.title, "build");
    assert_eq!(renamed.user_title.as_deref(), Some("build"));
    let event =
        tokio::time::timeout(Duration::from_secs(5), updated.recv()).await.expect("updated event").expect("event");
    assert_eq!(event.data()["sessionID"], json!("ses_rename"));
    assert_eq!(event.data()["terminal"]["id"], json!(terminal.id.0));

    let cleared = harness.service.rename(&terminal.id.0, None).await.expect("clear");
    assert_eq!(cleared.title, "test");
    assert_eq!(cleared.user_title, None);
}

#[tokio::test]
async fn creates_inspects_signals_and_waits_for_a_pipe_process() {
    let harness = harness().await;
    let input = pipe_input(&harness.root);
    let created = harness.service.create_pipe(input.clone()).await.expect("create pipe");
    assert_eq!(created.group_id, "proj_test");
    assert_eq!(created.output_path, input.output_path);
    assert!(matches!(created.lifecycle, PipeLifecycle::Running));
    assert_eq!(created.exited_at, None);
    assert_eq!(created.metadata, input.metadata);

    harness
        .service
        .set_pipe_metadata(created.id, metadata(&[("id", json!("sh_test")), ("status", json!("killed"))]))
        .await
        .expect("set metadata");
    assert_eq!(
        harness.service.get_pipe(created.id).await.expect("get pipe").metadata,
        metadata(&[("id", json!("sh_test")), ("status", json!("killed"))])
    );
    assert_eq!(
        harness.service.list_pipes().await.expect("list pipes").into_iter().map(|pipe| pipe.id).collect::<Vec<_>>(),
        [created.id]
    );

    let service = harness.service.clone();
    let waiting = tokio::spawn(async move { service.wait_pipe(created.id).await });
    wait_for(|| {
        harness.state.lock().expect("state").waiters.get(&created.id).is_some_and(|waiters| !waiters.is_empty())
    })
    .await;
    harness.service.signal_pipe(created.id, ProcessSignal::Term).await.expect("signal");
    let exit = waiting.await.expect("wait task").expect("exited");
    assert_eq!(exit.exit_code, None);
    assert_eq!(exit.signal.as_deref(), Some("SIGTERM"));
    assert_eq!(exit.output_lines, 0);
    assert_eq!(harness.service.wait_pipe(created.id).await.expect("wait again").signal.as_deref(), Some("SIGTERM"));
    assert!(matches!(
        harness.service.get_pipe(created.id).await.expect("get").lifecycle,
        PipeLifecycle::Exited { exit_code: None, signal: Some(ref signal) } if signal == "SIGTERM"
    ));

    harness.service.terminate_pipe(created.id).await.expect("terminate");
    assert_eq!(
        harness.service.get_pipe(created.id).await.expect_err("gone"),
        PersistentPtyError::ProcessNotFound { process_id: created.id }
    );
    assert!(harness.service.list_pipes().await.expect("list").is_empty());
}

#[tokio::test]
async fn keeps_pipe_processes_and_terminals_apart() {
    let harness = harness().await;
    let directory = harness.root.join("work");
    std::fs::create_dir_all(&directory).expect("create the terminal directory");
    let terminal = harness.service.create("ses_pipe", create_input(&directory)).await.expect("terminal");
    let pipe = harness.service.create_pipe(pipe_input(&directory)).await.expect("pipe");
    assert_eq!(
        harness
            .service
            .list(Some("ses_pipe"))
            .await
            .expect("terminals")
            .into_iter()
            .map(|item| item.id.0)
            .collect::<Vec<_>>(),
        [terminal.id.0]
    );
    assert_eq!(
        harness.service.list_pipes().await.expect("pipes").into_iter().map(|item| item.id).collect::<Vec<_>>(),
        [pipe.id]
    );
    let pty_id = format!("pty_persistent_{}", pipe.id);
    assert_eq!(
        harness.service.get(&pty_id).await.expect_err("pipe is not a terminal"),
        PersistentPtyError::NotFound { pty_id }
    );
}

#[tokio::test]
async fn reports_daemon_rejections_as_request_errors() {
    let harness = harness().await;
    let input = pipe_input(&harness.root);
    assert_eq!(
        harness
            .service
            .create_pipe(CreatePipeInput { cwd: PathBuf::from("relative"), ..input.clone() })
            .await
            .expect_err("relative cwd"),
        PersistentPtyError::Request { message: "cwd must be absolute".to_owned() }
    );
    let created = harness.service.create_pipe(input).await.expect("create");
    let service = harness.service.clone();
    let waiting = tokio::spawn(async move { service.wait_pipe(created.id).await });
    wait_for(|| {
        harness.state.lock().expect("state").waiters.get(&created.id).is_some_and(|waiters| !waiters.is_empty())
    })
    .await;
    harness.service.terminate_pipe(created.id).await.expect("terminate");
    assert_eq!(
        waiting.await.expect("wait task").expect_err("terminated"),
        PersistentPtyError::Request { message: "terminated".to_owned() }
    );
}

#[tokio::test]
async fn knows_a_handoff_is_prepared_until_a_shutdown_or_expiry() {
    let harness = harness().await;
    assert!(!harness.service.handoff_prepared().await);
    harness.service.create_pipe(pipe_input(&harness.root)).await.expect("create pipe");
    assert!(harness.service.handoff().await.expect("handoff").is_some());
    assert!(harness.service.handoff_prepared().await);
    harness.service.shutdown().await.expect("shutdown");
    assert!(!harness.service.handoff_prepared().await);

    let expiring = harness_with_ttl(30).await;
    expiring.service.create_pipe(pipe_input(&expiring.root)).await.expect("create pipe");
    assert!(expiring.service.handoff().await.expect("handoff").is_some());
    assert!(expiring.service.handoff_prepared().await);
    sleep(Duration::from_millis(80)).await;
    assert!(!expiring.service.handoff_prepared().await);
}

async fn harness_with_ttl(ttl: u64) -> Harness {
    let harness = harness().await;
    harness.state.lock().expect("state").handoff_ttl = ttl;
    harness
}

#[tokio::test]
async fn prepares_a_handoff_more_than_once_on_the_same_owner_connection() {
    let harness = harness().await;
    harness.service.create_pipe(pipe_input(&harness.root)).await.expect("create pipe");
    assert_eq!(harness.service.handoff().await.expect("first").expect("handoff").ticket, "next");
    assert_eq!(harness.service.handoff().await.expect("second").expect("handoff").ticket, "next");
}

#[tokio::test]
async fn attaches_and_reports_a_controller_attachment() {
    let harness = harness().await;
    let directory = harness.root.join("work");
    std::fs::create_dir_all(&directory).expect("create the terminal directory");
    let terminal = harness.service.create("ses_attach", create_input(&directory)).await.expect("terminal");
    let attached = Arc::new(Mutex::new(Vec::<String>::new()));
    let events = attached.clone();
    let ended = attached.clone();
    let attachment = harness
        .service
        .attach(
            &terminal.id.0,
            AttachInput {
                cursor: 0,
                attachment_id: "att".to_owned(),
                role: AttachmentRole::Controller,
                takeover: true,
                on_event: Arc::new(move |event| events.lock().expect("events").push(format!("{event:?}"))),
                on_end: Arc::new(move || ended.lock().expect("events").push("end".to_owned())),
            },
        )
        .await
        .expect("attach");
    assert_eq!(attachment.role, AttachmentRole::Controller);
    attachment.detach();
}
