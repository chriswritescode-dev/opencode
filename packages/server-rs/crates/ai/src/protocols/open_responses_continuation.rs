use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::json::{canonical, encode_body};
use crate::protocols::open_responses;
use crate::protocols::open_responses_channel::Observer;
use crate::protocols::shared;
use crate::route::transport::channel::{
    ChannelCheckpoint, ChannelCreate, ChannelDriver, ChannelMode, ChannelObservation,
};
use crate::schema::{AiError, AiErrorReason, Delivery, Recovery, TransportKind, TransportOperation, TransportPhase};

pub const PROTOCOL: &str = "open-responses.websocket.v1";
pub const VERSION: u64 = 1;

pub type Shape = Arc<dyn Fn(&Map<String, Value>) -> Option<Map<String, Value>> + Send + Sync>;

pub struct DriverInput {
    pub id: String,
    pub name: String,
    pub request: Map<String, Value>,
    pub base: Observer,
    pub continuation: Option<Shape>,
}

struct CheckpointValue {
    response_id: String,
    request: Map<String, Value>,
    output: Vec<Value>,
}

pub struct Driver {
    id: String,
    name: String,
    request: Map<String, Value>,
    base: Observer,
    shape: Shape,
    output: Vec<Value>,
}

pub fn driver(input: DriverInput) -> Driver {
    let mut request = input.request;
    request.remove("previous_response_id");
    let shape = input.continuation.unwrap_or_else(|| Arc::new(|fields: &Map<String, Value>| Some(fields.clone())));
    Driver { id: input.id, name: input.name, request, base: input.base, shape, output: Vec::new() }
}

impl ChannelDriver for Driver {
    fn create(&mut self, checkpoint: Option<&ChannelCheckpoint>) -> Result<ChannelCreate, AiError> {
        self.output.clear();
        let previous = checkpoint.and_then(checkpoint_value);
        let fields = match previous.as_ref() {
            Some(_) => (self.shape)(&self.request),
            None => None,
        };
        let delta = match previous.as_ref() {
            Some(previous) if fields.is_some() => incremental(&self.request, previous),
            _ => None,
        };
        let (Some(previous), Some(fields), Some(delta)) = (previous, fields, delta) else {
            return Ok(ChannelCreate {
                message: encode_body(&Value::Object(self.request.clone())),
                mode: ChannelMode::Full,
            });
        };
        let mut message = fields;
        message.insert("input".to_string(), Value::Array(delta));
        message.insert("previous_response_id".to_string(), Value::String(previous.response_id));
        Ok(ChannelCreate { message: encode_body(&Value::Object(message)), mode: ChannelMode::Incremental })
    }

    fn observe(&mut self, create: &ChannelCreate, frame: &str) -> Result<ChannelObservation, AiError> {
        let event = open_responses::parse_event(&self.id, frame).map_err(|_| {
            shared::event_error(&self.id, format!("Invalid {} WebSocket event", self.name), Some(frame.to_string()))
        })?;
        let observation = self.base.observe(&event, frame)?;
        if event.get("type").and_then(Value::as_str) == Some("response.output_item.done")
            && let Some(item) = event.get("item").filter(|item| !item.is_null())
        {
            self.output.push(item.clone());
        }
        if let ChannelObservation::ProviderFailure { error } = &observation {
            let rejection = open_responses::error_detail(&event).code;
            if rejection.as_deref() == Some("previous_response_not_found") {
                return Ok(rejected(error, Recovery::RetryFull));
            }
            if rejection.as_deref() == Some("websocket_connection_limit_reached") {
                return Ok(rejected(error, Recovery::RotateAndRetryFull));
            }
            if matches!(create.mode, ChannelMode::Incremental)
                && matches!(&error.reason, AiErrorReason::InvalidRequest { classification: None, .. })
            {
                return Ok(rejected(error, Recovery::RetryFull));
            }
        }
        if !matches!(observation, ChannelObservation::Completed { .. }) {
            return Ok(observation);
        }
        if let Some(Value::Array(input)) = self.request.get("input")
            && input.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("compaction_trigger"))
        {
            return Ok(observation);
        }
        let Some(response_id) = event
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(observation);
        };
        let output = match event.get("response").and_then(|response| response.get("output")).and_then(Value::as_array) {
            Some(items) if !items.is_empty() => items.iter().map(|item| replacement(item, &self.output)).collect(),
            _ => self.output.clone(),
        };
        Ok(ChannelObservation::Completed {
            frame: frame.to_string(),
            checkpoint: Some(ChannelCheckpoint {
                protocol: PROTOCOL.to_string(),
                value: json!({
                    "version": VERSION,
                    "responseID": response_id,
                    "request": self.request,
                    "output": output,
                }),
            }),
        })
    }
}

fn replacement(item: &Value, output: &[Value]) -> Value {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return item.clone();
    }
    let Some(id) = item.get("id") else {
        return item.clone();
    };
    output
        .iter()
        .find(|done| done.get("type").and_then(Value::as_str) == Some("reasoning") && done.get("id") == Some(id))
        .cloned()
        .unwrap_or_else(|| item.clone())
}

fn checkpoint_value(checkpoint: &ChannelCheckpoint) -> Option<CheckpointValue> {
    if checkpoint.protocol != PROTOCOL {
        return None;
    }
    let value = checkpoint.value.as_object()?;
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return None;
    }
    let response_id = value.get("responseID").and_then(Value::as_str).filter(|id| !id.trim().is_empty())?;
    let request = value.get("request").and_then(Value::as_object)?;
    let output = value.get("output").and_then(Value::as_array)?;
    Some(CheckpointValue { response_id: response_id.to_string(), request: request.clone(), output: output.clone() })
}

fn invariant(request: &Map<String, Value>) -> Map<String, Value> {
    request
        .iter()
        .filter(|(key, _)| key.as_str() != "type" && key.as_str() != "input" && key.as_str() != "previous_response_id")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn incremental(request: &Map<String, Value>, checkpoint: &CheckpointValue) -> Option<Vec<Value>> {
    let input = request.get("input")?.as_array()?;
    let previous_input = checkpoint.request.get("input")?.as_array()?;
    if canonical(&Value::Object(invariant(request))) != canonical(&Value::Object(invariant(&checkpoint.request))) {
        return None;
    }
    let baseline = previous_input.iter().chain(checkpoint.output.iter()).collect::<Vec<&Value>>();
    if input.len() <= baseline.len() {
        return None;
    }
    let matches = baseline
        .iter()
        .enumerate()
        .all(|(index, item)| canonical(&comparable(item)) == canonical(&comparable(&input[index])));
    if !matches {
        return None;
    }
    Some(input[baseline.len()..].to_vec())
}

fn fields(entries: Vec<(&str, Option<Value>)>) -> Value {
    Value::Object(entries.into_iter().filter_map(|(key, value)| value.map(|value| (key.to_string(), value))).collect())
}

fn json_value(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let Some(text) = value.as_str() else {
        return Some(value.clone());
    };
    Some(serde_json::from_str(text).unwrap_or_else(|_| value.clone()))
}

fn comparable(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let kind = object.get("type").and_then(Value::as_str);
    if kind == Some("message") && object.get("role").and_then(Value::as_str) == Some("assistant") {
        let content = match object.get("content") {
            Some(Value::Array(parts)) => Some(Value::Array(
                parts
                    .iter()
                    .map(|part| match part {
                        Value::Object(part) if part.get("type").and_then(Value::as_str) == Some("output_text") => {
                            fields(vec![("type", part.get("type").cloned()), ("text", part.get("text").cloned())])
                        }
                        part => part.clone(),
                    })
                    .collect(),
            )),
            Some(content) => Some(content.clone()),
            None => None,
        };
        return fields(vec![
            ("role", Some(Value::String("assistant".to_string()))),
            ("content", content),
            ("phase", object.get("phase").cloned()),
        ]);
    }
    if kind == Some("function_call") {
        return fields(vec![
            ("type", object.get("type").cloned()),
            ("call_id", object.get("call_id").cloned()),
            ("name", object.get("name").cloned()),
            ("arguments", json_value(object.get("arguments"))),
        ]);
    }
    if kind == Some("custom_tool_call") {
        return fields(vec![
            ("type", object.get("type").cloned()),
            ("call_id", object.get("call_id").cloned()),
            ("name", object.get("name").cloned()),
            ("input", object.get("input").cloned()),
        ]);
    }
    if kind == Some("reasoning") {
        return fields(vec![
            ("type", object.get("type").cloned()),
            ("summary", object.get("summary").cloned()),
            ("encrypted_content", object.get("encrypted_content").cloned()),
        ]);
    }
    value.clone()
}

fn rejected(error: &AiError, recovery: Recovery) -> ChannelObservation {
    ChannelObservation::Rejected {
        error: AiError {
            reason: AiErrorReason::Transport {
                message: error.message().to_string(),
                body: error.body().map(str::to_string),
                http: error.http_context().cloned(),
                transport: TransportKind::WebSocket,
                operation: TransportOperation::Read,
                code: None,
                url: None,
                phase: Some(TransportPhase::Receive),
                delivery: Some(Delivery::Rejected),
                recovery: Some(recovery.clone()),
            },
        },
        recovery,
    }
}
