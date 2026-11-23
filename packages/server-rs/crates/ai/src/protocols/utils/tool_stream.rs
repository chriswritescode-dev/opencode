use std::collections::BTreeMap;

use serde_json::Value;

use crate::schema::{AiError, AiErrorReason, LlmEvent, ProviderMetadata};

use super::partial_json;

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
    pub tools: State,
    pub tool: PendingTool,
    pub events: Vec<LlmEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishAll {
    pub tools: State,
    pub events: Vec<LlmEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishOutcome {
    pub tools: State,
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

pub fn start(mut tools: State, key: i64, tool: PendingTool) -> State {
    tools.insert(key, tool);
    tools
}

pub fn append_existing(
    route: &str,
    tools: State,
    key: i64,
    text: &str,
    missing: &str,
) -> Result<AppendOutcome, AiError> {
    match append(tools, key, text) {
        Some(outcome) => Ok(outcome),
        None => Err(invalid_output(route, missing.to_string(), None)),
    }
}

pub fn finish(route: &str, tools: State, key: i64, input: Option<&str>) -> Result<FinishOutcome, AiError> {
    let Some(tool) = tools.get(&key).cloned() else {
        return Ok(FinishOutcome { tools, events: Vec::new() });
    };
    let event = tool_call(route, &tool, input)?;
    let mut tools = tools;
    tools.remove(&key);
    Ok(FinishOutcome { tools, events: vec![input_end(&tool), event] })
}

pub fn append_or_start(
    route: &str,
    tools: State,
    key: i64,
    delta: AppendDelta<'_>,
    missing: &str,
) -> Result<AppendOutcome, AiError> {
    let current = tools.get(&key).cloned();
    let id = current.as_ref().map(|tool| tool.id.clone()).or_else(|| delta.id.map(str::to_string));
    let name = current.as_ref().map(|tool| tool.name.clone()).or_else(|| delta.name.map(str::to_string));
    let (Some(id), Some(name)) = (id, name) else {
        return Err(invalid_output(route, missing.to_string(), None));
    };
    let tool = PendingTool {
        id,
        name,
        namespace: current.as_ref().and_then(|tool| tool.namespace.clone()),
        input: current.as_ref().map_or_else(String::new, |tool| tool.input.clone()) + delta.text,
        provider_executed: current.as_ref().and_then(|tool| tool.provider_executed),
        provider_metadata: current.as_ref().and_then(|tool| tool.provider_metadata.clone()).or(delta.provider_metadata),
    };
    if let Some(current) = current
        && delta.text.is_empty()
        && current.id == tool.id
        && current.name == tool.name
    {
        return Ok(AppendOutcome { tools, tool: current, events: Vec::new() });
    }
    Ok(append_tool(tools, key, tool, delta.text))
}

pub fn append(tools: State, key: i64, text: &str) -> Option<AppendOutcome> {
    let current = tools.get(&key)?.clone();
    if text.is_empty() {
        return Some(AppendOutcome { tools, tool: current, events: Vec::new() });
    }
    let mut tool = current;
    tool.input.push_str(text);
    Some(append_tool(tools, key, tool, text))
}

pub fn finish_all(route: &str, tools: State) -> Result<FinishAll, AiError> {
    let mut events = Vec::new();
    for tool in tools.into_values() {
        events.push(input_end(&tool));
        events.push(tool_call(route, &tool, None)?);
    }
    Ok(FinishAll { tools: empty(), events })
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

fn append_tool(mut tools: State, key: i64, tool: PendingTool, text: &str) -> AppendOutcome {
    let mut events = Vec::new();
    if !tools.contains_key(&key) {
        events.push(input_start(&tool));
    }
    if !text.is_empty() {
        events.push(input_delta(&tool, text));
    }
    tools.insert(key, tool.clone());
    AppendOutcome { tools, tool, events }
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
    let input = partial_json::parse(&tool.input).unwrap_or_else(|| Value::Object(Default::default()));
    LlmEvent::ToolInputDelta {
        id: tool.id.clone(),
        name: tool.name.clone(),
        namespace: tool.namespace.clone(),
        text: text.to_string(),
        input: Some(input),
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
