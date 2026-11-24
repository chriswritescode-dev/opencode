use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::protocols::open_responses;
use crate::protocols::shared;
use crate::protocols::utils::lifecycle;
use crate::route::protocol::{Frame, ProtocolState};
use crate::schema::{AiError, LlmEvent, ToolResultValue};

pub type ToolResultLowering = fn(&Value) -> Result<ToolResultValue, AiError>;

#[derive(Clone)]
pub struct Definition {
    pub name: &'static str,
    pub input: fn(&Value) -> Value,
    pub result: Option<ToolResultLowering>,
}

pub type Definitions = BTreeMap<&'static str, Definition>;

pub fn common() -> Definitions {
    let mut definitions = Definitions::new();
    definitions.insert("web_search_call", Definition { name: "web_search", input: action_input, result: None });
    definitions.insert("file_search_call", Definition { name: "file_search", input: file_search_input, result: None });
    definitions.insert(
        "code_interpreter_call",
        Definition { name: "code_interpreter", input: code_interpreter_input, result: None },
    );
    definitions.insert("mcp_call", Definition { name: "mcp", input: mcp_input, result: None });
    definitions
}

pub fn on_done(
    state: &mut open_responses::State,
    item: &Value,
    definitions: &Definitions,
) -> Result<Vec<LlmEvent>, AiError> {
    let Some(kind) = item.get("type").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    let Some(definition) = definitions.get(kind) else {
        return Ok(Vec::new());
    };
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut metadata = Map::new();
    metadata.insert("itemId".to_string(), Value::String(id.clone()));
    let provider_metadata = open_responses::provider_metadata(&state.provider_metadata_key, metadata);
    let mut events = Vec::new();
    state.lifecycle = lifecycle::step_start(state.lifecycle.clone(), &mut events);
    let name = definition.name.to_string();
    events.push(LlmEvent::ToolCall {
        id: id.clone(),
        name: name.clone(),
        namespace: None,
        input: (definition.input)(item),
        provider_executed: Some(true),
        provider_metadata: Some(provider_metadata.clone()),
    });
    events.push(LlmEvent::ToolResult {
        id,
        name,
        namespace: None,
        result: match definition.result {
            Some(result) => result(item)?,
            None => match item.get("error").filter(|error| !error.is_null()) {
                Some(error) => ToolResultValue::Error { value: error.clone() },
                None => ToolResultValue::Json { value: item.clone() },
            },
        },
        output: None,
        provider_executed: Some(true),
        provider_metadata: Some(provider_metadata),
    });
    Ok(events)
}

pub struct HostedState {
    pub state: open_responses::State,
    pub definitions: Definitions,
}

impl ProtocolState for HostedState {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = open_responses::parse_event(&self.state.id, &frame)?;
        let event = open_responses::normalize(&self.state, &event);
        if event.get("type").and_then(Value::as_str) == Some("response.reasoning_text.delta") {
            let Some(item_id) = event.get("item_id").and_then(Value::as_str) else {
                return Err(shared::event_error(
                    &self.state.id,
                    "response.reasoning_text.delta is missing item_id",
                    None,
                ));
            };
            return Ok(open_responses::on_reasoning_delta(&mut self.state, &event, item_id));
        }
        if event.get("type").and_then(Value::as_str) == Some("response.output_item.done")
            && let Some(item) = event.get("item").filter(|item| item.is_object())
            && item.get("type").and_then(Value::as_str).is_some_and(|kind| self.definitions.contains_key(kind))
        {
            return on_done(&mut self.state, item, &self.definitions);
        }
        open_responses::step(&mut self.state, &event)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        open_responses::parse_event(&self.state.id, frame)
            .map(|event| open_responses::terminal(&event))
            .unwrap_or(false)
    }
}

pub fn action_input(item: &Value) -> Value {
    item.get("action").filter(|action| !action.is_null()).cloned().unwrap_or_else(|| Value::Object(Map::new()))
}

fn file_search_input(item: &Value) -> Value {
    let mut object = Map::new();
    object.insert(
        "queries".to_string(),
        item.get("queries").filter(|queries| !queries.is_null()).cloned().unwrap_or_else(|| Value::Array(Vec::new())),
    );
    Value::Object(object)
}

fn code_interpreter_input(item: &Value) -> Value {
    fields_input(item, &["code", "container_id"])
}

fn mcp_input(item: &Value) -> Value {
    fields_input(item, &["server_label", "name", "arguments"])
}

pub(crate) fn fields_input(item: &Value, keys: &[&str]) -> Value {
    let mut object = Map::new();
    for key in keys {
        if let Some(value) = item.get(*key) {
            object.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(object)
}
