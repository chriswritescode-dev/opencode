use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{AgentID, PermissionSource, PermissionSource2, PermissionSourceType, SessionID};
use opencode_fs::access::{Authorizer, Invocation};
use opencode_permission::service::{AssertInput, PermissionError};
use serde_json::Value;
use tokio::sync::watch;

pub type Progress = Arc<dyn Fn(JsonMap<Value>) + Send + Sync>;

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub agent: String,
    pub message_id: String,
    pub call_id: String,
    pub progress: Progress,
    pub cancel: Option<watch::Receiver<bool>>,
    pub authorizer: Arc<dyn Authorizer>,
}

impl Default for ToolContext {
    fn default() -> Self {
        ToolContext {
            session_id: String::new(),
            agent: String::new(),
            message_id: String::new(),
            call_id: String::new(),
            progress: Arc::new(|_| {}),
            cancel: None,
            authorizer: Arc::new(DenyAll),
        }
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("session_id", &self.session_id)
            .field("agent", &self.agent)
            .field("message_id", &self.message_id)
            .field("call_id", &self.call_id)
            .field("cancel", &self.cancel.is_some())
            .finish_non_exhaustive()
    }
}

pub fn invocation(context: &ToolContext) -> PermissionSource {
    PermissionSource2 {
        r#type: PermissionSourceType::Tool,
        message_id: context.message_id.clone(),
        id: context.call_id.clone(),
    }
}

pub fn file_invocation(context: &ToolContext) -> Invocation {
    Invocation {
        session_id: context.session_id.clone(),
        agent: Some(context.agent.clone()).filter(|agent| !agent.is_empty()),
        message_id: context.message_id.clone(),
        id: context.call_id.clone(),
    }
}

pub fn permission_input(context: &ToolContext, action: &str, resources: Vec<String>, save: Vec<String>) -> AssertInput {
    AssertInput {
        id: None,
        session_id: SessionID(context.session_id.clone()),
        agent: Some(AgentID(context.agent.clone())).filter(|agent| !agent.0.is_empty()),
        action: action.to_owned(),
        resources,
        save: Some(save),
        metadata: None,
        source: Some(invocation(context)),
        unattended: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text { text: String },
    File { uri: String, mime: String, name: Option<String> },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text { text: text.into() }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Content::Text { text } => serde_json::json!({ "type": "text", "text": text }),
            Content::File { uri, mime, name } => {
                let mut value = serde_json::json!({ "type": "file", "uri": uri, "mime": mime });
                if let Some(name) = name {
                    value["name"] = Value::String(name.clone());
                }
                value
            }
        }
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value.get("type").and_then(Value::as_str) {
            Some("text") => Some(Content::Text { text: value.get("text")?.as_str()?.to_owned() }),
            Some("file") => Some(Content::File {
                uri: value.get("uri")?.as_str()?.to_owned(),
                mime: value.get("mime")?.as_str()?.to_owned(),
                name: value.get("name").and_then(Value::as_str).map(str::to_owned),
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub output: Value,
    pub content: Vec<Content>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolErrorKind {
    Execution,
    Rejected,
    Declined,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
    pub kind: ToolErrorKind,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        ToolError { message: message.into(), kind: ToolErrorKind::Execution }
    }

    pub fn permission(error: impl std::borrow::Borrow<opencode_permission::service::PermissionError>) -> Self {
        match error.borrow() {
            error @ (opencode_permission::service::PermissionError::Blocked { .. }
            | opencode_permission::service::PermissionError::Corrected { .. }) => {
                ToolError { message: error.to_string(), kind: ToolErrorKind::Rejected }
            }
            opencode_permission::service::PermissionError::Declined => {
                ToolError { message: "Permission declined".to_owned(), kind: ToolErrorKind::Declined }
            }
            error => ToolError::new(error.to_string()),
        }
    }
}

pub trait ToolRuntime: Send + Sync {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>>;
}

pub struct Unrestricted;

impl Authorizer for Unrestricted {
    fn assert<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async { Ok(()) })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

pub struct DenyAll;

impl Authorizer for DenyAll {
    fn assert<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async {
            Err(PermissionError::Blocked {
                rules: Vec::new(),
                permission: "tool".to_owned(),
                resources: Vec::new(),
                reason: Some("No authorizer is configured for this tool call".to_owned()),
            })
        })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(false) })
    }
}
