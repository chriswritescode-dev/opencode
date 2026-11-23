use std::sync::Arc;

use serde_json::Value;

use crate::route::executor::BoxFuture;
use crate::route::framing::FrameStream;
use crate::route::transport::websocket::WebSocketRequest;
use crate::schema::{AiError, HttpContext, Recovery};

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelCheckpoint {
    pub protocol: String,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone)]
pub struct ChannelCreate {
    pub message: String,
    pub mode: ChannelMode,
}

pub enum ChannelObservation {
    Frame { frame: String },
    Completed { frame: String, checkpoint: Option<ChannelCheckpoint> },
    Incomplete { frame: String },
    ProviderFailure { error: AiError },
    Rejected { error: AiError, recovery: Recovery },
}

pub trait ChannelDriver: Send {
    fn create(&mut self, checkpoint: Option<&ChannelCheckpoint>) -> Result<ChannelCreate, AiError>;
    fn observe(&mut self, create: &ChannelCreate, frame: &str) -> Result<ChannelObservation, AiError>;
}

pub struct ChannelPreparation {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub rotate_after_ms: Option<u64>,
    pub driver: Box<dyn ChannelDriver>,
}

pub struct ChannelFallbackExecution {
    pub frames: FrameStream,
    pub http: Option<HttpContext>,
}

pub type ChannelFallback = Arc<dyn Fn() -> BoxFuture<Result<ChannelFallbackExecution, AiError>> + Send + Sync>;

pub struct ChannelExchange {
    pub id: String,
    pub connect: WebSocketRequest,
    pub rotate_after_ms: Option<u64>,
    pub fallback: ChannelFallback,
    pub driver: Box<dyn ChannelDriver>,
}

pub struct ChannelExecution {
    pub frames: FrameStream,
    pub http: Option<HttpContext>,
    pub complete: BoxFuture<()>,
}

pub trait ChannelExecutor: Send + Sync {
    fn execute(&self, exchange: ChannelExchange) -> BoxFuture<Result<ChannelExecution, AiError>>;
}

pub trait WebSocketInterceptor: Send + Sync {
    fn handshake(&self, connect: WebSocketRequest) -> WebSocketRequest {
        connect
    }

    fn send(&self, frame: String) -> String {
        frame
    }

    fn receive(&self, frame: String) -> String {
        frame
    }
}
