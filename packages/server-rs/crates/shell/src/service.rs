use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_config::shell::{self, Priority, ShellSelect};
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::requests::ShellCreateBody;
use opencode_contract::generated::{
    LocationRef, ShellID, ShellInfo, ShellInfoStatus, ShellInfoTime, ShellOutputResponseData,
};
use opencode_util::lock;
use opencode_util::path::Flavor;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::output::{self, Limits};
use crate::result::Capture;
use crate::retention;
use crate::spawn;

pub const STOP_GRACE: Duration = Duration::from_secs(3);

pub struct Options {
    pub location: LocationRef,
    pub project_id: String,
    pub data: PathBuf,
    pub env: Vec<(String, String)>,
    pub shell: Arc<ShellSelect>,
    pub limits: Arc<dyn Fn() -> Limits + Send + Sync>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub session_id: String,
    pub assistant_message_id: String,
    pub call_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Timeout,
    Killed,
}

#[derive(Clone, Debug)]
pub struct CreateInput {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout: Option<u64>,
    pub metadata: Option<JsonMap<Value>>,
    pub shell: Option<String>,
    pub tool_call: Option<ToolCall>,
    pub waited: bool,
}

impl From<ShellCreateBody> for CreateInput {
    fn from(body: ShellCreateBody) -> Self {
        Self {
            command: body.command,
            cwd: body.cwd,
            timeout: body.timeout.map(|timeout| timeout.max(0.0) as u64),
            metadata: body.metadata,
            shell: None,
            tool_call: None,
            waited: false,
        }
    }
}

pub struct CreateBefore {
    pub command: String,
    pub cwd: String,
    pub timeout: u64,
    pub shell: String,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct CommandResult {
    pub info: ShellInfo,
    pub capture: Option<Capture>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ShellError {
    #[error("Shell.NotFoundError: {id}")]
    NotFound { id: String },
    #[error("failed to start {command}: {message}")]
    Spawn { command: String, message: String },
    #[error("{message}")]
    Io { message: String },
}

struct Command {
    info: ShellInfo,
    tracker: Arc<Mutex<spawn::Tracker>>,
    pid: u32,
    done: watch::Sender<Done>,
    stop_reason: Option<StopReason>,
    timeout: Option<JoinHandle<()>>,
    tool_call: Option<ToolCall>,
}

#[derive(Clone)]
enum Done {
    Running,
    Ended(Box<ShellInfo>),
    Removed,
}

struct State {
    commands: BTreeMap<String, Command>,
    exit_order: Vec<String>,
    pending_kills: BTreeMap<String, u32>,
}

pub struct Shell {
    inner: Arc<Inner>,
}

struct Inner {
    bus: Arc<Bus>,
    location: LocationRef,
    directory: PathBuf,
    output_dir: PathBuf,
    env: Vec<(String, String)>,
    shell: Arc<ShellSelect>,
    limits: Arc<dyn Fn() -> Limits + Send + Sync>,
    state: Mutex<State>,
}

impl Shell {
    pub fn new(bus: Arc<Bus>, options: Options) -> Arc<Self> {
        let output_dir = retention::output_directory(&options.data, &options.project_id);
        Arc::new(Self {
            inner: Arc::new(Inner {
                bus,
                location: options.location.clone(),
                directory: PathBuf::from(&options.location.directory.0),
                output_dir,
                env: options.env,
                shell: options.shell,
                limits: options.limits,
                state: Mutex::new(State {
                    commands: BTreeMap::new(),
                    exit_order: Vec::new(),
                    pending_kills: BTreeMap::new(),
                }),
            }),
        })
    }

    pub async fn create(
        &self,
        input: CreateInput,
        before: impl FnOnce(&mut CreateBefore),
    ) -> Result<ShellInfo, ShellError> {
        let mut invocation = CreateBefore {
            command: input.command,
            cwd: input.cwd.unwrap_or_else(|| self.inner.directory.to_string_lossy().into_owned()),
            timeout: input.timeout.unwrap_or(0),
            shell: input.shell.unwrap_or_else(|| self.inner.shell.resolve(Priority::Config)),
            env: self.inner.environment(),
        };
        before(&mut invocation);
        let cwd = Flavor::NATIVE.resolve(&[&self.inner.directory.to_string_lossy(), &invocation.cwd]);
        std::fs::create_dir_all(&self.inner.output_dir)
            .map_err(|error| ShellError::Io { message: error.to_string() })?;
        let id = format!("sh_{}", opencode_storage::id::ascending());
        let args = shell::args(&invocation.shell, &invocation.command);
        let file = self.inner.output_dir.join(format!("{id}.out"));
        let info = ShellInfo {
            id: ShellID(id.clone()),
            status: ShellInfoStatus::Running,
            command: invocation.command,
            cwd,
            shell: invocation.shell,
            file: file.to_string_lossy().into_owned(),
            pid: None,
            exit: None,
            signal: None,
            metadata: input.metadata.unwrap_or_default(),
            time: ShellInfoTime { started: now_ms(), completed: None },
        };
        let running = spawn::start(spawn::Spawn {
            program: &info.shell,
            args: &args,
            cwd: &info.cwd,
            env: &invocation.env,
            file: &file,
        })
        .map_err(|error| ShellError::Spawn { command: info.command.clone(), message: error.to_string() })?;
        let mut info = info;
        info.pid = Some(f64::from(running.pid));
        let (done, _receiver) = watch::channel(Done::Running);
        {
            let mut state = lock(&self.inner.state);
            state.commands.insert(
                id.clone(),
                Command {
                    info: info.clone(),
                    tracker: running.tracker.clone(),
                    pid: running.pid,
                    done,
                    stop_reason: None,
                    timeout: None,
                    tool_call: input.tool_call,
                },
            );
            self.inner.publish(
                "shell.created",
                json!({ "info": serde_json::from_str::<Value>(&encode_json(&info)).expect("the shell info encodes as JSON") }),
            );
        }
        if invocation.timeout != 0 {
            let _ = self.timeout(&id, invocation.timeout);
        }
        let weak = Arc::downgrade(&self.inner);
        running.supervise(move |exit| {
            if let Some(inner) = weak.upgrade() {
                inner.finish(&id, exit);
            }
        });
        Ok(info)
    }

    pub fn list(&self) -> Vec<ShellInfo> {
        lock(&self.inner.state)
            .commands
            .values()
            .filter(|command| command.info.status == ShellInfoStatus::Running)
            .map(|command| command.info.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<ShellInfo, ShellError> {
        lock(&self.inner.state).commands.get(id).map(|command| command.info.clone()).ok_or_else(|| not_found(id))
    }

    pub async fn wait(&self, id: &str) -> Result<ShellInfo, ShellError> {
        let mut receiver = {
            let state = lock(&self.inner.state);
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

    pub async fn result(&self, started: ShellInfo, wait: Option<Duration>) -> CommandResult {
        let id = started.id.0.clone();
        let info = match wait {
            None => self.wait(&id).await.unwrap_or_else(|_| killed_info(&started)),
            Some(duration) => match tokio::time::timeout(duration, self.wait(&id)).await {
                Ok(Ok(info)) => info,
                Ok(Err(_)) => killed_info(&started),
                Err(_) => self.get(&id).unwrap_or_else(|_| killed_info(&started)),
            },
        };
        let capture = self.capture(&info);
        CommandResult { info, capture }
    }

    pub fn timeout(&self, id: &str, duration: u64) -> Result<ShellInfo, ShellError> {
        let mut state = lock(&self.inner.state);
        let command = state.commands.get_mut(id).ok_or_else(|| not_found(id))?;
        if command.stop_reason.is_some() || command.info.status != ShellInfoStatus::Running {
            return Ok(command.info.clone());
        }
        if let Some(handle) = command.timeout.take() {
            handle.abort();
        }
        if duration != 0 {
            let weak = Arc::downgrade(&self.inner);
            let timer_id = id.to_owned();
            command.timeout = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(duration)).await;
                if let Some(inner) = weak.upgrade() {
                    let _ = inner.stop(&timer_id, StopReason::Timeout);
                }
            }));
        }
        Ok(command.info.clone())
    }

    pub fn stop(&self, id: &str, reason: StopReason) -> Result<ShellInfo, ShellError> {
        self.inner.stop(id, reason)
    }

    pub fn by_tool_call(&self, call: &ToolCall) -> Option<ShellInfo> {
        lock(&self.inner.state)
            .commands
            .values()
            .find(|command| command.tool_call.as_ref().is_some_and(|candidate| candidate == call))
            .map(|command| command.info.clone())
    }

    pub fn output(
        &self,
        id: &str,
        cursor: Option<u64>,
        limit: Option<u64>,
    ) -> Result<ShellOutputResponseData, ShellError> {
        let (running, file, snapshot) = {
            let state = lock(&self.inner.state);
            let command = state.commands.get(id).ok_or_else(|| not_found(id))?;
            (
                command.info.status == ShellInfoStatus::Running,
                PathBuf::from(&command.info.file),
                *lock(&command.tracker),
            )
        };
        Ok(output::page(&file, snapshot.size, cursor, limit, running))
    }

    pub fn remove(&self, id: &str) -> Result<(), ShellError> {
        let pid = {
            let mut state = lock(&self.inner.state);
            if !state.commands.contains_key(id) {
                return Err(not_found(id));
            }
            self.inner.remove_locked(&mut state, id)
        };
        if let Some(pid) = pid {
            spawn::ProcessGroup::new(pid).terminate();
            self.inner.escalate(id.to_owned());
        }
        Ok(())
    }

    pub fn watch<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }

    pub fn handoff_prepared(&self) -> bool {
        false
    }

    pub fn shutdown(&self) {
        let (running, pending) = {
            let mut state = lock(&self.inner.state);
            let mut running = Vec::new();
            for command in state.commands.values() {
                if let Some(handle) = command.timeout.as_ref() {
                    handle.abort();
                }
                if command.info.status == ShellInfoStatus::Running {
                    running.push(command.pid);
                }
            }
            let pending = state.pending_kills.values().copied().collect::<Vec<_>>();
            state.commands.clear();
            state.exit_order.clear();
            state.pending_kills.clear();
            (running, pending)
        };
        for pid in running {
            spawn::ProcessGroup::new(pid).terminate();
        }
        for pid in pending {
            spawn::ProcessGroup::new(pid).force();
        }
    }

    fn capture(&self, info: &ShellInfo) -> Option<Capture> {
        let snapshot = {
            let state = lock(&self.inner.state);
            let command = state.commands.get(&info.id.0)?;
            *lock(&command.tracker)
        };
        Some(output::tail(
            Path::new(&info.file),
            output::Snapshot { size: snapshot.size, newlines: snapshot.newlines, line_starts: snapshot.line_starts },
            (self.inner.limits)(),
            None,
        ))
    }
}

impl Inner {
    fn environment(&self) -> Vec<(String, String)> {
        let mut env = self.env.clone();
        set(&mut env, "TERM", "xterm-256color");
        set(&mut env, "OPENCODE_TERMINAL", "1");
        env
    }

    fn stop(self: &Arc<Self>, id: &str, reason: StopReason) -> Result<ShellInfo, ShellError> {
        let (info, pid) = {
            let mut state = lock(&self.state);
            let command = state.commands.get_mut(id).ok_or_else(|| not_found(id))?;
            if command.stop_reason.is_some() || command.info.status != ShellInfoStatus::Running {
                return Ok(command.info.clone());
            }
            command.stop_reason = Some(reason);
            if let Some(handle) = command.timeout.take() {
                handle.abort();
            }
            (command.info.clone(), command.pid)
        };
        spawn::ProcessGroup::new(pid).terminate();
        self.escalate(id.to_owned());
        Ok(info)
    }

    fn finish(&self, id: &str, exit: spawn::Exit) {
        let mut state = lock(&self.state);
        state.pending_kills.remove(id);
        let Some(command) = state.commands.get_mut(id) else {
            return;
        };
        if let Some(handle) = command.timeout.take() {
            handle.abort();
        }
        let status = match command.stop_reason {
            Some(StopReason::Timeout) => ShellInfoStatus::Timeout,
            Some(StopReason::Killed) => ShellInfoStatus::Killed,
            None => ShellInfoStatus::Exited,
        };
        command.info.status = status;
        command.info.exit = exit.code.map(f64::from);
        command.info.signal = exit.signal.map(spawn::signal_name);
        command.info.time.completed = Some(now_ms());
        let info = command.info.clone();
        command.done.send_replace(Done::Ended(Box::new(info)));
        let mut data = serde_json::Map::new();
        data.insert("id".to_owned(), json!(id));
        if let Some(exit) = exit.code {
            data.insert("exit".to_owned(), json!(exit));
        }
        data.insert("status".to_owned(), json!(status_name(status)));
        self.publish("shell.exited", Value::Object(data));
        state.exit_order.push(id.to_owned());
        while state.exit_order.len() > retention::EXITED_LIMIT {
            let Some(oldest) = state.exit_order.first().cloned() else {
                break;
            };
            self.remove_locked(&mut state, &oldest);
        }
    }

    fn remove_locked(&self, state: &mut State, id: &str) -> Option<u32> {
        state.exit_order.retain(|candidate| candidate != id);
        let mut command = state.commands.remove(id)?;
        if let Some(handle) = command.timeout.take() {
            handle.abort();
        }
        let running = command.info.status == ShellInfoStatus::Running;
        let mut pid = None;
        if running {
            command.done.send_replace(Done::Removed);
            pid = Some(command.pid);
            state.pending_kills.insert(id.to_owned(), command.pid);
            let data = json!({ "id": id, "status": "killed" });
            self.publish("shell.exited", data);
        }
        let _ = std::fs::remove_file(&command.info.file);
        self.publish("shell.deleted", json!({ "id": id }));
        pid
    }

    fn escalate(self: &Arc<Self>, id: String) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let force = {
                let mut state = lock(&inner.state);
                match state.commands.get(&id) {
                    Some(command) if command.info.status == ShellInfoStatus::Running => Some(command.pid),
                    Some(_) => None,
                    None => state.pending_kills.remove(&id),
                }
            };
            if let Some(pid) = force {
                spawn::ProcessGroup::new(pid).force();
            }
        });
    }

    fn publish(&self, r#type: &'static str, data: Value) {
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&Definition::ephemeral(r#type), data, options) {
            log::warn!("failed to publish {type}: {error}");
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|error| error.into_inner());
        for command in state.commands.values() {
            if command.info.status == ShellInfoStatus::Running {
                spawn::ProcessGroup::new(command.pid).terminate();
            }
        }
        for pid in state.pending_kills.values() {
            spawn::ProcessGroup::new(*pid).force();
        }
    }
}

fn not_found(id: &str) -> ShellError {
    ShellError::NotFound { id: id.to_owned() }
}

fn killed_info(started: &ShellInfo) -> ShellInfo {
    let mut info = started.clone();
    info.status = ShellInfoStatus::Killed;
    info.time.completed = Some(now_ms());
    info
}

fn set(env: &mut Vec<(String, String)>, name: &str, value: &str) {
    match env.iter_mut().find(|(key, _)| key == name) {
        Some(entry) => entry.1 = value.to_owned(),
        None => env.push((name.to_owned(), value.to_owned())),
    }
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn status_name(status: ShellInfoStatus) -> &'static str {
    match status {
        ShellInfoStatus::Running => "running",
        ShellInfoStatus::Exited => "exited",
        ShellInfoStatus::Timeout => "timeout",
        ShellInfoStatus::Killed => "killed",
    }
}
