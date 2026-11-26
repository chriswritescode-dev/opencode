use crate::daemon::protocol::{AttachmentRole, ProcessInfo, Registration, Request, Response, TerminalId};
use crate::daemon::{DaemonError, DaemonOptions, ErrorKind, Handoff, Replay, SubscribeInput};

pub struct Daemon;

impl Daemon {
    pub async fn open(_options: DaemonOptions) -> Self {
        Daemon
    }

    pub async fn request(&self, _request: &Request, _should_start: bool) -> Result<Response, DaemonError> {
        Err(unavailable())
    }

    pub async fn request_if_running(&self, _request: &Request) -> Result<Option<Response>, DaemonError> {
        Err(unavailable())
    }

    pub async fn wait(&self, _id: TerminalId) -> Result<Response, DaemonError> {
        Err(unavailable())
    }

    pub async fn subscribe(&self, _id: TerminalId, _input: SubscribeInput) -> Result<DaemonAttachment, DaemonError> {
        Err(unavailable())
    }

    pub async fn shutdown(&self) -> Result<Option<Response>, DaemonError> {
        Err(unavailable())
    }

    pub async fn handoff(&self) -> Result<Option<Handoff>, DaemonError> {
        Err(unavailable())
    }

    pub async fn handoff_prepared(&self) -> bool {
        false
    }

    pub async fn registration(&self) -> Option<Registration> {
        None
    }
}

pub struct DaemonAttachment {
    pub terminal: ProcessInfo,
    pub role: AttachmentRole,
    pub generation: u64,
    pub replay: Replay,
}

impl DaemonAttachment {
    pub fn activate(&self) {}

    pub fn detach(&self) {}
}

fn unavailable() -> DaemonError {
    DaemonError::new(ErrorKind::Connect, "persistent PTYs are unavailable on this platform")
}
