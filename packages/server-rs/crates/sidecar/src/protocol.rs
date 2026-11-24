//! The messages both sides exchange. These Rust types are the definition: `tests/all/bindings.rs` turns them into
//! `host/src/protocol.gen.ts` through `ts-rs`, so the TypeScript host follows these serde attributes.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const VERSION: u32 = 1;
pub const MAGIC: &str = "opencode-sidecar";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
pub struct LocationRef {
    pub directory: String,
    #[serde(rename = "workspaceID", default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
pub struct RemoteError {
    #[serde(rename = "_tag")]
    pub tag: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// One protocol message. `Call` and `Cancel` name the sender's own call id; `Result`, `Error`, `Chunk` and `End`
/// are sent by the callee and name the caller's id; `Credit` is sent by the caller for its own stream call. Each
/// side keeps its own id counter starting at 1.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Message {
    Hello {
        version: u32,
        max_frame: u32,
        window: u32,
        heartbeat_ms: u32,
        test: bool,
    },
    Ready {
        magic: String,
        version: u32,
        pid: u32,
    },
    Call {
        #[ts(type = "number")]
        id: u64,
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        location: Option<LocationRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        parent: Option<u64>,
        params: serde_json::Value,
        stream: bool,
    },
    Result {
        #[ts(type = "number")]
        id: u64,
        value: serde_json::Value,
    },
    Error {
        #[ts(type = "number")]
        id: u64,
        error: RemoteError,
    },
    Cancel {
        #[ts(type = "number")]
        id: u64,
    },
    Chunk {
        #[ts(type = "number")]
        id: u64,
        value: serde_json::Value,
    },
    Credit {
        #[ts(type = "number")]
        id: u64,
        chunks: u32,
    },
    End {
        #[ts(type = "number")]
        id: u64,
    },
    Ping {
        #[ts(type = "number")]
        seq: u64,
    },
    Pong {
        #[ts(type = "number")]
        seq: u64,
        #[ts(type = "number")]
        rss: u64,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}
