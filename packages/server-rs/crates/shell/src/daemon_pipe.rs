use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{LocationRef, ShellInfo, ShellInfoStatus, ShellOutputResponseData};
use opencode_terminal::daemon::{PipeMetadata, ProcessSignal};
use opencode_terminal::persistent::{
    self, CreatePipeInput, PersistentPty, PersistentPtyError, PipeExit, PipeInfo, PipeLifecycle,
};
use opencode_util::lock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{OnceCell, watch};
use tokio::task::JoinHandle;

use crate::output::{self, Limits, LineStarts, Newlines, Tail};
use crate::result::Capture;
use crate::retention;
use crate::service::{STOP_GRACE, ShellError, StopReason, ToolCall};

pub const LOST: &str = "Command was killed: the process daemon stopped";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredLocation {
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    pub id: String,
    pub command: String,
    pub shell: String,
    pub location: StoredLocation,
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_deadline: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
}

impl Stored {
    pub fn from_metadata(metadata: &PipeMetadata) -> Option<Self> {
        serde_json::from_value(Value::Object(metadata.clone())).ok()
    }

    pub fn metadata(&self) -> PipeMetadata {
        match serde_json::to_value(self) {
            Ok(Value::Object(metadata)) => metadata,
            _ => Map::new(),
        }
    }

    pub fn output_metadata(metadata: &JsonMap<Value>) -> Value {
        Value::Object(metadata.0.iter().cloned().collect())
    }

    pub fn shell_metadata(metadata: Value) -> JsonMap<Value> {
        match metadata {
            Value::Object(entries) => JsonMap(entries.into_iter().collect()),
            _ => JsonMap::default(),
        }
    }
}

#[derive(Clone)]
pub struct DaemonPipe {
    pty: Arc<PersistentPty>,
}

impl DaemonPipe {
    pub fn new(pty: Arc<PersistentPty>) -> Self {
        Self { pty }
    }

    pub fn enabled(&self) -> bool {
        persistent::available()
    }

    pub async fn create(&self, input: CreatePipeInput) -> Result<PipeInfo, PersistentPtyError> {
        self.pty.create_pipe(input).await
    }

    pub async fn list(&self) -> Result<Vec<PipeInfo>, PersistentPtyError> {
        self.pty.list_pipes().await
    }

    pub async fn wait(&self, id: u64) -> Result<PipeExit, PersistentPtyError> {
        self.pty.wait_pipe(id).await
    }

    pub async fn signal(&self, id: u64, signal: ProcessSignal) -> Result<(), PersistentPtyError> {
        self.pty.signal_pipe(id, signal).await
    }

    pub async fn terminate(&self, id: u64) -> Result<(), PersistentPtyError> {
        self.pty.terminate_pipe(id).await
    }

    pub async fn store(&self, id: u64, stored: &Stored) -> Result<(), PersistentPtyError> {
        self.pty.set_pipe_metadata(id, stored.metadata()).await
    }

    pub async fn handoff_prepared(&self) -> bool {
        self.pty.handoff_prepared().await
    }
}

type PlaceKey = (String, Option<String>);

pub struct DaemonCommands {
    daemon: DaemonPipe,
    bus: Arc<Bus>,
    places: Mutex<HashMap<PlaceKey, Arc<Place>>>,
}

impl DaemonCommands {
    pub fn new(pty: Arc<PersistentPty>, bus: Arc<Bus>) -> Arc<Self> {
        Arc::new(Self { daemon: DaemonPipe::new(pty), bus, places: Mutex::new(HashMap::new()) })
    }

    pub fn enabled(&self) -> bool {
        self.daemon.enabled()
    }

    pub async fn handoff_prepared(&self) -> bool {
        self.daemon.handoff_prepared().await
    }

    pub async fn at(&self, location: &LocationRef, output_dir: PathBuf) -> Arc<Place> {
        let key = (location.directory.0.clone(), location.workspace_id.as_ref().map(|workspace| workspace.0.clone()));
        let place = {
            let mut places = lock(&self.places);
            places
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(Place {
                        daemon: self.daemon.clone(),
                        bus: self.bus.clone(),
                        location: location.clone(),
                        output_dir,
                        state: Mutex::new(PlaceState { commands: BTreeMap::new(), exit_order: Vec::new() }),
                        recovery: OnceCell::new(),
                    })
                })
                .clone()
        };
        place.recovered().await;
        place
    }

    pub async fn cleanup(&self, data: &Path) -> std::io::Result<()> {
        let processes = match self.daemon.list().await {
            Ok(processes) => processes,
            Err(error) => {
                log::warn!("failed to list shell commands to clean up: {error}");
                Vec::new()
            }
        };
        let running = processes
            .iter()
            .filter(|process| matches!(process.lifecycle, PipeLifecycle::Running))
            .map(|process| process.output_path.clone())
            .collect::<HashSet<_>>();
        retention::cleanup(data, &running)?;
        let cutoff = now_ms() as i64 - retention::RETENTION.as_millis() as i64;
        for process in processes {
            if running.contains(&process.output_path) {
                continue;
            }
            let ended = process.exited_at.unwrap_or(process.started_at) as i64;
            if ended >= cutoff && process.output_path.exists() {
                continue;
            }
            let _ = self.daemon.terminate(process.id).await;
        }
        Ok(())
    }
}

struct PlaceState {
    commands: BTreeMap<String, DaemonCommand>,
    exit_order: Vec<String>,
}

struct DaemonCommand {
    info: ShellInfo,
    pipe_id: u64,
    done: watch::Sender<Done>,
    stop_reason: Option<StopReason>,
    timeout: Option<JoinHandle<()>>,
    timeout_deadline: Option<f64>,
    output_lines: Option<u64>,
    output_error: Option<String>,
    killed: Option<String>,
    tool_call: Option<ToolCall>,
}

#[derive(Clone)]
enum Done {
    Running,
    Ended(Box<ShellInfo>),
    Removed,
}

struct Taken {
    file: String,
    pipe_id: u64,
}

pub struct Place {
    daemon: DaemonPipe,
    bus: Arc<Bus>,
    location: LocationRef,
    output_dir: PathBuf,
    state: Mutex<PlaceState>,
    recovery: OnceCell<watch::Receiver<bool>>,
}

impl Place {
    pub async fn recovered(self: &Arc<Self>) {
        let receiver = self
            .recovery
            .get_or_init(|| async {
                let (sender, receiver) = watch::channel(false);
                let place = self.clone();
                tokio::spawn(async move {
                    place.recover().await;
                    let _ = sender.send(true);
                });
                receiver
            })
            .await
            .clone();
        let mut receiver = receiver;
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn create(
        self: &Arc<Self>,
        mut info: ShellInfo,
        input: CreatePipeInput,
        timeout: u64,
        timeout_deadline: Option<f64>,
        tool_call: Option<ToolCall>,
    ) -> Result<ShellInfo, PersistentPtyError> {
        let pipe = self.daemon.create(input).await?;
        if let Some(pid) = pipe.pid {
            info.pid = Some(f64::from(pid));
        }
        info.time.started = pipe.started_at as f64;
        let (done, _receiver) = watch::channel(Done::Running);
        {
            let mut state = lock(&self.state);
            state.commands.insert(
                info.id.0.clone(),
                DaemonCommand {
                    info: info.clone(),
                    pipe_id: pipe.id,
                    done,
                    stop_reason: None,
                    timeout: None,
                    timeout_deadline,
                    output_lines: None,
                    output_error: None,
                    killed: None,
                    tool_call,
                },
            );
            self.publish(
                "shell.created",
                json!({ "info": serde_json::from_str::<Value>(&encode_json(&info)).expect("the shell info encodes as JSON") }),
            );
        }
        self.attach(pipe.id);
        self.arm_timeout(&info.id.0, timeout);
        Ok(info)
    }

    pub fn contains(&self, id: &str) -> bool {
        lock(&self.state).commands.contains_key(id)
    }

    pub fn list(&self) -> Vec<ShellInfo> {
        lock(&self.state)
            .commands
            .values()
            .filter(|command| command.info.status == ShellInfoStatus::Running)
            .map(|command| command.info.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<ShellInfo, ShellError> {
        lock(&self.state).commands.get(id).map(|command| command.info.clone()).ok_or_else(|| not_found(id))
    }

    pub async fn wait(&self, id: &str) -> Result<ShellInfo, ShellError> {
        let mut receiver = {
            let state = lock(&self.state);
            state.commands.get(id).ok_or_else(|| not_found(id))?.done.subscribe()
        };
        loop {
            if !matches!(*receiver.borrow_and_update(), Done::Running) {
                break;
            }
            if receiver.changed().await.is_err() {
                return Err(not_found(id));
            }
        }
        match receiver.borrow().clone() {
            Done::Ended(info) => Ok(*info),
            Done::Running | Done::Removed => Err(not_found(id)),
        }
    }

    pub async fn timeout(self: &Arc<Self>, id: &str, duration: u64) -> Result<ShellInfo, ShellError> {
        let (info, pipe_id, stored) = {
            let mut state = lock(&self.state);
            let command = state.commands.get_mut(id).ok_or_else(|| not_found(id))?;
            if command.stop_reason.is_some() || command.info.status != ShellInfoStatus::Running {
                return Ok(command.info.clone());
            }
            if let Some(handle) = command.timeout.take() {
                handle.abort();
            }
            command.timeout_deadline = (duration != 0).then(|| now_ms() + duration as f64);
            (command.info.clone(), command.pipe_id, stored(&self.location, command))
        };
        self.store(pipe_id, stored).await;
        if duration != 0 {
            self.arm_timeout(id, duration);
        }
        Ok(info)
    }

    pub async fn stop(self: &Arc<Self>, id: &str, reason: StopReason) -> Result<ShellInfo, ShellError> {
        let (info, pipe_id, stored) = {
            let mut state = lock(&self.state);
            let command = state.commands.get_mut(id).ok_or_else(|| not_found(id))?;
            if command.stop_reason.is_some() || command.info.status != ShellInfoStatus::Running {
                return Ok(command.info.clone());
            }
            command.stop_reason = Some(reason);
            command.timeout_deadline = None;
            let stored = stored(&self.location, command);
            (command.info.clone(), command.pipe_id, stored)
        };
        self.store(pipe_id, stored).await;
        if let Err(error) = self.daemon.signal(pipe_id, ProcessSignal::Term).await {
            log::warn!("failed to stop shell command {id}: {error}");
        }
        self.escalate(id.to_owned(), pipe_id);
        Ok(info)
    }

    pub async fn remove(self: &Arc<Self>, id: &str) -> Result<(), ShellError> {
        let taken = {
            let mut state = lock(&self.state);
            state.exit_order.retain(|candidate| candidate != id);
            let Some(mut command) = state.commands.remove(id) else {
                return Err(not_found(id));
            };
            if let Some(handle) = command.timeout.take() {
                handle.abort();
            }
            if command.info.status == ShellInfoStatus::Running {
                command.done.send_replace(Done::Removed);
                self.publish("shell.exited", json!({ "id": id, "status": "killed" }));
            }
            Taken { file: command.info.file, pipe_id: command.pipe_id }
        };
        if let Err(error) = self.daemon.terminate(taken.pipe_id).await {
            log::warn!("failed to remove shell command {id} from the daemon: {error}");
        }
        let _ = std::fs::remove_file(&taken.file);
        self.publish("shell.deleted", json!({ "id": id }));
        Ok(())
    }

    pub fn output(
        &self,
        id: &str,
        cursor: Option<u64>,
        limit: Option<u64>,
    ) -> Result<ShellOutputResponseData, ShellError> {
        let (running, file) = {
            let state = lock(&self.state);
            let command = state.commands.get(id).ok_or_else(|| not_found(id))?;
            (command.info.status == ShellInfoStatus::Running, PathBuf::from(&command.info.file))
        };
        Ok(output::page(&file, file_size(&file), cursor, limit, running))
    }

    pub fn by_tool_call(&self, call: &ToolCall) -> Option<ShellInfo> {
        lock(&self.state)
            .commands
            .values()
            .find(|command| command.tool_call.as_ref().is_some_and(|candidate| candidate == call))
            .map(|command| command.info.clone())
    }

    pub fn capture(&self, info: &ShellInfo, limits: Limits) -> Option<Capture> {
        let (output_lines, output_error, killed) = {
            let state = lock(&self.state);
            let command = state.commands.get(&info.id.0)?;
            (command.output_lines, command.output_error.clone(), command.killed.clone())
        };
        let file = Path::new(&info.file);
        Some(output::tail(
            file,
            Tail {
                size: file_size(file),
                newlines: output_lines.map_or(Newlines::Scan, Newlines::Known),
                line_starts: LineStarts::Scan,
                stopped_early: output_error,
            },
            limits,
            killed,
        ))
    }

    fn attach(self: &Arc<Self>, pipe_id: u64) {
        let place = self.clone();
        let daemon = self.daemon.clone();
        tokio::spawn(async move {
            let result = daemon.wait(pipe_id).await;
            match result {
                Ok(exit) => place.finish(pipe_id, exit).await,
                Err(error) => place.lose(pipe_id, error).await,
            }
        });
    }

    async fn store(&self, pipe_id: u64, stored: Stored) {
        if let Err(error) = self.daemon.store(pipe_id, &stored).await {
            log::warn!("failed to update shell command metadata: {error}");
        }
    }

    fn arm_timeout(self: &Arc<Self>, id: &str, duration: u64) {
        if duration == 0 {
            return;
        }
        let mut state = lock(&self.state);
        let Some(command) = state.commands.get_mut(id) else { return };
        if command.stop_reason.is_some() || command.info.status != ShellInfoStatus::Running {
            return;
        }
        if let Some(handle) = command.timeout.take() {
            handle.abort();
        }
        let place = self.clone();
        let timer_id = id.to_owned();
        command.timeout = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(duration)).await;
            let _ = place.stop(&timer_id, StopReason::Timeout).await;
        }));
    }

    async fn finish(self: &Arc<Self>, pipe_id: u64, exit: PipeExit) {
        let finished = {
            let mut state = lock(&self.state);
            let Some(id) =
                state.commands.iter().find(|(_, command)| command.pipe_id == pipe_id).map(|(id, _)| id.clone())
            else {
                return;
            };
            let command = state.commands.get_mut(&id).expect("the command was found");
            if let Some(handle) = command.timeout.take() {
                handle.abort();
            }
            let status = status_for(command.stop_reason);
            command.info.status = status;
            command.info.exit = exit.exit_code.map(f64::from);
            command.info.signal = exit.signal;
            command.info.time.completed = Some(exit.exited_at as f64);
            command.output_lines = Some(exit.output_lines);
            command.output_error = exit.output_error;
            let info = command.info.clone();
            command.done.send_replace(Done::Ended(Box::new(info)));
            self.publish_exited(&id, &command.info, status);
            state.exit_order.push(id);
            true
        };
        if finished {
            self.evict().await;
        }
    }

    async fn lose(self: &Arc<Self>, pipe_id: u64, error: PersistentPtyError) {
        log::warn!("lost a shell command's process: {error}");
        {
            let mut state = lock(&self.state);
            let Some(id) =
                state.commands.iter().find(|(_, command)| command.pipe_id == pipe_id).map(|(id, _)| id.clone())
            else {
                return;
            };
            let command = state.commands.get_mut(&id).expect("the command was found");
            if let Some(handle) = command.timeout.take() {
                handle.abort();
            }
            command.info.status = ShellInfoStatus::Killed;
            command.info.time.completed = Some(now_ms());
            command.killed = Some(LOST.to_owned());
            let info = command.info.clone();
            command.done.send_replace(Done::Ended(Box::new(info)));
            self.publish_exited(&id, &command.info, ShellInfoStatus::Killed);
            state.exit_order.push(id);
        }
        self.evict().await;
    }

    async fn evict(self: &Arc<Self>) {
        loop {
            let oldest = {
                let state = lock(&self.state);
                if state.exit_order.len() > retention::EXITED_LIMIT { state.exit_order.first().cloned() } else { None }
            };
            let Some(id) = oldest else { break };
            let _ = self.remove(&id).await;
        }
    }

    async fn recover(self: &Arc<Self>) {
        let pipes = match self.daemon.list().await {
            Ok(pipes) => pipes,
            Err(error) => {
                log::warn!("failed to list shell commands to recover: {error}");
                return;
            }
        };
        let mut finished = Vec::new();
        for pipe in pipes {
            let Some(stored) = Stored::from_metadata(&pipe.metadata) else {
                continue;
            };
            if stored.location.directory != self.location.directory.0
                || stored.location.workspace_id
                    != self.location.workspace_id.as_ref().map(|workspace| workspace.0.clone())
                || pipe.output_path.parent() != Some(self.output_dir.as_path())
                || self.contains(&stored.id)
            {
                continue;
            }
            let (running, exited, failed) = match &pipe.lifecycle {
                PipeLifecycle::Running => (true, None, None),
                PipeLifecycle::Exited { exit_code, signal } => (false, Some((*exit_code, signal.clone())), None),
                PipeLifecycle::Failed { message } => (false, None, Some(message.clone())),
            };
            let status = if running {
                ShellInfoStatus::Running
            } else {
                match stored.stop_reason {
                    Some(StopReason::Timeout) => ShellInfoStatus::Timeout,
                    Some(StopReason::Killed) => ShellInfoStatus::Killed,
                    None if exited.is_some() => ShellInfoStatus::Exited,
                    None => ShellInfoStatus::Killed,
                }
            };
            let info = ShellInfo {
                id: opencode_contract::generated::ShellID(stored.id.clone()),
                status,
                command: stored.command,
                cwd: pipe.cwd.to_string_lossy().into_owned(),
                shell: stored.shell,
                file: pipe.output_path.to_string_lossy().into_owned(),
                pid: pipe.pid.map(f64::from),
                exit: exited.as_ref().and_then(|(code, _)| *code).map(f64::from),
                signal: exited.as_ref().and_then(|(_, signal)| signal.clone()),
                metadata: Stored::shell_metadata(stored.metadata),
                time: opencode_contract::generated::ShellInfoTime {
                    started: pipe.started_at as f64,
                    completed: (!running).then(|| pipe.exited_at.unwrap_or(pipe.started_at) as f64),
                },
            };
            let killed =
                failed.filter(|_| stored.stop_reason.is_none()).map(|message| format!("Command was killed: {message}"));
            let (done, _receiver) = watch::channel(Done::Running);
            let command = DaemonCommand {
                info: info.clone(),
                pipe_id: pipe.id,
                done: done.clone(),
                stop_reason: stored.stop_reason,
                timeout: None,
                timeout_deadline: stored.timeout_deadline,
                output_lines: (!running).then_some(pipe.output_lines),
                output_error: pipe.output_error.clone().filter(|_| !running),
                killed,
                tool_call: stored.tool_call,
            };
            lock(&self.state).commands.insert(info.id.0.clone(), command);
            if running {
                self.attach(pipe.id);
                if stored.stop_reason.is_some() {
                    let _ = self.daemon.signal(pipe.id, ProcessSignal::Kill).await;
                } else if let Some(deadline) = stored.timeout_deadline {
                    let remaining = deadline - now_ms();
                    if remaining <= 0.0 {
                        let _ = self.stop(&info.id.0, StopReason::Timeout).await;
                    } else {
                        let _ = self.timeout(&info.id.0, remaining.ceil() as u64).await;
                    }
                }
                continue;
            }
            done.send_replace(Done::Ended(Box::new(info.clone())));
            finished.push(info);
        }
        finished
            .sort_by(|left, right| left.time.completed.unwrap_or(0.0).total_cmp(&right.time.completed.unwrap_or(0.0)));
        {
            let mut state = lock(&self.state);
            state.exit_order.extend(finished.into_iter().map(|info| info.id.0));
        }
        self.evict().await;
    }

    fn escalate(self: &Arc<Self>, id: String, pipe_id: u64) {
        let place = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            let running = {
                let state = lock(&place.state);
                state.commands.get(&id).is_some_and(|command| {
                    command.info.status == ShellInfoStatus::Running && command.pipe_id == pipe_id
                })
            };
            if running {
                let _ = place.daemon.signal(pipe_id, ProcessSignal::Kill).await;
            }
        });
    }

    fn publish_exited(&self, id: &str, info: &ShellInfo, status: ShellInfoStatus) {
        let mut data = serde_json::Map::new();
        data.insert("id".to_owned(), json!(id));
        if let Some(exit) = info.exit {
            data.insert("exit".to_owned(), json!(exit));
        }
        data.insert("status".to_owned(), json!(status_name(status)));
        self.publish("shell.exited", Value::Object(data));
    }

    fn publish(&self, r#type: &'static str, data: Value) {
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&Definition::ephemeral(r#type), data, options) {
            log::warn!("failed to publish {type}: {error}");
        }
    }
}

fn stored(location: &LocationRef, command: &DaemonCommand) -> Stored {
    Stored {
        id: command.info.id.0.clone(),
        command: command.info.command.clone(),
        shell: command.info.shell.clone(),
        location: StoredLocation {
            directory: location.directory.0.clone(),
            workspace_id: location.workspace_id.as_ref().map(|workspace| workspace.0.clone()),
        },
        metadata: Stored::output_metadata(&command.info.metadata),
        tool_call: command.tool_call.clone(),
        timeout_deadline: command.timeout_deadline,
        stop_reason: command.stop_reason,
    }
}

fn not_found(id: &str) -> ShellError {
    ShellError::NotFound { id: id.to_owned() }
}

fn status_for(reason: Option<StopReason>) -> ShellInfoStatus {
    match reason {
        Some(StopReason::Timeout) => ShellInfoStatus::Timeout,
        Some(StopReason::Killed) => ShellInfoStatus::Killed,
        None => ShellInfoStatus::Exited,
    }
}

fn status_name(status: ShellInfoStatus) -> &'static str {
    match status {
        ShellInfoStatus::Running => "running",
        ShellInfoStatus::Exited => "exited",
        ShellInfoStatus::Timeout => "timeout",
        ShellInfoStatus::Killed => "killed",
    }
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}
