use std::collections::BTreeMap;

use serde_json::Value;

use crate::schema::{AiError, AiErrorReason, LlmEvent, PartialToolInput, ProviderMetadata};

#[derive(Debug, Clone, PartialEq)]
pub struct PendingTool {
    pub id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub input: String,
    pub provider_executed: Option<bool>,
    pub provider_metadata: Option<ProviderMetadata>,
}

pub type State = BTreeMap<i64, PendingTool>;

#[derive(Debug, Clone, PartialEq)]
pub struct AppendOutcome {
    pub events: Vec<LlmEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishAll {
    pub events: Vec<LlmEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishOutcome {
    pub events: Vec<LlmEvent>,
}

#[derive(Debug, Clone)]
pub struct AppendDelta<'a> {
    pub id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub text: &'a str,
    pub provider_metadata: Option<ProviderMetadata>,
}

pub fn empty() -> State {
    State::new()
}

pub fn start(tools: &mut State, key: i64, tool: PendingTool) {
    tools.insert(key, tool);
}

pub fn append_existing(
    route: &str,
    tools: &mut State,
    key: i64,
    text: &str,
    missing: &str,
) -> Result<AppendOutcome, AiError> {
    match append(tools, key, text) {
        Some(outcome) => Ok(outcome),
        None => Err(invalid_output(route, missing.to_string(), None)),
    }
}

pub fn finish(route: &str, tools: &mut State, key: i64, input: Option<&str>) -> Result<FinishOutcome, AiError> {
    let Some(tool) = tools.get(&key) else {
        return Ok(FinishOutcome { events: Vec::new() });
    };
    let event = tool_call(route, tool, input)?;
    let end = input_end(tool);
    tools.remove(&key);
    Ok(FinishOutcome { events: vec![end, event] })
}

pub fn append_or_start(
    route: &str,
    tools: &mut State,
    key: i64,
    delta: AppendDelta<'_>,
    missing: &str,
) -> Result<AppendOutcome, AiError> {
    let existing = tools.get(&key);
    let id = existing.map_or(delta.id, |tool| Some(tool.id.as_str()));
    let name = existing.map_or(delta.name, |tool| Some(tool.name.as_str()));
    let (Some(id), Some(name)) = (id, name) else {
        return Err(invalid_output(route, missing.to_string(), None));
    };
    if existing.is_some() {
        let current = tools.get_mut(&key).expect("pending tool is present");
        if delta.text.is_empty() {
            return Ok(AppendOutcome { events: Vec::new() });
        }
        if current.provider_metadata.is_none() {
            current.provider_metadata = delta.provider_metadata;
        }
        current.input.push_str(delta.text);
        return Ok(append_tool(tools, key, delta.text, false));
    }
    tools.insert(
        key,
        PendingTool {
            id: id.to_string(),
            name: name.to_string(),
            namespace: None,
            input: delta.text.to_string(),
            provider_executed: None,
            provider_metadata: delta.provider_metadata,
        },
    );
    Ok(append_tool(tools, key, delta.text, true))
}

pub fn append(tools: &mut State, key: i64, text: &str) -> Option<AppendOutcome> {
    let current = tools.get_mut(&key)?;
    if text.is_empty() {
        return Some(AppendOutcome { events: Vec::new() });
    }
    current.input.push_str(text);
    Some(append_tool(tools, key, text, false))
}

pub fn finish_all(route: &str, tools: &mut State) -> Result<FinishAll, AiError> {
    let mut events = Vec::new();
    for tool in tools.values() {
        events.push(input_end(tool));
        events.push(tool_call(route, tool, None)?);
    }
    tools.clear();
    Ok(FinishAll { events })
}

fn tool_call(route: &str, tool: &PendingTool, input: Option<&str>) -> Result<LlmEvent, AiError> {
    let raw = input.unwrap_or(&tool.input);
    match parse_tool_input(raw) {
        Some(input) => Ok(LlmEvent::ToolCall {
            id: tool.id.clone(),
            name: tool.name.clone(),
            namespace: tool.namespace.clone(),
            input,
            provider_executed: tool.provider_executed.filter(|executed| *executed),
            provider_metadata: tool.provider_metadata.clone(),
        }),
        None if tool.provider_executed == Some(true) => Err(invalid_output(
            route,
            format!("Invalid JSON input for {route} tool call {}", tool.name),
            Some(raw.to_string()),
        )),
        None => Ok(LlmEvent::ToolInputError {
            id: tool.id.clone(),
            name: tool.name.clone(),
            namespace: tool.namespace.clone(),
            raw: raw.to_string(),
        }),
    }
}

fn append_tool(tools: &State, key: i64, text: &str, started: bool) -> AppendOutcome {
    let tool = tools.get(&key).expect("pending tool is present");
    let mut events = Vec::new();
    if started {
        events.push(input_start(tool));
    }
    if !text.is_empty() {
        events.push(input_delta(tool, text));
    }
    AppendOutcome { events }
}

fn input_start(tool: &PendingTool) -> LlmEvent {
    LlmEvent::ToolInputStart {
        id: tool.id.clone(),
        name: tool.name.clone(),
        namespace: tool.namespace.clone(),
        provider_executed: tool.provider_executed.filter(|executed| *executed),
        provider_metadata: tool.provider_metadata.clone(),
    }
}

fn input_end(tool: &PendingTool) -> LlmEvent {
    LlmEvent::ToolInputEnd {
        id: tool.id.clone(),
        name: tool.name.clone(),
        namespace: tool.namespace.clone(),
        provider_metadata: tool.provider_metadata.clone(),
    }
}

fn input_delta(tool: &PendingTool, text: &str) -> LlmEvent {
    LlmEvent::ToolInputDelta {
        id: tool.id.clone(),
        name: tool.name.clone(),
        namespace: tool.namespace.clone(),
        text: text.to_string(),
        input: Some(PartialToolInput::raw(tool.input.as_str())),
    }
}

fn parse_tool_input(raw: &str) -> Option<Value> {
    if raw.is_empty() {
        return Some(Value::Object(Default::default()));
    }
    serde_json::from_str::<Value>(raw).ok()
}

fn invalid_output(route: &str, message: String, body: Option<String>) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message,
            body,
            http: None,
            classification: None,
            route: Some(route.to_string()),
        },
    }
}
