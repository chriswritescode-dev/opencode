use std::collections::{HashMap, HashSet};
use std::fs::{File, FileTimes};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opencode_bus::{Bus, BusError, BusOptions, Event};
use opencode_config::shell::{Search, ShellSelect};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{AbsolutePath, LocationRef, ShellInfoStatus};
use opencode_shell::daemon_pipe::DaemonCommands;
use opencode_shell::output::Limits;
use opencode_shell::retention::EXITED_LIMIT;
use opencode_shell::service::{CreateInput, Options, Shell, ShellError, StopReason};
use opencode_storage::{Database, DatabaseLocation};
use opencode_terminal::daemon::protocol::{
    PipeInfo, PipeMetadata, ProcessInfo, ProcessMode, ProcessSignal, Request, Response, TerminalLifecycle,
};
use opencode_terminal::persistent::{self, PersistentPty};
use opencode_util::lock;
use opencode_util::temp::TempDir;
use serde_json::json;

use crate::fake_daemon::{Action, Connection, FakeServer, Handler, frame, listen, write_registration};

#[derive(Clone)]
struct CreateRecord {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: HashMap<String, String>,
    group_id: String,
    output_path: PathBuf,
    metadata: PipeMetadata,
}

struct State {
    processes: Vec<ProcessInfo>,
    next_id: u64,
    waiters: HashMap<u64, Vec<Connection>>,
    trap: HashSet<u64>,
    fail_create: Option<String>,
    creates: Vec<CreateRecord>,
    ops: Vec<String>,
    signals: Vec<(u64, ProcessSignal)>,
    metadata_updates: Vec<(u64, PipeMetadata)>,
    hold_list: bool,
    held_lists: Vec<Connection>,
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
                waiter.send(frame(&reply));
            }
        }
    }

    fn output(&mut self, id: u64, text: &str) {
        let Some(process) = self.find_mut(id) else { return };
        let Some(pipe) = &mut process.pipe else { return };
        if pipe.output_error.is_none() {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&pipe.output_path)
                .expect("the daemon writes the output file");
            file.write_all(text.as_bytes()).expect("the daemon writes the output file");
            pipe.output_lines += text.matches('\n').count() as u64;
        }
        process.output_tail += text.len() as u64;
    }

    fn fail(&mut self, id: u64, message: &str) {
        let Some(process) = self.find_mut(id) else { return };
        if let Some(pipe) = &mut process.pipe {
            pipe.output_error = Some(message.to_owned());
        }
    }

    fn release_lists(&mut self) {
        let processes = self.processes.clone();
        for connection in std::mem::take(&mut self.held_lists) {
            connection.send(frame(&Response::Processes { processes: processes.clone() }));
        }
    }

    fn drop_wait(&mut self, id: u64) {
        if let Some(waiters) = self.waiters.remove(&id) {
            for waiter in waiters {
                waiter.close();
            }
        }
    }

    fn exit_name(signal: ProcessSignal) -> &'static str {
        match signal {
            ProcessSignal::Term => "SIGTERM",
            ProcessSignal::Int => "SIGINT",
            ProcessSignal::Kill => "SIGKILL",
        }
    }
}

struct Harness {
    _dir: TempDir,
    _server: FakeServer,
    state: Arc<Mutex<State>>,
    pty: Arc<PersistentPty>,
    commands: Arc<DaemonCommands>,
    bus: Arc<Bus>,
    data: PathBuf,
    directory: PathBuf,
    project: String,
    shell: Vec<(String, String)>,
}

async fn harness() -> Harness {
    let dir = TempDir::new().expect("a temporary directory");
    let root = dir.path().join("state");
    let daemon_directory = root.join("daemon");
    std::fs::create_dir_all(&daemon_directory).expect("create the daemon directory");
    let socket = dir.path().join("daemon.sock");
    let state = Arc::new(Mutex::new(State {
        processes: Vec::new(),
        next_id: 1,
        waiters: HashMap::new(),
        trap: HashSet::new(),
        fail_create: None,
        creates: Vec::new(),
        ops: Vec::new(),
        signals: Vec::new(),
        metadata_updates: Vec::new(),
        hold_list: false,
        held_lists: Vec::new(),
    }));
    let handler: Handler = {
        let state = state.clone();
        let daemon_directory = daemon_directory.clone();
        Arc::new(move |request, _token, connection| handle(&state, &daemon_directory, request, connection))
    };
    let server = listen(&socket, handler).await;
    write_registration(&daemon_directory, &socket, "test", "test").await;
    let bus = bus(dir.path());
    let pty = PersistentPty::new(
        persistent::Options {
            root,
            id: "daemon".to_owned(),
            env: vec![("PATH".to_owned(), String::new())],
            inherited: None,
            default_command: "/bin/sh".to_owned(),
        },
        bus.clone(),
    );
    let commands = DaemonCommands::new(pty.clone(), bus.clone());
    let directory = dir.path().join("work");
    std::fs::create_dir_all(&directory).expect("create the working directory");
    let data = dir.path().join("data");
    Harness {
        _dir: dir,
        _server: server,
        state,
        pty,
        commands,
        bus,
        data,
        directory,
        project: "prj_shell".to_owned(),
        shell: vec![
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("OPENCODE_TEST_REMOVED".to_owned(), "1".to_owned()),
        ],
    }
}

impl Harness {
    async fn open_in(&self, directory: &Path) -> Arc<Shell> {
        self.open_in_with(directory, self.commands.clone()).await
    }

    async fn open_in_with(&self, directory: &Path, commands: Arc<DaemonCommands>) -> Arc<Shell> {
        Shell::new(self.bus.clone(), self.options(directory, commands)).await
    }

    async fn open(&self) -> Arc<Shell> {
        self.open_in(&self.directory).await
    }

    fn options(&self, directory: &Path, commands: Arc<DaemonCommands>) -> Options {
        let select = Arc::new(ShellSelect::new(
            Search { bin: PathBuf::from("/usr/bin"), path: std::env::var_os("PATH"), gitbash: None },
            |name| std::env::var_os(name),
        ));
        Options {
            location: LocationRef {
                directory: AbsolutePath(directory.to_string_lossy().into_owned()),
                workspace_id: None,
            },
            project_id: self.project.clone(),
            data: self.data.clone(),
            env: self.shell.clone(),
            shell: select,
            limits: Arc::new(Limits::default),
            daemon: Some(commands),
        }
    }

    fn hold_next_list(&self) {
        lock(&self.state).hold_list = true;
    }

    fn held_lists(&self) -> usize {
        lock(&self.state).held_lists.len()
    }

    fn release_lists(&self) {
        lock(&self.state).release_lists();
    }

    fn list_ops(&self) -> usize {
        self.ops().iter().filter(|op| op.as_str() == "list").count()
    }

    fn new_server(&self) -> Arc<DaemonCommands> {
        DaemonCommands::new(self.pty.clone(), bus(self._dir.path()))
    }

    fn input(&self, command: &str) -> CreateInput {
        CreateInput {
            command: command.to_owned(),
            cwd: None,
            timeout: None,
            metadata: None,
            shell: Some("/bin/sh".to_owned()),
            tool_call: None,
            waited: false,
        }
    }

    fn created(&self) -> Vec<CreateRecord> {
        lock(&self.state).creates.clone()
    }

    fn ops(&self) -> Vec<String> {
        lock(&self.state).ops.clone()
    }

    fn signals(&self, id: u64) -> Vec<ProcessSignal> {
        lock(&self.state).signals.iter().filter(|(candidate, _)| *candidate == id).map(|(_, signal)| *signal).collect()
    }

    fn processes(&self) -> Vec<u64> {
        lock(&self.state).processes.iter().map(|process| process.id).collect()
    }

    fn process_metadata(&self, id: u64) -> Option<PipeMetadata> {
        lock(&self.state).find(id).and_then(|process| process.pipe.as_ref()).map(|pipe| pipe.metadata.clone())
    }

    fn output(&self, id: u64, text: &str) {
        lock(&self.state).output(id, text);
    }

    fn fail(&self, id: u64, message: &str) {
        lock(&self.state).fail(id, message);
    }

    fn exit(&self, id: u64, code: u32) {
        lock(&self.state).exit(id, Some(code), None);
    }

    async fn until_waiting(&self, id: u64) {
        wait_until(|| lock(&self.state).waiters.get(&id).is_some_and(|waiters| !waiters.is_empty())).await;
    }

    async fn until_signal(&self, id: u64, signal: ProcessSignal) {
        wait_until(|| self.signals(id).contains(&signal)).await;
    }
}

fn handle(state: &Arc<Mutex<State>>, directory: &Path, request: &Request, connection: Connection) -> Action {
    let mut state = lock(state);
    state.ops.push(request.op().to_owned());
    match request {
        Request::Ping => Action::Reply(pong()),
        Request::List { mode } => {
            if state.hold_list {
                state.hold_list = false;
                state.held_lists.push(connection);
                return Action::Hold;
            }
            Action::Reply(Response::Processes {
                processes: state.processes.iter().filter(|process| process.mode == *mode).cloned().collect(),
            })
        }
        Request::Get { id } => match state.find(*id) {
            Some(process) => Action::Reply(Response::Process { process: process.clone() }),
            None => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::CreatePipe { program, args, cwd, env, group_id, output_path, metadata } => {
            if let Some(message) = state.fail_create.clone() {
                return Action::Reply(Response::Error { message });
            }
            let mut command = vec![program.clone()];
            command.extend(args.iter().cloned());
            let id = state.next_id;
            state.next_id += 1;
            let process = ProcessInfo {
                id,
                mode: ProcessMode::Pipe,
                pid: Some(1_000 + id as u32),
                command,
                cwd: cwd.clone(),
                group_id: group_id.clone(),
                lifecycle: TerminalLifecycle::Running,
                output_head: 0,
                output_tail: 0,
                started_at: now_ms(),
                exited_at: None,
                pty: None,
                pipe: Some(PipeInfo {
                    output_path: output_path.clone(),
                    output_lines: 0,
                    output_error: None,
                    metadata: metadata.clone(),
                }),
            };
            std::fs::write(output_path, "").expect("create the output file");
            state.creates.push(CreateRecord {
                program: program.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                env: env.clone(),
                group_id: group_id.clone(),
                output_path: output_path.clone(),
                metadata: metadata.clone(),
            });
            state.processes.push(process.clone());
            Action::Reply(Response::Created { process })
        }
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
                state.signals.push((*id, *signal));
                if *signal != ProcessSignal::Term || !state.trap.contains(id) {
                    state.exit(*id, None, Some(State::exit_name(*signal).to_owned()));
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
                state.metadata_updates.push((*id, metadata.clone()));
                Action::Reply(Response::Ok)
            }
            _ => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
        },
        Request::Terminate { id } => {
            let index = state.processes.iter().position(|process| process.id == *id);
            match index {
                Some(index) => {
                    state.processes.remove(index);
                    state.drop_wait(*id);
                    Action::Reply(Response::Ok)
                }
                None => Action::Reply(Response::Error { message: format!("terminal {id} not found") }),
            }
        }
        Request::Shutdown => {
            let _ = std::fs::remove_file(directory.join("service.json"));
            Action::Reply(Response::Ok)
        }
        Request::PrepareHandoff => {
            Action::Reply(Response::Handoff { ticket: "next".to_owned(), expires_at: now_ms() + 60_000 })
        }
        _ => Action::Reply(Response::Error { message: format!("unsupported request: {}", request.op()) }),
    }
}

fn bus(root: &Path) -> Arc<Bus> {
    let (database, _) = Database::open(DatabaseLocation::Memory, root).expect("open the database");
    Bus::new(Arc::new(database), BusOptions::default())
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

async fn wait_until(check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "the condition was not met in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn runs_a_command_on_the_daemon_with_the_server_environment_and_metadata() {
    let harness = harness().await;
    let shell = harness.open().await;
    std::fs::create_dir_all(harness.directory.join("sub")).expect("a subdirectory");
    let mut input = harness.input("echo hi");
    input.cwd = Some("sub".to_owned());
    input.timeout = Some(60_000);
    input.metadata = Some(JsonMap(vec![("sessionID".to_owned(), json!("ses_test"))]));
    let info = shell
        .create(input, |invocation| {
            invocation.env.retain(|(name, _)| name != "OPENCODE_TEST_REMOVED");
        })
        .await
        .expect("a daemon command starts");

    let created = harness.created();
    let request = created.first().expect("the daemon received a create request");
    assert_eq!(request.program, "/bin/sh");
    assert_eq!(request.args, vec!["-c".to_owned(), "echo hi".to_owned()]);
    assert_eq!(request.cwd, harness.directory.join("sub"));
    assert_eq!(request.output_path, PathBuf::from(&info.file));
    assert_eq!(request.group_id, harness.project);
    assert_eq!(request.metadata["id"], json!(info.id.0));
    assert_eq!(request.metadata["command"], json!("echo hi"));
    assert_eq!(request.metadata["shell"], json!("/bin/sh"));
    assert_eq!(request.metadata["location"]["directory"], json!(harness.directory.to_string_lossy()));
    assert_eq!(request.metadata["metadata"]["sessionID"], json!("ses_test"));
    assert!(request.metadata["timeoutDeadline"].is_number());
    assert_eq!(request.env.get("TERM").map(String::as_str), Some("xterm-256color"));
    assert_eq!(request.env.get("OPENCODE_TERMINAL").map(String::as_str), Some("1"));
    assert!(!request.env.contains_key("OPENCODE_TEST_REMOVED"));
    assert_eq!(info.status, ShellInfoStatus::Running);
    assert_eq!(info.pid, Some(1001.0));
    assert_eq!(PathBuf::from(&info.cwd), harness.directory.join("sub"));

    harness.output(1, "first\nsecond");
    let page = shell.output(&info.id.0, None, None).expect("the command is known");
    assert_eq!(page.output, "first\nsecond");
    assert_eq!(page.cursor, 12.0);
    assert_eq!(page.size, 12.0);
    let middle = shell.output(&info.id.0, Some(6), Some(3)).expect("the command is known");
    assert_eq!(middle.output, "sec");
    assert_eq!(middle.cursor, 9.0);
    let caught_up = shell.output(&info.id.0, Some(u64::MAX), None).expect("the command is known");
    assert_eq!(caught_up.output, "");
    assert_eq!(caught_up.cursor, 12.0);
    assert_eq!(shell.list().iter().map(|item| item.id.clone()).collect::<Vec<_>>(), vec![info.id.clone()]);

    harness.exit(1, 0);
    let result = shell.result(info, None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Exited);
    assert_eq!(result.info.exit, Some(0.0));
    assert!(result.info.time.completed.is_some());
    let capture = result.capture.expect("the output file is known");
    assert_eq!(capture.output, "first\nsecond");
    assert!(!capture.truncated);
    assert!(shell.list().is_empty());
}

#[tokio::test]
async fn pages_daemon_output_at_utf8_boundaries_with_byte_cursors() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("pages"), |_| {}).await.expect("a daemon command starts");
    let prefix = "a".repeat(65535);
    for character in ["é", "€", "😀"] {
        let text = format!("{prefix}{character}\u{FEFF}z");
        std::fs::write(&info.file, &text).expect("the daemon output file");
        let page = shell.output(&info.id.0, None, None).expect("the command is known");
        assert_eq!(page.cursor, prefix.len() as f64);
        assert_eq!(page.size, text.len() as f64);
        let rest = shell.output(&info.id.0, Some(page.cursor as u64), None).expect("the command is known");
        assert_eq!(format!("{}{}", page.output, rest.output), text);
        assert_eq!(rest.cursor, page.size);
        let tiny = shell.output(&info.id.0, Some(page.cursor as u64), Some(1)).expect("the command is known");
        assert_eq!(tiny.output, character);
        assert_eq!(tiny.cursor, page.cursor + character.len() as f64);
        let tail = shell.output(&info.id.0, Some(page.cursor as u64 + 1), None).expect("the command is known");
        assert_eq!(tail.output, "\u{FEFF}z");
        assert_eq!(tail.cursor, page.size);
    }
}

#[tokio::test]
async fn reports_a_command_lost_with_the_daemon_as_killed_with_its_output() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("work"), |_| {}).await.expect("a daemon command starts");
    harness.output(1, "partial\n");
    harness.until_waiting(1).await;
    lock(&harness.state).drop_wait(1);
    let result = shell.result(info, None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Killed);
    let capture = result.capture.expect("the output file is known");
    assert_eq!(capture.output, "partial\n");
    assert_eq!(capture.killed.as_deref(), Some("Command was killed: the process daemon stopped"));
}

#[tokio::test]
async fn stops_a_timed_out_command_with_sigterm_then_sigkill_and_keeps_it() {
    let harness = harness().await;
    let shell = harness.open().await;
    lock(&harness.state).trap.insert(1);
    let mut input = harness.input("trap '' TERM; sleep 60");
    input.timeout = Some(10);
    let info = shell.create(input, |_| {}).await.expect("a daemon command starts");
    harness.until_signal(1, ProcessSignal::Term).await;
    assert_eq!(shell.get(&info.id.0).expect("the command is known").status, ShellInfoStatus::Running);
    let metadata = harness.process_metadata(1).expect("the command is still known");
    assert_eq!(metadata["stopReason"], json!("timeout"));
    assert!(!metadata.contains_key("timeoutDeadline"));
    harness.until_signal(1, ProcessSignal::Kill).await;
    let done = shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Timeout);
    assert_eq!(done.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(harness.signals(1), vec![ProcessSignal::Term, ProcessSignal::Kill]);
    assert!(!harness.ops().contains(&"terminate".to_owned()));
    assert!(Path::new(&info.file).exists());
}

#[tokio::test]
async fn removes_a_daemon_command_from_the_daemon_before_deleting_its_file() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("sleep 60"), |_| {}).await.expect("a daemon command starts");
    shell.remove(&info.id.0).await.expect("the command is known");
    assert!(harness.ops().contains(&"terminate".to_owned()));
    assert!(harness.processes().is_empty());
    assert!(!Path::new(&info.file).exists());
    assert!(matches!(shell.get(&info.id.0), Err(ShellError::NotFound { .. })));
    let result = shell.result(info, None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Killed);
    assert!(result.capture.is_none());
}

#[tokio::test]
async fn recovers_its_stored_pipes_and_resumes_their_timeouts() {
    let harness = harness().await;
    let first = harness.open_in_with(&harness.directory, harness.new_server()).await;
    let running = first
        .create(
            CreateInput { metadata: Some(JsonMap(vec![("tag".to_owned(), json!("a"))])), ..harness.input("running") },
            |_| {},
        )
        .await
        .expect("the running command starts");
    let finished = first.create(harness.input("finished"), |_| {}).await.expect("the finished command starts");
    harness.output(2, "done\n");
    harness.exit(2, 3);
    let exited = first.wait(&finished.id.0).await.expect("the command ends");
    let mut expiring_input = harness.input("expiring");
    expiring_input.timeout = Some(60_000);
    let expiring = first.create(expiring_input, |_| {}).await.expect("the expiring command starts");
    let mut waited_input = harness.input("waited");
    waited_input.timeout = Some(60_000);
    waited_input.waited = true;
    let waited = first.create(waited_input, |_| {}).await.expect("the waited command starts");

    drop(first);

    lock(&harness.state)
        .find_mut(3)
        .and_then(|process| process.pipe.as_mut())
        .expect("the expiring command is still in the daemon")
        .metadata
        .insert("timeoutDeadline".to_owned(), json!(now_ms() - 1));

    let other_directory = harness._dir.path().join("other");
    let other = harness.open_in_with(&other_directory, harness.new_server()).await;
    assert!(other.list().is_empty());
    assert!(matches!(other.get(&running.id.0), Err(ShellError::NotFound { .. })));

    let second = harness.open_in_with(&harness.directory, harness.new_server()).await;
    let recovered = second.get(&running.id.0).expect("the running command was taken back");
    assert_eq!(recovered.status, ShellInfoStatus::Running);
    assert_eq!(recovered.command, "running");
    assert_eq!(recovered.shell, "/bin/sh");
    assert_eq!(recovered.file, running.file);
    assert_eq!(recovered.metadata.0, vec![("tag".to_owned(), json!("a"))]);
    assert_eq!(recovered.pid, running.pid);
    assert_eq!(second.get(&exited.id.0).expect("the finished command was taken back"), exited);
    let result = second.result(exited.clone(), None).await;
    assert_eq!(result.capture.expect("the output file is known").output, "done\n");

    assert_eq!(second.wait(&expiring.id.0).await.expect("the expiring command ends").status, ShellInfoStatus::Timeout);
    assert_eq!(harness.signals(3), vec![ProcessSignal::Term]);
    assert_eq!(second.get(&waited.id.0).expect("the waited command was taken back").status, ShellInfoStatus::Running);
    assert!(harness.signals(4).is_empty());
    let waited_metadata = harness.process_metadata(4).expect("the waited command is known");
    assert!(!waited_metadata.contains_key("timeoutDeadline"));

    harness.exit(1, 0);
    let ended = second.wait(&running.id.0).await.expect("the running command ends");
    assert_eq!(ended.status, ShellInfoStatus::Exited);
    assert_eq!(ended.exit, Some(0.0));
}

#[tokio::test]
async fn fails_a_command_the_daemon_rejected_instead_of_spawning_it() {
    let harness = harness().await;
    lock(&harness.state).fail_create = Some("output path is a symlink".to_owned());
    let shell = harness.open().await;
    let error = shell.create(harness.input("echo hi"), |_| {}).await.expect_err("the daemon rejected the command");
    assert!(matches!(error, ShellError::Spawn { .. }), "{error}");
    assert_eq!(harness.created().len(), 0);
    assert!(shell.list().is_empty());
}

#[tokio::test]
async fn falls_back_to_spawning_when_the_daemon_is_unavailable() {
    let harness = harness().await;
    std::fs::remove_file(harness._dir.path().join("state").join("daemon").join("service.json"))
        .expect("forget the daemon registration");
    let shell = harness.open().await;
    let info = shell.create(harness.input("printf fallback"), |_| {}).await.expect("the command spawns");
    assert!(harness.created().is_empty());
    let done = shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Exited);
    let result = shell.result(done, None).await;
    assert_eq!(result.capture.expect("the output file is known").output, "fallback");
}

#[tokio::test]
async fn reports_the_output_a_write_error_stopped_early() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("failed"), |_| {}).await.expect("a daemon command starts");
    harness.output(1, "kept\n");
    harness.fail(1, "No space left on device");
    harness.output(1, "lost\n");
    harness.exit(1, 1);
    let result = shell.result(info.clone(), None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Exited);
    assert_eq!(result.info.exit, Some(1.0));
    let capture = result.capture.expect("the output file is known");
    assert_eq!(capture.output, format!("kept\n\n[output file {} stopped early: No space left on device]", info.file));
}

#[tokio::test]
async fn sizes_a_cut_off_line_from_the_daemons_output_file() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("wide"), |_| {}).await.expect("a daemon command starts");
    harness.output(1, &format!("short\n{}\n", "x".repeat(60 * 1024)));
    harness.exit(1, 0);
    let result = shell.result(info.clone(), None).await;
    let capture = result.capture.expect("the output file is known");
    assert!(
        capture
            .output
            .ends_with(&format!("\n\n[showing the last 50 KB of line 2 (60 KB); full output saved to {}]", info.file)),
        "{}",
        capture.output
    );
}

fn lines(count: usize) -> String {
    (1..=count).map(|index| format!("line {index}\n")).collect()
}

fn expired() -> SystemTime {
    SystemTime::now().checked_sub(Duration::from_secs(8 * 24 * 60 * 60)).expect("eight days ago")
}

fn set_modified(path: &Path, time: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .expect("the output file opens")
        .set_times(FileTimes::new().set_modified(time))
        .expect("the output file timestamp is set");
}

#[tokio::test]
async fn retries_an_incomplete_live_character_and_consumes_malformed_bytes_at_exit() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("partial"), |_| {}).await.expect("a daemon command starts");
    std::fs::write(&info.file, [0xff, 0xf0, 0x9f]).expect("the daemon writes bytes");
    let page = shell.output(&info.id.0, None, None).expect("the command is known");
    assert_eq!(page.output, "�");
    assert_eq!(page.cursor, 1.0);
    assert_eq!(page.size, 3.0);
    let tiny = shell.output(&info.id.0, Some(1), Some(1)).expect("the command is known");
    assert_eq!(tiny.output, "");
    assert_eq!(tiny.cursor, 1.0);

    let mut file = std::fs::OpenOptions::new().append(true).open(&info.file).expect("the output file");
    file.write_all(&[0x98, 0x80, 0xe0, 0x80, 0xe2]).expect("the daemon writes bytes");
    let rest = shell.output(&info.id.0, Some(page.cursor as u64), None).expect("the command is known");
    assert_eq!(rest.output, "😀��");
    assert_eq!(rest.cursor, 7.0);
    assert_eq!(rest.size, 8.0);

    harness.exit(1, 0);
    shell.wait(&info.id.0).await.expect("the command ends");
    let final_page = shell.output(&info.id.0, Some(rest.cursor as u64), None).expect("the command is known");
    assert_eq!(final_page.output, "�");
    assert_eq!(final_page.cursor, 8.0);
    assert_eq!(final_page.size, 8.0);
}

#[tokio::test]
async fn counts_daemon_lines_for_the_result_tail() {
    let harness = harness().await;
    let shell = harness.open().await;
    let info = shell.create(harness.input("long"), |_| {}).await.expect("a daemon command starts");
    harness.output(1, &lines(3000));
    harness.exit(1, 0);
    let result = shell.result(info.clone(), None).await;
    let capture = result.capture.expect("the output file is known");
    assert!(
        capture
            .output
            .ends_with(&format!("line 3000\n\n[showing lines 1001-3000 of 3000; full output saved to {}]", info.file)),
        "{}",
        capture.output
    );
}

#[tokio::test]
async fn bounds_running_output_to_what_the_daemon_has_counted() {
    let harness = harness().await;
    let shell = harness.open().await;
    let big = shell.create(harness.input("big"), |_| {}).await.expect("a daemon command starts");
    harness.output(1, &format!("{}\n{}", "y".repeat(17 * 1024 * 1024), lines(10)));
    let capture = shell.result(big.clone(), Some(Duration::ZERO)).await.capture.expect("the output file is known");
    assert!(
        capture.output.ends_with(&format!("line 10\n\n[showing the last 10 lines; full output saved to {}]", big.file)),
        "{}",
        capture.output
    );

    let wide = shell.create(harness.input("wide"), |_| {}).await.expect("a daemon command starts");
    harness.output(2, &format!("short\n{}", "x".repeat(2 * 1024 * 1024)));
    let capture = shell.result(wide.clone(), Some(Duration::ZERO)).await.capture.expect("the output file is known");
    assert!(
        capture.output.ends_with(&format!(
            "[showing the last 50 KB of line 2 (over 1024 KB); full output saved to {}]",
            wide.file
        )),
        "{}",
        capture.output
    );

    let ended = shell.create(harness.input("ended"), |_| {}).await.expect("a daemon command starts");
    harness.output(3, &format!("short\n{}\n", "x".repeat(2 * 1024 * 1024)));
    let capture = shell.result(ended.clone(), Some(Duration::ZERO)).await.capture.expect("the output file is known");
    assert!(
        capture.output.ends_with(&format!(
            "[showing the last 50 KB of line 2 (over 1024 KB); full output saved to {}]",
            ended.file
        )),
        "{}",
        capture.output
    );
}

#[tokio::test]
async fn records_exits_without_signalling_a_finished_command() {
    let harness = harness().await;
    let shell = harness.open().await;
    let succeeded = shell.create(harness.input("serve & exit 0"), |_| {}).await.expect("the command starts");
    let failed = shell.create(harness.input("serve & exit 3"), |_| {}).await.expect("the command starts");
    harness.until_waiting(1).await;
    harness.until_waiting(2).await;
    harness.exit(1, 0);
    harness.exit(2, 3);
    let failed = shell.wait(&failed.id.0).await.expect("the command ends");
    assert_eq!(failed.status, ShellInfoStatus::Exited);
    assert_eq!(failed.exit, Some(3.0));
    let succeeded = shell.wait(&succeeded.id.0).await.expect("the command ends");
    assert_eq!(succeeded.status, ShellInfoStatus::Exited);
    assert_eq!(succeeded.exit, Some(0.0));
    assert!(harness.signals(1).is_empty());
    assert!(harness.signals(2).is_empty());
}

#[tokio::test]
async fn recreates_the_output_directory_removed_while_the_location_is_open() {
    let harness = harness().await;
    let shell = harness.open().await;
    shell.create(harness.input("echo first"), |_| {}).await.expect("the first command starts");
    std::fs::remove_dir_all(harness.data.join("shell")).expect("remove the output directory");
    let info = shell.create(harness.input("echo second"), |_| {}).await.expect("the second command starts");
    assert_eq!(harness.created().len(), 2, "the daemon received the second command");
    harness.output(2, "second");
    harness.exit(2, 0);
    let result = shell.result(info.clone(), None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Exited);
    assert_eq!(result.info.exit, Some(0.0));
    assert_eq!(result.capture.expect("the output file is known").output, "second");
}

#[tokio::test]
async fn forgets_an_evicted_daemon_command_in_the_daemon() {
    let harness = harness().await;
    let shell = harness.open().await;
    let mut commands = Vec::new();
    for index in 0..=EXITED_LIMIT {
        let info = shell.create(harness.input(&format!("echo {index}")), |_| {}).await.expect("the command starts");
        harness.exit(index as u64 + 1, 0);
        shell.wait(&info.id.0).await.expect("the command ends");
        commands.push(info);
    }
    wait_until(|| !Path::new(&commands[0].file).exists()).await;
    assert!(matches!(shell.get(&commands[0].id.0), Err(ShellError::NotFound { .. })));
    assert!(!harness.processes().contains(&1));
    assert!(harness.ops().contains(&"terminate".to_owned()));
    assert!(Path::new(&commands[1].file).exists());
}

#[tokio::test]
async fn the_sweep_keeps_a_running_command_file_and_forgets_finished_pipes() {
    let harness = harness().await;
    let shell = harness.open().await;
    let running = shell.create(harness.input("running"), |_| {}).await.expect("the running command starts");
    let finished = shell.create(harness.input("finished"), |_| {}).await.expect("the finished command starts");
    harness.exit(2, 0);
    shell.wait(&finished.id.0).await.expect("the command ends");
    let lost_file = shell.create(harness.input("lost file"), |_| {}).await.expect("the command starts");
    harness.exit(3, 0);
    shell.wait(&lost_file.id.0).await.expect("the command ends");
    let old = shell.create(harness.input("old"), |_| {}).await.expect("the command starts");
    harness.exit(4, 0);
    shell.wait(&old.id.0).await.expect("the command ends");

    std::fs::remove_file(&lost_file.file).expect("remove the lost command file");
    lock(&harness.state).find_mut(4).expect("the old command").exited_at =
        Some(expired().duration_since(UNIX_EPOCH).expect("the epoch").as_millis() as u64);
    set_modified(Path::new(&running.file), expired());
    set_modified(Path::new(&finished.file), expired());
    let orphan = harness.data.join("shell").join(&harness.project).join("sh_0123456789abORPHANORPHANOR.out");
    std::fs::write(&orphan, "orphan").expect("an orphan output file");
    set_modified(&orphan, expired());

    harness.commands.cleanup(&harness.data).await.expect("the sweep runs");

    assert!(Path::new(&running.file).exists());
    assert!(!Path::new(&finished.file).exists());
    assert!(!orphan.exists());
    assert_eq!(harness.processes(), vec![1]);
    assert!(Path::new(&old.file).exists());
}

#[tokio::test]
async fn a_closed_location_keeps_enforcing_its_commands_timeouts() {
    let harness = harness().await;
    let shell = harness.open().await;
    let mut input = harness.input("sleep 60");
    input.timeout = Some(50);
    let info = shell.create(input, |_| {}).await.expect("the command starts");
    drop(shell);
    harness.until_signal(1, ProcessSignal::Term).await;
    let replacement = harness.open().await;
    let done = replacement.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Timeout);
}

#[tokio::test]
async fn two_shells_for_one_location_share_commands_and_report_each_exit_once() {
    let harness = harness().await;
    let closing = harness.open().await;
    let mut backgrounded_input = harness.input("sleep 60");
    backgrounded_input.timeout = Some(200);
    let backgrounded = closing.create(backgrounded_input, |_| {}).await.expect("the command starts");
    let stopped = closing.create(harness.input("sleep 60"), |_| {}).await.expect("the command starts");
    let removed = closing.create(harness.input("sleep 60"), |_| {}).await.expect("the command starts");
    let replacement = harness.open().await;

    let exits = Arc::new(AtomicUsize::new(0));
    let _listener = harness.bus.listen(Box::new({
        let exits = exits.clone();
        move |event: &Event| -> Result<(), BusError> {
            if event.r#type() == "shell.exited" {
                exits.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }));

    closing.timeout(&backgrounded.id.0, 0).await.expect("the command is known");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(harness.signals(1).is_empty());

    closing.stop(&stopped.id.0, StopReason::Killed).await.expect("the command is known");
    let from_replacement = replacement.wait(&stopped.id.0).await.expect("the command ends");
    assert_eq!(from_replacement.status, ShellInfoStatus::Killed);
    let from_closing = closing.wait(&stopped.id.0).await.expect("the command ends");
    assert_eq!(from_closing.status, ShellInfoStatus::Killed);
    wait_until(|| exits.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(exits.load(Ordering::SeqCst), 1);

    replacement.remove(&removed.id.0).await.expect("the command is known");
    wait_until(|| exits.load(Ordering::SeqCst) == 2).await;
    assert!(matches!(closing.get(&removed.id.0), Err(ShellError::NotFound { .. })));
    let waits = harness.ops().iter().filter(|op| op.as_str() == "wait").count();
    assert_eq!(waits, 3);
}

#[tokio::test]
async fn finishes_recovery_after_the_first_caller_is_cancelled() {
    let harness = harness().await;
    let first = harness.open_in_with(&harness.directory, harness.new_server()).await;
    let info = first.create(harness.input("running"), |_| {}).await.expect("the command starts");
    drop(first);

    let server = harness.new_server();
    harness.hold_next_list();
    let before = harness.list_ops();
    let task = tokio::spawn(Shell::new(harness.bus.clone(), harness.options(&harness.directory, server.clone())));
    wait_until(|| harness.held_lists() == 1).await;
    task.abort();
    let _ = task.await;
    harness.release_lists();

    let second = Shell::new(harness.bus.clone(), harness.options(&harness.directory, server)).await;
    assert!(second.list().iter().any(|command| command.id == info.id));
    assert_eq!(harness.list_ops(), before + 1);
}
