use serde_json::{Map, Value};

use crate::schema::LlmRequest;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamOptions {
    pub include_obfuscation: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedToolsMode {
    Auto,
    None,
    Required,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AllowedTools {
    pub tool_names: Vec<String>,
    pub mode: AllowedToolsMode,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub store: Option<bool>,
    pub metadata: Option<Map<String, Value>>,
    pub safety_identifier: Option<String>,
    pub stream_options: Option<StreamOptions>,
    pub top_logprobs: Option<u64>,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub include: Option<Vec<String>>,
    pub text_verbosity: Option<String>,
    pub service_tier: Option<String>,
    pub truncation: Option<String>,
    pub allowed_tools: Option<AllowedTools>,
    pub max_tool_calls: Option<i64>,
    pub parallel_tool_calls: Option<bool>,
}

pub fn resolve(request: &LlmRequest) -> Options {
    let empty = Map::new();
    let input = request.provider_options.as_ref().unwrap_or(&empty);
    Options {
        store: input.get("store").and_then(Value::as_bool),
        metadata: metadata(input),
        safety_identifier: string(input, "safetyIdentifier"),
        stream_options: stream_options(input),
        top_logprobs: input
            .get("topLogprobs")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value <= 20),
        reasoning_effort: string(input, "reasoningEffort"),
        reasoning_summary: literal(input, "reasoningSummary", &["auto", "concise", "detailed"]),
        include: include(input),
        text_verbosity: string(input, "textVerbosity"),
        service_tier: string(input, "serviceTier"),
        truncation: literal(input, "truncation", &["auto", "disabled"]),
        allowed_tools: allowed_tools(input),
        max_tool_calls: input.get("maxToolCalls").and_then(Value::as_i64),
        parallel_tool_calls: input.get("parallelToolCalls").and_then(Value::as_bool),
    }
}

pub fn resolve_parallel_tool_calls(request: &LlmRequest, options: &Options) -> Option<bool> {
    if let Some(value) = options.parallel_tool_calls {
        return Some(value);
    }
    request.tool_choice.as_ref().and_then(|choice| choice.disable_parallel_tool_use).map(|disabled| !disabled)
}

fn string(input: &Map<String, Value>, key: &str) -> Option<String> {
    input.get(key)?.as_str().map(str::to_string)
}

fn literal(input: &Map<String, Value>, key: &str, allowed: &[&str]) -> Option<String> {
    let value = input.get(key)?.as_str()?;
    allowed.contains(&value).then(|| value.to_string())
}

fn metadata(input: &Map<String, Value>) -> Option<Map<String, Value>> {
    let metadata = input.get("metadata")?.as_object()?;
    metadata.values().all(Value::is_string).then(|| metadata.clone())
}

fn stream_options(input: &Map<String, Value>) -> Option<StreamOptions> {
    let options = input.get("streamOptions")?.as_object()?;
    match options.get("includeObfuscation") {
        None => Some(StreamOptions::default()),
        Some(Value::Bool(value)) => Some(StreamOptions { include_obfuscation: Some(*value) }),
        Some(_) => None,
    }
}

fn include(input: &Map<String, Value>) -> Option<Vec<String>> {
    let include = input.get("include")?.as_array()?;
    let values = include.iter().map(|value| value.as_str().map(str::to_string)).collect::<Option<Vec<String>>>()?;
    (!values.is_empty()).then_some(values)
}

fn allowed_tools(input: &Map<String, Value>) -> Option<AllowedTools> {
    let allowed = input.get("allowedTools")?.as_object()?;
    let tool_names = allowed.get("toolNames")?.as_array()?;
    let tool_names =
        tool_names.iter().map(|value| value.as_str().map(str::to_string)).collect::<Option<Vec<String>>>()?;
    if tool_names.is_empty() {
        return None;
    }
    let mode = match allowed.get("mode") {
        None => AllowedToolsMode::Auto,
        Some(value) => match value.as_str()? {
            "auto" => AllowedToolsMode::Auto,
            "none" => AllowedToolsMode::None,
            "required" => AllowedToolsMode::Required,
            _ => return None,
        },
    };
    Some(AllowedTools { tool_names, mode })
}
