pub mod builtin;
pub mod definition;
pub mod generated;
pub mod policy;
pub mod registry;
pub mod snapshot;

pub use definition::{Freeform, ToolDefinition, ToolOptions, ToolSpec};
pub use registry::{RegisteredTool, RegistrationFailure, ToolEditor, ToolNamespace, ToolRegistration, ToolRegistry};
pub use snapshot::{Snapshot, SnapshotOptions, ToolCall};

use futures_util::future::BoxFuture;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct ToolContext {
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub output: Value,
    pub content: Vec<Value>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        ToolError { message: message.into() }
    }
}

pub trait ToolRuntime: Send + Sync {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>>;
}
