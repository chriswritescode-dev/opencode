#[cfg(unix)]
mod binary;
#[cfg(unix)]
mod owner;
pub mod protocol;
#[cfg(unix)]
mod transport;
#[cfg(not(unix))]
mod unavailable;

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use tokio::sync::Mutex;
#[cfg(unix)]
use tokio::time::{sleep, timeout};

pub use protocol::{
    AttachmentRole, PipeMetadata, ProcessInfo, ProcessMode, ProcessSignal, PtyInfo, Registration, Request, Response,
    TerminalId, TerminalLifecycle,
};
#[cfg(unix)]
pub use transport::DaemonAttachment;
#[cfg(not(unix))]
pub use unavailable::{Daemon, DaemonAttachment};

#[cfg(unix)]
use protocol::PROTOCOL_VERSION;

#[cfg(unix)]
const START_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Connect,
    Response,
    Registration,
    Protocol,
    Spawn,
    Request,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("opencode-pty {kind:?}: {message}")]
pub struct DaemonError {
    pub kind: ErrorKind,
    pub message: String,
    pub pid: Option<u32>,
}

impl DaemonError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        DaemonError { kind, message: message.into(), pid: None }
    }

    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Connect, message)
    }

    pub fn response(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Response, message)
    }

    pub fn registration(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Registration, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub fn spawn(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Spawn, message)
    }

    pub fn request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Request, message)
    }

    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    pub directory: PathBuf,
    #[serde(rename = "instanceID")]
    pub instance_id: String,
    pub ticket: String,
    pub expires_at: u64,
}

#[derive(Clone)]
pub struct DaemonOptions {
    pub root: PathBuf,
    pub id: String,
    pub inherited: Option<Handoff>,
    pub env: Vec<(String, String)>,
}

pub struct SubscribeInput {
    pub cursor: u64,
    pub attachment_id: String,
    pub role: AttachmentRole,
    pub takeover: bool,
    pub on_event: std::sync::Arc<dyn Fn(StreamEvent) + Send + Sync>,
    pub on_end: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl Clone for SubscribeInput {
    fn clone(&self) -> Self {
        SubscribeInput {
            cursor: self.cursor,
            attachment_id: self.attachment_id.clone(),
            role: self.role,
            takeover: self.takeover,
            on_event: self.on_event.clone(),
            on_end: self.on_end.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum StreamEvent {
    Output { start: u64, end: u64, data: Vec<u8> },
    Resized { cols: u16, rows: u16, generation: u64, checkpoint: Vec<u8> },
    Exited { exit_code: Option<u32>, final_offset: u64 },
    ControllerChanged { attachment_id: Option<String>, generation: u64 },
    TitleChanged { title: String },
    ForegroundProcessChanged { process: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Replay {
    pub requested_offset: u64,
    pub available_offset: u64,
    pub end_offset: u64,
    pub truncated: bool,
    pub data: Vec<u8>,
}

#[cfg(unix)]
struct State {
    id: String,
    registration: Option<Registration>,
    owner: Option<owner::Owner>,
    incompatible: Option<DaemonError>,
    handoff_expires_at: Option<u64>,
}

#[cfg(unix)]
pub struct Daemon {
    root: PathBuf,
    env: Vec<(String, String)>,
    startup: Mutex<()>,
    state: Mutex<State>,
}

#[cfg(unix)]
impl Daemon {
    pub async fn open(options: DaemonOptions) -> Self {
        let daemon = Daemon {
            root: options.root,
            env: options.env,
            startup: Mutex::new(()),
            state: Mutex::new(State {
                id: options.id,
                registration: None,
                owner: None,
                incompatible: None,
                handoff_expires_at: None,
            }),
        };
        if let Some(inherited) = options.inherited {
            daemon.adopt(inherited).await;
        }
        daemon
    }

    pub async fn request(&self, request: &Request, should_start: bool) -> Result<Response, DaemonError> {
        match self.attempt(request, should_start).await {
            Ok(response) => Ok(response),
            Err(error) if error.kind == ErrorKind::Registration => self.attempt(request, should_start).await,
            Err(error) if error.kind == ErrorKind::Connect && should_start => {
                self.state.lock().await.registration = None;
                self.attempt(request, true).await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn request_if_running(&self, request: &Request) -> Result<Option<Response>, DaemonError> {
        match self.request(request, false).await {
            Ok(response) => Ok(Some(response)),
            Err(error) if error.kind == ErrorKind::Connect => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn wait(&self, id: TerminalId) -> Result<Response, DaemonError> {
        match self.wait_attempt(id).await {
            Err(error) if error.kind == ErrorKind::Registration => self.wait_attempt(id).await,
            result => result,
        }
    }

    pub async fn subscribe(&self, id: TerminalId, input: SubscribeInput) -> Result<DaemonAttachment, DaemonError> {
        match self.subscribe_attempt(id, &input).await {
            Err(error) if error.kind == ErrorKind::Registration => self.subscribe_attempt(id, &input).await,
            result => result,
        }
    }

    async fn wait_attempt(&self, id: TerminalId) -> Result<Response, DaemonError> {
        let current = self.connect(false).await?;
        let result = transport::wait(&current, id).await;
        if let Err(error) = &result
            && error.kind == ErrorKind::Registration
        {
            self.clear_registration(&current).await;
        }
        result
    }

    async fn subscribe_attempt(&self, id: TerminalId, input: &SubscribeInput) -> Result<DaemonAttachment, DaemonError> {
        let current = self.connect(false).await?;
        let result = transport::subscribe(&current, id, input.clone()).await;
        if let Err(error) = &result
            && error.kind == ErrorKind::Registration
        {
            self.clear_registration(&current).await;
        }
        result
    }

    pub async fn shutdown(&self) -> Result<Option<Response>, DaemonError> {
        let response = self.request_if_running(&Request::Shutdown).await?;
        {
            let mut state = self.state.lock().await;
            state.registration = None;
            state.handoff_expires_at = None;
        }
        let Some(response) = response else { return Ok(None) };
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let directory = self.directory().await;
            if discover(&directory).await.is_err() {
                return Ok(Some(response));
            }
            sleep(POLL_INTERVAL).await;
        }
        Err(DaemonError::connect("opencode-pty did not stop"))
    }

    pub async fn handoff(&self) -> Result<Option<Handoff>, DaemonError> {
        let _guard = self.startup.lock().await;
        let (owner, registration, id) = {
            let state = self.state.lock().await;
            (state.owner.clone(), state.registration.clone(), state.id.clone())
        };
        let (Some(owner), Some(registration)) = (owner, registration) else { return Ok(None) };
        let response = timeout(START_TIMEOUT, owner.exchange(Request::PrepareHandoff))
            .await
            .map_err(|_| DaemonError::response("PTY handoff preparation timed out"))??;
        let Response::Handoff { ticket, expires_at } = response else {
            return Err(DaemonError::protocol("Expected PTY handoff ticket"));
        };
        self.state.lock().await.handoff_expires_at = Some(expires_at);
        Ok(Some(Handoff { directory: self.root.join(id), instance_id: registration.instance_id, ticket, expires_at }))
    }

    pub async fn handoff_prepared(&self) -> bool {
        self.state.lock().await.handoff_expires_at.is_some_and(|expires_at| now_ms() < expires_at)
    }

    pub async fn registration(&self) -> Option<Registration> {
        self.state.lock().await.registration.clone()
    }

    async fn directory(&self) -> PathBuf {
        self.root.join(self.state.lock().await.id.clone())
    }

    async fn attempt(&self, request: &Request, should_start: bool) -> Result<Response, DaemonError> {
        let current = self.connect(should_start).await?;
        let result = transport::one_shot(&current, request).await;
        if let Err(error) = &result
            && error.kind == ErrorKind::Registration
        {
            self.clear_registration(&current).await;
        }
        result
    }

    async fn clear_registration(&self, current: &Registration) {
        let mut state = self.state.lock().await;
        if state.registration.as_ref() == Some(current) {
            state.registration = None;
        }
    }

    async fn connect(&self, should_start: bool) -> Result<Registration, DaemonError> {
        {
            let mut state = self.state.lock().await;
            if let Some(registration) = state.registration.clone() {
                if state.owner.as_ref().is_some_and(|owner| !owner.is_closed()) {
                    return Ok(registration);
                }
                state.registration = None;
                state.owner = None;
            }
        }
        let _guard = self.startup.lock().await;
        {
            let mut state = self.state.lock().await;
            if let Some(registration) = state.registration.clone() {
                if state.owner.as_ref().is_some_and(|owner| !owner.is_closed()) {
                    return Ok(registration);
                }
                state.registration = None;
                state.owner = None;
            }
        }
        let directory = self.directory().await;
        match discover(&directory).await {
            Ok(found) => {
                if self.state.lock().await.owner.is_none() {
                    self.claim(found.clone(), None).await?;
                }
                self.state.lock().await.registration = Some(found.clone());
                Ok(found)
            }
            Err(error) => {
                if should_start && error.kind == ErrorKind::Connect {
                    if let Some(incompatible) = self.state.lock().await.incompatible.clone() {
                        return Err(incompatible);
                    }
                    self.start().await
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn claim(&self, current: Registration, ticket: Option<String>) -> Result<(), DaemonError> {
        let opened = timeout(START_TIMEOUT, owner::open(&current, ticket.as_deref()))
            .await
            .map_err(|_| DaemonError::connect("PTY ownership claim timed out"))?;
        let opened = opened.map_err(|error| DaemonError::connect(error.message))?;
        let mut state = self.state.lock().await;
        state.owner = Some(opened);
        state.registration = Some(current);
        Ok(())
    }

    async fn start(&self) -> Result<Registration, DaemonError> {
        match self.spawn_daemon().await {
            Ok(found) => Ok(found),
            Err(error) => {
                let incompatible = self.state.lock().await.incompatible.clone();
                if error.kind == ErrorKind::Spawn || incompatible.as_ref() == Some(&error) {
                    return Err(error);
                }
                log::warn!("Starting opencode-pty in a fresh runtime directory: {}", error.message);
                self.abandon().await;
                self.spawn_daemon().await
            }
        }
    }

    async fn spawn_daemon(&self) -> Result<Registration, DaemonError> {
        let binary = binary::resolve(&self.env)?;
        let (id, root) = {
            let state = self.state.lock().await;
            (state.id.clone(), self.root.clone())
        };
        let mut command = tokio::process::Command::new(binary);
        command
            .arg("daemon")
            .arg("--runtime-dir")
            .arg(&root)
            .arg("--name")
            .arg(&id)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.envs(self.env.iter().map(|(key, value)| (key, value)));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let mut child = command.spawn().map_err(|error| DaemonError::spawn(error.to_string()))?;
        let directory = root.join(&id);
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        let mut last: Option<DaemonError> = None;
        while tokio::time::Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(error) => {
                    last = Some(DaemonError::spawn(error.to_string()));
                    break;
                }
            }
            match discover(&directory).await {
                Ok(found) => {
                    self.claim(found.clone(), None).await?;
                    return Ok(found);
                }
                Err(error) => {
                    if error.kind == ErrorKind::Protocol && error.pid.is_some() && error.pid == child.id() {
                        self.state.lock().await.incompatible = Some(error.clone());
                        let _ = child.start_kill();
                        return Err(error);
                    }
                    last = Some(error);
                }
            }
            sleep(POLL_INTERVAL).await;
        }
        let _ = child.start_kill();
        Err(last.unwrap_or_else(|| DaemonError::connect("opencode-pty did not become ready")))
    }

    async fn abandon(&self) {
        let mut state = self.state.lock().await;
        state.owner = None;
        state.registration = None;
        state.id = opencode_local::id::v4();
    }

    async fn adopt(&self, inherited: Handoff) {
        let id = inherited.directory.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        self.state.lock().await.id = id;
        if inherited.expires_at <= now_ms() {
            self.abandon().await;
            return;
        }
        let mut found: Option<Registration> = None;
        let result = async {
            let discovered = discover(&inherited.directory).await?;
            found = Some(discovered.clone());
            if discovered.instance_id != inherited.instance_id {
                return Err(DaemonError::registration("PTY restart daemon changed"));
            }
            self.claim(discovered, Some(inherited.ticket.clone())).await
        }
        .await;
        if let Err(error) = result {
            log::warn!("Discarding PTY restart handoff; persistent terminals will start fresh: {}", error.message);
            if let Some(found) = found {
                transport::one_shot(&found, &Request::Shutdown).await.ok();
            }
            self.abandon().await;
        }
    }
}

#[cfg(unix)]
async fn discover(directory: &Path) -> Result<Registration, DaemonError> {
    let value = tokio::fs::read_to_string(directory.join("service.json"))
        .await
        .map_err(|error| DaemonError::connect(error.to_string()))?;
    let found: Registration = serde_json::from_str(&value).map_err(|error| DaemonError::protocol(error.to_string()))?;
    if found.protocol != PROTOCOL_VERSION {
        return Err(DaemonError::protocol(format!(
            "opencode-pty protocol mismatch: daemon={}, client={PROTOCOL_VERSION}",
            found.protocol
        ))
        .with_pid(found.pid));
    }
    match transport::one_shot(&found, &Request::Ping).await? {
        Response::Pong { instance_id, pid, protocol, .. }
            if instance_id == found.instance_id && pid == found.pid && protocol == PROTOCOL_VERSION =>
        {
            Ok(found)
        }
        _ => Err(DaemonError::protocol("opencode-pty registration mismatch")),
    }
}

#[cfg(unix)]
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}
