use serde_json::Value;

use crate::ExecutionContext;
use crate::prepare::tool_updates::ToolUpdateEncoding;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::schema::{AiError, LlmEvent, LlmRequest, SanitizerKind};

pub type Frame = String;

pub trait Protocol: Send + Sync {
    fn id(&self) -> &str;
    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        context: &ExecutionContext,
    ) -> Result<Value, AiError>;
    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        None
    }
    fn framing(&self) -> Definition;
    fn new_state(&self, _request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState>;
    fn required_headers(&self, _request: &LlmRequest) -> Vec<(String, String)> {
        Vec::new()
    }
    fn body_headers(&self, _model: &Model, _request: &LlmRequest, _body: &Value) -> Vec<(String, String)> {
        Vec::new()
    }
    fn sanitizer(&self) -> Option<SanitizerKind> {
        None
    }
    fn supports_effort_updates(&self, _model: &Model, _request: &LlmRequest) -> bool {
        false
    }
    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        None
    }
}

pub trait ProtocolState: Send {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError>;
    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        Ok(Vec::new())
    }
    fn terminal(&self, frame: &Frame) -> bool;
}
