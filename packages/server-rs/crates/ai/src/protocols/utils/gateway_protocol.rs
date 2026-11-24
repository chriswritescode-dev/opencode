use std::sync::Arc;

use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::prepare::tool_updates::ToolUpdateEncoding;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::route::transport::channel::ChannelPreparation;
use crate::route::transport::http::JsonRequestParts;
use crate::schema::{AiError, LlmEvent, LlmRequest, SanitizerKind};

pub struct PreparedBody {
    pub request: LlmRequest,
    pub body: Map<String, Value>,
}

pub type Prepare = Arc<dyn Fn(&LlmRequest) -> Result<PreparedBody, AiError> + Send + Sync>;

pub struct GatewayProtocol {
    id: String,
    inner: Arc<dyn Protocol>,
    prepare: Prepare,
}

impl GatewayProtocol {
    pub fn new(id: impl Into<String>, inner: Arc<dyn Protocol>, prepare: Prepare) -> Self {
        Self { id: id.into(), inner, prepare }
    }
}

impl Protocol for GatewayProtocol {
    fn id(&self) -> &str {
        &self.id
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let prepared = (self.prepare)(&request.request)?;
        let resolved = ResolvedRequest { request: prepared.request, model: request.model.clone() };
        let body = self.inner.build_body(&resolved, model, context)?;
        Ok(overlay_body(body, prepared.body))
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        self.inner.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        self.inner.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(GatewayState { inner: self.inner.new_state(request, context), gateway: None })
    }

    fn required_headers(&self, request: &LlmRequest) -> Vec<(String, String)> {
        self.inner.required_headers(request)
    }

    fn prepare_channel(&self, model: &Model, parts: &JsonRequestParts) -> Option<Result<ChannelPreparation, AiError>> {
        self.inner.prepare_channel(model, parts)
    }

    fn sanitizer(&self) -> Option<SanitizerKind> {
        self.inner.sanitizer()
    }

    fn supports_effort_updates(&self, model: &Model, request: &LlmRequest) -> bool {
        self.inner.supports_effort_updates(model, request)
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        self.inner.tool_updates()
    }
}

struct GatewayState {
    inner: Box<dyn ProtocolState>,
    gateway: Option<Map<String, Value>>,
}

impl ProtocolState for GatewayState {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        if frame.contains("\"gateway\"")
            && let Ok(parsed) = serde_json::from_str(&frame)
        {
            let records = gateway_records(&parsed);
            if !records.is_empty() {
                let merged = match self.gateway.as_ref() {
                    Some(existing) => {
                        crate::json::merge_records(std::iter::once(existing).chain(records.iter().copied()))
                    }
                    None => crate::json::merge_records(records.iter().copied()),
                };
                self.gateway = merged;
            }
        }
        let events = self.inner.step(frame)?;
        Ok(attach_gateway(events, self.gateway.as_ref()))
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let events = self.inner.halt()?;
        Ok(attach_gateway(events, self.gateway.as_ref()))
    }

    fn continuation(&mut self) -> Option<Result<String, AiError>> {
        self.inner.continuation()
    }

    fn terminal(&self, frame: &Frame) -> bool {
        self.inner.terminal(frame)
    }
}

fn overlay_body(body: Value, prepared: Map<String, Value>) -> Value {
    let Value::Object(mut body) = body else {
        return body;
    };
    for (key, value) in prepared {
        body.insert(key, value);
    }
    Value::Object(body)
}

fn gateway_records(value: &Value) -> Vec<&Map<String, Value>> {
    let mut records = Vec::new();
    if let Some(gateway) = holder_gateway(value) {
        records.push(gateway);
    }
    if let Some(gateway) = value.get("response").and_then(holder_gateway) {
        records.push(gateway);
    }
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(gateway) = choice.get("delta").and_then(holder_gateway) {
                records.push(gateway);
            }
        }
    }
    records
}

fn holder_gateway(value: &Value) -> Option<&Map<String, Value>> {
    value.get("provider_metadata")?.get("gateway")?.as_object()
}

fn attach_gateway(events: Vec<LlmEvent>, gateway: Option<&Map<String, Value>>) -> Vec<LlmEvent> {
    let Some(gateway) = gateway else {
        return events;
    };
    events
        .into_iter()
        .map(|event| match event {
            LlmEvent::Finish { reason, usage, message, provider_metadata } => {
                let mut metadata = provider_metadata.unwrap_or_default();
                metadata.insert("gateway".to_string(), Value::Object(gateway.clone()));
                LlmEvent::Finish { reason, usage, message, provider_metadata: Some(metadata) }
            }
            event => event,
        })
        .collect()
}
