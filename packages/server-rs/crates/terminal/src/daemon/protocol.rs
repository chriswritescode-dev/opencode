use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROTOCOL_VERSION: u32 = 9;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const OUTPUT_FRAME_TAG: u8 = 0;

pub type TerminalId = u64;
pub type PipeMetadata = Map<String, Value>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    Controller,
    Observer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    Pty,
    Pipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProcessSignal {
    #[serde(rename = "SIGTERM")]
    Term,
    #[serde(rename = "SIGINT")]
    Int,
    #[serde(rename = "SIGKILL")]
    Kill,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum TerminalLifecycle {
    Running,
    Exited {
        exit_code: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PtyInfo {
    pub title: String,
    pub user_title: Option<String>,
    pub foreground_process: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PipeInfo {
    pub output_path: PathBuf,
    pub output_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_error: Option<String>,
    pub metadata: PipeMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub id: TerminalId,
    pub mode: ProcessMode,
    pub pid: Option<u32>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub group_id: String,
    pub lifecycle: TerminalLifecycle,
    pub output_head: u64,
    pub output_tail: u64,
    pub started_at: u64,
    pub exited_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty: Option<PtyInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe: Option<PipeInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub instance_id: String,
    pub pid: u32,
    pub protocol: u32,
    pub socket: PathBuf,
    pub token: String,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub token: String,
    pub request: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum Request {
    Ping,
    Own {
        instance_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ticket: Option<String>,
    },
    PrepareHandoff,
    CreatePty {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        env: HashMap<String, String>,
        group_id: String,
        title: String,
        cols: u16,
        rows: u16,
    },
    CreatePipe {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        env: HashMap<String, String>,
        group_id: String,
        output_path: PathBuf,
        metadata: PipeMetadata,
    },
    List {
        mode: ProcessMode,
    },
    Get {
        id: TerminalId,
    },
    Wait {
        id: TerminalId,
    },
    Signal {
        id: TerminalId,
        signal: ProcessSignal,
    },
    SetMetadata {
        id: TerminalId,
        metadata: PipeMetadata,
    },
    Write {
        id: TerminalId,
        attachment_id: Option<String>,
        data_base64: String,
    },
    Resize {
        id: TerminalId,
        attachment_id: Option<String>,
        cols: u16,
        rows: u16,
    },
    Control {
        id: TerminalId,
        attachment_id: String,
        cols: u16,
        rows: u16,
    },
    Input {
        id: TerminalId,
        attachment_id: String,
        cols: u16,
        rows: u16,
        data_base64: String,
    },
    Snapshot {
        id: TerminalId,
    },
    ReadRows {
        id: TerminalId,
        rows: Option<u16>,
    },
    Rename {
        id: TerminalId,
        title: Option<String>,
    },
    Replay {
        id: TerminalId,
        offset: u64,
    },
    Subscribe {
        id: TerminalId,
        offset: u64,
        attachment_id: String,
        role: AttachmentRole,
        takeover: bool,
    },
    Terminate {
        id: TerminalId,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Response {
    Owned,
    Handoff {
        ticket: String,
        expires_at: u64,
    },
    Pong {
        instance_id: String,
        pid: u32,
        protocol: u32,
        features: Vec<String>,
    },
    Created {
        process: ProcessInfo,
    },
    Process {
        process: ProcessInfo,
    },
    Processes {
        processes: Vec<ProcessInfo>,
    },
    Ok,
    Snapshot {
        process: ProcessInfo,
        text: String,
        checkpoint_base64: String,
        cursor_x: u16,
        cursor_y: u16,
    },
    Rows {
        process: ProcessInfo,
        lines: Vec<String>,
        cursor_x: u16,
        cursor_y: u16,
    },
    Replay {
        requested_offset: u64,
        available_offset: u64,
        end_offset: u64,
        truncated: bool,
        data_base64: String,
    },
    Attached {
        process: ProcessInfo,
        role: AttachmentRole,
        generation: u64,
        requested_offset: u64,
        available_offset: u64,
        end_offset: u64,
        truncated: bool,
        replay_base64: String,
    },
    Resized {
        cols: u16,
        rows: u16,
        generation: u64,
        checkpoint_base64: String,
    },
    Exited {
        exit_code: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
        final_offset: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_lines: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_error: Option<String>,
        exited_at: u64,
    },
    ControllerChanged {
        attachment_id: Option<String>,
        generation: u64,
    },
    TitleChanged {
        title: String,
    },
    ForegroundProcessChanged {
        process: Option<String>,
    },
    Error {
        message: String,
    },
}

impl Request {
    pub fn op(&self) -> &'static str {
        match self {
            Request::Ping => "ping",
            Request::Own { .. } => "own",
            Request::PrepareHandoff => "prepare_handoff",
            Request::CreatePty { .. } => "create_pty",
            Request::CreatePipe { .. } => "create_pipe",
            Request::List { .. } => "list",
            Request::Get { .. } => "get",
            Request::Wait { .. } => "wait",
            Request::Signal { .. } => "signal",
            Request::SetMetadata { .. } => "set_metadata",
            Request::Write { .. } => "write",
            Request::Resize { .. } => "resize",
            Request::Control { .. } => "control",
            Request::Input { .. } => "input",
            Request::Snapshot { .. } => "snapshot",
            Request::ReadRows { .. } => "read_rows",
            Request::Rename { .. } => "rename",
            Request::Replay { .. } => "replay",
            Request::Subscribe { .. } => "subscribe",
            Request::Terminate { .. } => "terminate",
            Request::Shutdown => "shutdown",
        }
    }
}
