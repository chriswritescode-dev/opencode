use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    PersistentPtyInfo, PersistentPtyInfoOutput, PersistentPtyInfoSize, PersistentPtyInfoStatus,
    PersistentPtyReadResult, PersistentPtyReadResultScreen, PersistentPtyReadResultScreenCursor, PersistentPtySnapshot,
    PersistentPtySnapshotCursor, PtyID, SessionID,
};
use opencode_util::lock;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::daemon::protocol::{
    AttachmentRole, PipeMetadata, ProcessInfo, ProcessMode, ProcessSignal, TerminalLifecycle,
};
use crate::daemon::{
    Daemon, DaemonAttachment, DaemonError, DaemonOptions, ErrorKind, Handoff, Replay, Request, Response, StreamEvent,
    SubscribeInput,
};

pub const fn available() -> bool {
    cfg!(unix)
}

pub struct Options {
    pub root: PathBuf,
    pub id: String,
    pub env: Vec<(String, String)>,
    pub inherited: Option<Handoff>,
    pub default_command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PersistentPtyError {
    #[error("PersistentPty.UnavailableError: {message}")]
    Unavailable { message: String },
    #[error("PersistentPty.RequestError: {message}")]
    Request { message: String },
    #[error("PersistentPty.NotFoundError: {pty_id}")]
    NotFound { pty_id: String },
    #[error("PersistentPty.ProcessNotFoundError: {process_id}")]
    ProcessNotFound { process_id: u64 },
    #[error("PersistentPty.InvalidCwdError: {message}")]
    InvalidCwd { cwd: String, message: String },
}

#[derive(Clone, Debug)]
pub struct CreateInput {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub title: String,
    pub env: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub struct PipeInfo {
    pub id: u64,
    pub pid: Option<u32>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub group_id: String,
    pub lifecycle: PipeLifecycle,
    pub output_tail: u64,
    pub output_lines: u64,
    pub output_path: PathBuf,
    pub output_error: Option<String>,
    pub metadata: PipeMetadata,
    pub started_at: u64,
    pub exited_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum PipeLifecycle {
    Running,
    Exited { exit_code: Option<u32>, signal: Option<String> },
    Failed { message: String },
}

#[derive(Clone, Debug)]
pub struct PipeExit {
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
    pub final_offset: u64,
    pub output_lines: u64,
    pub output_error: Option<String>,
    pub exited_at: u64,
}

#[derive(Clone, Debug)]
pub struct CreatePipeInput {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub group_id: String,
    pub output_path: PathBuf,
    pub metadata: PipeMetadata,
}

pub struct AttachInput {
    pub cursor: u64,
    pub attachment_id: String,
    pub role: AttachmentRole,
    pub takeover: bool,
    pub on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    pub on_end: Arc<dyn Fn() + Send + Sync>,
}

pub struct Attachment {
    pub info: PersistentPtyInfo,
    pub role: AttachmentRole,
    pub generation: u64,
    pub replay: Replay,
    daemon: DaemonAttachment,
}

impl Attachment {
    pub fn activate(&self) {
        self.daemon.activate();
    }

    pub fn detach(&self) {
        self.daemon.detach();
    }
}

struct Inner {
    bus: Arc<Bus>,
    daemon: OnceCell<Daemon>,
    daemon_options: DaemonOptions,
    default_command: String,
    removing: Mutex<HashSet<String>>,
    current: RwLock<BTreeMap<String, String>>,
}

#[derive(Clone)]
pub struct PersistentPty {
    inner: Arc<Inner>,
}

impl PersistentPty {
    pub fn new(options: Options, bus: Arc<Bus>) -> Arc<Self> {
        let daemon_options =
            DaemonOptions { root: options.root, id: options.id, inherited: options.inherited, env: options.env };
        let service = Arc::new(PersistentPty {
            inner: Arc::new(Inner {
                bus: bus.clone(),
                daemon: OnceCell::new(),
                daemon_options,
                default_command: options.default_command,
                removing: Mutex::new(HashSet::new()),
                current: RwLock::new(BTreeMap::new()),
            }),
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let listener = service.clone();
            let mut subscription = bus.subscribe(&["session.deleted"], None);
            handle.spawn(async move {
                while let Some(event) = subscription.recv().await {
                    let Some(session_id) = event.data().get("sessionID").and_then(Value::as_str) else {
                        continue;
                    };
                    listener.remove_session(session_id).await;
                }
            });
        }
        service
    }

    pub async fn list(&self, session_id: Option<&str>) -> Result<Vec<PersistentPtyInfo>, PersistentPtyError> {
        let Some(response) = self
            .daemon()
            .await
            .request_if_running(&Request::List { mode: ProcessMode::Pty })
            .await
            .map_err(unavailable)?
        else {
            return Ok(Vec::new());
        };
        let Response::Processes { processes } = response else {
            return Err(unexpected(&response));
        };
        processes
            .into_iter()
            .filter(|process| session_id.is_none_or(|session_id| process.group_id == session_id))
            .map(to_info)
            .collect()
    }

    pub async fn get(&self, id: &str) -> Result<PersistentPtyInfo, PersistentPtyError> {
        let Some(parsed) = parse_id(id) else {
            return Err(PersistentPtyError::NotFound { pty_id: id.to_owned() });
        };
        let Some(found) = self.lookup(parsed).await? else {
            return Err(PersistentPtyError::NotFound { pty_id: id.to_owned() });
        };
        if found.mode != ProcessMode::Pty {
            return Err(PersistentPtyError::NotFound { pty_id: id.to_owned() });
        }
        to_info(found)
    }

    pub async fn create(&self, session_id: &str, input: CreateInput) -> Result<PersistentPtyInfo, PersistentPtyError> {
        let cwd = resolve_cwd(input.cwd.as_deref());
        if !tokio::fs::metadata(&cwd).await.is_ok_and(|metadata| metadata.is_dir()) {
            let message = format!("cwd is not a directory: {cwd}");
            return Err(PersistentPtyError::InvalidCwd { cwd, message });
        }
        let response = self
            .daemon()
            .await
            .request(
                &Request::CreatePty {
                    program: input.command.unwrap_or_else(|| self.inner.default_command.clone()),
                    args: input.args,
                    cwd: PathBuf::from(cwd),
                    env: input.env,
                    group_id: session_id.to_owned(),
                    title: input.title,
                    cols: input.cols,
                    rows: input.rows,
                },
                true,
            )
            .await
            .map_err(unavailable)?;
        let Response::Created { process } = response else {
            return Err(unexpected(&response));
        };
        let terminal = to_info(process)?;
        self.publish(
            "persistent-pty.added",
            json!({ "sessionID": terminal.session_id.0, "terminal": info_value(&terminal) }),
        );
        Ok(terminal)
    }

    pub async fn write(&self, id: &str, data: &str, attachment_id: Option<&str>) -> Result<(), PersistentPtyError> {
        self.send(
            id,
            Request::Write {
                id: from_id(id)?,
                attachment_id: attachment_id.map(str::to_owned),
                data_base64: STANDARD.encode(data.as_bytes()),
            },
            false,
        )
        .await
    }

    pub async fn resize(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        attachment_id: Option<&str>,
    ) -> Result<(), PersistentPtyError> {
        self.send(
            id,
            Request::Resize { id: from_id(id)?, attachment_id: attachment_id.map(str::to_owned), cols, rows },
            true,
        )
        .await
    }

    pub async fn control(&self, id: &str, attachment_id: &str, cols: u16, rows: u16) -> Result<(), PersistentPtyError> {
        self.send(id, Request::Control { id: from_id(id)?, attachment_id: attachment_id.to_owned(), cols, rows }, true)
            .await
    }

    pub async fn input(
        &self,
        id: &str,
        attachment_id: &str,
        cols: u16,
        rows: u16,
        data: &[u8],
    ) -> Result<(), PersistentPtyError> {
        self.send(
            id,
            Request::Input {
                id: from_id(id)?,
                attachment_id: attachment_id.to_owned(),
                cols,
                rows,
                data_base64: STANDARD.encode(data),
            },
            true,
        )
        .await
    }

    pub async fn rename(&self, id: &str, title: Option<&str>) -> Result<PersistentPtyInfo, PersistentPtyError> {
        let terminal = self.get(id).await?;
        let response = self
            .daemon()
            .await
            .request(&Request::Rename { id: from_id(id)?, title: title.map(str::to_owned) }, false)
            .await
            .map_err(unavailable)?;
        if !matches!(response, Response::Ok) {
            return Err(unexpected(&response));
        }
        let renamed = self.get(id).await?;
        self.publish(
            "persistent-pty.updated",
            json!({ "sessionID": terminal.session_id.0, "terminal": info_value(&renamed) }),
        );
        Ok(renamed)
    }

    pub async fn snapshot(&self, id: &str) -> Result<PersistentPtySnapshot, PersistentPtyError> {
        self.get(id).await?;
        let response =
            self.daemon().await.request(&Request::Snapshot { id: from_id(id)? }, false).await.map_err(unavailable)?;
        let Response::Snapshot { process, text, checkpoint_base64, cursor_x, cursor_y } = response else {
            return Err(unexpected(&response));
        };
        Ok(PersistentPtySnapshot {
            info: to_info(process)?,
            text,
            checkpoint: checkpoint_base64,
            cursor: PersistentPtySnapshotCursor { x: f64::from(cursor_x), y: f64::from(cursor_y) },
        })
    }

    pub async fn read(
        &self,
        session_id: &str,
        lines: Option<u16>,
    ) -> Result<Option<PersistentPtyReadResult>, PersistentPtyError> {
        if lines.is_some_and(|lines| lines == 0) {
            return Err(PersistentPtyError::Unavailable {
                message: "lines must be an integer between 1 and 65535".to_owned(),
            });
        }
        let Some(id) = self.current(session_id) else {
            return Ok(None);
        };
        let terminal = match self.get(&id).await {
            Ok(terminal) => Some(terminal),
            Err(PersistentPtyError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        if terminal.as_ref().is_none_or(|terminal| terminal.session_id.0 != session_id) {
            let mut current = self.inner.current.write().expect("the current-terminal lock is never poisoned");
            if current.get(session_id) == Some(&id) {
                current.remove(session_id);
            }
            return Ok(None);
        }
        let response = self
            .daemon()
            .await
            .request(&Request::ReadRows { id: from_id(&id)?, rows: lines }, false)
            .await
            .map_err(unavailable)?;
        let Response::Rows { process, lines, cursor_x, cursor_y } = response else {
            return Err(unexpected(&response));
        };
        let info = to_info(process)?;
        Ok(Some(PersistentPtyReadResult {
            pty_id: info.id.clone(),
            title: info.title.clone(),
            cwd: info.cwd.clone(),
            foreground_process: info.foreground_process.clone(),
            screen: PersistentPtyReadResultScreen {
                text: lines.join("\n"),
                cols: info.size.cols,
                rows: info.size.rows,
                cursor: PersistentPtyReadResultScreenCursor { x: f64::from(cursor_x), y: f64::from(cursor_y) },
            },
        }))
    }

    pub async fn remove(&self, id: &str) -> Result<(), PersistentPtyError> {
        let terminal = self.get(id).await?;
        let response =
            self.daemon().await.request(&Request::Terminate { id: from_id(id)? }, false).await.map_err(unavailable)?;
        if !matches!(response, Response::Ok) {
            return Err(unexpected(&response));
        }
        {
            let mut current = self.inner.current.write().expect("the current-terminal lock is never poisoned");
            if current.get(&terminal.session_id.0).is_some_and(|selected| selected == id) {
                current.remove(&terminal.session_id.0);
            }
        }
        self.publish("persistent-pty.removed", json!({ "sessionID": terminal.session_id.0, "ptyID": id }));
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), PersistentPtyError> {
        let response = self.daemon().await.shutdown().await.map_err(unavailable)?;
        self.inner.current.write().expect("the current-terminal lock is never poisoned").clear();
        match response {
            None | Some(Response::Ok) => Ok(()),
            Some(response) => Err(unexpected(&response)),
        }
    }

    pub async fn handoff(&self) -> Result<Option<Handoff>, PersistentPtyError> {
        self.daemon().await.handoff().await.map_err(unavailable)
    }

    pub async fn handoff_prepared(&self) -> bool {
        self.daemon().await.handoff_prepared().await
    }

    pub async fn attach(&self, id: &str, input: AttachInput) -> Result<Attachment, PersistentPtyError> {
        self.get(id).await?;
        let on_event = input.on_event;
        let service = self.clone();
        let terminal_id = id.to_owned();
        let wrapped: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |event| {
            if matches!(event, StreamEvent::Exited { .. }) {
                service.remove_visible_exit(&terminal_id);
            }
            on_event(event);
        });
        let attachment = self
            .daemon()
            .await
            .subscribe(
                from_id(id)?,
                SubscribeInput {
                    cursor: input.cursor,
                    attachment_id: input.attachment_id,
                    role: input.role,
                    takeover: input.takeover,
                    on_event: wrapped,
                    on_end: input.on_end,
                },
            )
            .await
            .map_err(unavailable)?;
        let info = to_info(attachment.terminal.clone())?;
        if attachment.role == AttachmentRole::Controller {
            self.inner
                .current
                .write()
                .expect("the current-terminal lock is never poisoned")
                .insert(info.session_id.0.clone(), id.to_owned());
        }
        Ok(Attachment {
            info,
            role: attachment.role,
            generation: attachment.generation,
            replay: attachment.replay.clone(),
            daemon: attachment,
        })
    }

    pub async fn create_pipe(&self, input: CreatePipeInput) -> Result<PipeInfo, PersistentPtyError> {
        let response = self
            .daemon()
            .await
            .request(
                &Request::CreatePipe {
                    program: input.program,
                    args: input.args,
                    cwd: input.cwd,
                    env: input.env,
                    group_id: input.group_id,
                    output_path: input.output_path,
                    metadata: input.metadata,
                },
                true,
            )
            .await
            .map_err(pipe_error)?;
        let Response::Created { process } = response else {
            return Err(pipe_unexpected(&response));
        };
        to_pipe_info(process)
    }

    pub async fn wait_pipe(&self, id: u64) -> Result<PipeExit, PersistentPtyError> {
        let response = self.daemon().await.wait(id).await.map_err(pipe_error)?;
        let Response::Exited { exit_code, signal, final_offset, output_lines, output_error, exited_at } = response
        else {
            return Err(pipe_unexpected(&response));
        };
        Ok(PipeExit {
            exit_code,
            signal,
            final_offset,
            output_lines: output_lines.unwrap_or(0),
            output_error,
            exited_at,
        })
    }

    pub async fn get_pipe(&self, id: u64) -> Result<PipeInfo, PersistentPtyError> {
        let Some(found) = self.lookup(id).await? else {
            return Err(PersistentPtyError::ProcessNotFound { process_id: id });
        };
        if found.mode != ProcessMode::Pipe {
            return Err(PersistentPtyError::ProcessNotFound { process_id: id });
        }
        to_pipe_info(found)
    }

    pub async fn signal_pipe(&self, id: u64, signal: ProcessSignal) -> Result<(), PersistentPtyError> {
        self.pipe_command(Request::Signal { id, signal }).await
    }

    pub async fn set_pipe_metadata(&self, id: u64, metadata: PipeMetadata) -> Result<(), PersistentPtyError> {
        self.pipe_command(Request::SetMetadata { id, metadata }).await
    }

    pub async fn terminate_pipe(&self, id: u64) -> Result<(), PersistentPtyError> {
        self.pipe_command(Request::Terminate { id }).await
    }

    pub async fn list_pipes(&self) -> Result<Vec<PipeInfo>, PersistentPtyError> {
        let Some(response) = self
            .daemon()
            .await
            .request_if_running(&Request::List { mode: ProcessMode::Pipe })
            .await
            .map_err(pipe_error)?
        else {
            return Ok(Vec::new());
        };
        let Response::Processes { processes } = response else {
            return Err(pipe_unexpected(&response));
        };
        processes.into_iter().map(to_pipe_info).collect()
    }

    async fn send(&self, id: &str, request: Request, select: bool) -> Result<(), PersistentPtyError> {
        let terminal = self.get(id).await?;
        let response = self.daemon().await.request(&request, false).await.map_err(unavailable)?;
        if !matches!(response, Response::Ok) {
            return Err(unexpected(&response));
        }
        if select {
            self.inner
                .current
                .write()
                .expect("the current-terminal lock is never poisoned")
                .insert(terminal.session_id.0, id.to_owned());
        }
        Ok(())
    }

    async fn pipe_command(&self, request: Request) -> Result<(), PersistentPtyError> {
        let response = self.daemon().await.request(&request, false).await.map_err(pipe_error)?;
        if !matches!(response, Response::Ok) {
            return Err(pipe_unexpected(&response));
        }
        Ok(())
    }

    async fn daemon(&self) -> &Daemon {
        let options = self.inner.daemon_options.clone();
        self.inner.daemon.get_or_init(|| Daemon::open(options)).await
    }

    async fn lookup(&self, id: u64) -> Result<Option<ProcessInfo>, PersistentPtyError> {
        match self.daemon().await.request_if_running(&Request::Get { id }).await {
            Ok(Some(Response::Process { process })) => Ok(Some(process)),
            Ok(Some(response)) => Err(unexpected(&response)),
            Ok(None) => Ok(None),
            Err(error) if error.kind == ErrorKind::Request => Ok(None),
            Err(error) => Err(unavailable(error)),
        }
    }

    fn current(&self, session_id: &str) -> Option<String> {
        self.inner.current.read().expect("the current-terminal lock is never poisoned").get(session_id).cloned()
    }

    fn remove_visible_exit(&self, id: &str) {
        if !lock(&self.inner.removing).insert(id.to_owned()) {
            return;
        }
        let service = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            if let Err(error) = service.remove(&id).await {
                match error {
                    PersistentPtyError::NotFound { .. } => {}
                    other => log::warn!("failed to remove visible exited terminal {id}: {other}"),
                }
            }
            lock(&service.inner.removing).remove(&id);
        });
    }

    async fn remove_session(&self, session_id: &str) {
        match self.list(Some(session_id)).await {
            Ok(terminals) => {
                for terminal in terminals {
                    if let Err(error) = self.remove(&terminal.id.0).await {
                        match error {
                            PersistentPtyError::NotFound { .. } => {}
                            other => log::warn!(
                                "failed to remove deleted session's terminal {} for {session_id}: {other}",
                                terminal.id.0
                            ),
                        }
                    }
                }
            }
            Err(error) => log::warn!("failed to list deleted session's terminals for {session_id}: {error}"),
        }
        self.inner.current.write().expect("the current-terminal lock is never poisoned").remove(session_id);
    }

    fn publish(&self, r#type: &'static str, data: Value) {
        if let Err(error) = self.inner.bus.publish(&Definition::ephemeral(r#type), data, PublishOptions::default()) {
            log::warn!("failed to publish {type}: {error}");
        }
    }
}

fn resolve_cwd(cwd: Option<&str>) -> String {
    let cwd = Path::new(cwd.unwrap_or("/"));
    std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf()).to_string_lossy().into_owned()
}

fn parse_id(value: &str) -> Option<u64> {
    let parsed = value.strip_prefix("pty_persistent_")?.parse::<u64>().ok()?;
    (1..=9_007_199_254_740_991).contains(&parsed).then_some(parsed)
}

fn from_id(value: &str) -> Result<u64, PersistentPtyError> {
    parse_id(value).ok_or_else(|| PersistentPtyError::NotFound { pty_id: value.to_owned() })
}

fn to_info(value: ProcessInfo) -> Result<PersistentPtyInfo, PersistentPtyError> {
    let Some(pty) = value.pty else {
        return Err(PersistentPtyError::Unavailable {
            message: format!("opencode-pty process {} is not a terminal", value.id),
        });
    };
    let (status, exit_code) = match value.lifecycle {
        TerminalLifecycle::Running => (PersistentPtyInfoStatus::Running, None),
        TerminalLifecycle::Exited { exit_code, .. } => (PersistentPtyInfoStatus::Exited, exit_code.map(f64::from)),
        TerminalLifecycle::Failed { .. } => (PersistentPtyInfoStatus::Exited, None),
    };
    Ok(PersistentPtyInfo {
        id: PtyID(format!("pty_persistent_{}", value.id)),
        title: pty.title,
        command: value.command.first().cloned().unwrap_or_default(),
        args: value.command.iter().skip(1).cloned().collect(),
        cwd: value.cwd.to_string_lossy().into_owned(),
        status,
        pid: value.pid.map_or(0.0, f64::from),
        exit_code,
        session_id: SessionID(value.group_id),
        user_title: pty.user_title,
        foreground_process: pty.foreground_process,
        size: PersistentPtyInfoSize { cols: f64::from(pty.cols), rows: f64::from(pty.rows) },
        output: PersistentPtyInfoOutput { head: value.output_head as f64, tail: value.output_tail as f64 },
    })
}

fn to_pipe_info(value: ProcessInfo) -> Result<PipeInfo, PersistentPtyError> {
    let Some(pipe) = value.pipe else {
        return Err(PersistentPtyError::Unavailable {
            message: format!("opencode-pty process {} is not a pipe process", value.id),
        });
    };
    Ok(PipeInfo {
        id: value.id,
        pid: value.pid,
        command: value.command,
        cwd: value.cwd,
        group_id: value.group_id,
        lifecycle: match value.lifecycle {
            TerminalLifecycle::Running => PipeLifecycle::Running,
            TerminalLifecycle::Exited { exit_code, signal } => PipeLifecycle::Exited { exit_code, signal },
            TerminalLifecycle::Failed { message } => PipeLifecycle::Failed { message },
        },
        output_tail: value.output_tail,
        output_lines: pipe.output_lines,
        output_path: pipe.output_path,
        output_error: pipe.output_error,
        metadata: pipe.metadata,
        started_at: value.started_at,
        exited_at: value.exited_at,
    })
}

fn info_value(info: &PersistentPtyInfo) -> Value {
    serde_json::from_str(&encode_json(info)).expect("a persistent PTY info round trips")
}

fn unavailable(error: DaemonError) -> PersistentPtyError {
    PersistentPtyError::Unavailable { message: error.message }
}

fn pipe_error(error: DaemonError) -> PersistentPtyError {
    match error.kind {
        ErrorKind::Request | ErrorKind::Response => PersistentPtyError::Request { message: error.message },
        _ => PersistentPtyError::Unavailable { message: error.message },
    }
}

fn unexpected(response: &Response) -> PersistentPtyError {
    PersistentPtyError::Unavailable {
        message: format!("unexpected opencode-pty response: {}", response_type(response)),
    }
}

fn pipe_unexpected(response: &Response) -> PersistentPtyError {
    PersistentPtyError::Request { message: format!("unexpected opencode-pty response: {}", response_type(response)) }
}

fn response_type(response: &Response) -> &'static str {
    match response {
        Response::Owned => "owned",
        Response::Handoff { .. } => "handoff",
        Response::Pong { .. } => "pong",
        Response::Created { .. } => "created",
        Response::Process { .. } => "process",
        Response::Processes { .. } => "processes",
        Response::Ok => "ok",
        Response::Snapshot { .. } => "snapshot",
        Response::Rows { .. } => "rows",
        Response::Replay { .. } => "replay",
        Response::Attached { .. } => "attached",
        Response::Resized { .. } => "resized",
        Response::Exited { .. } => "exited",
        Response::ControllerChanged { .. } => "controller_changed",
        Response::TitleChanged { .. } => "title_changed",
        Response::ForegroundProcessChanged { .. } => "foreground_process_changed",
        Response::Error { .. } => "error",
    }
}
