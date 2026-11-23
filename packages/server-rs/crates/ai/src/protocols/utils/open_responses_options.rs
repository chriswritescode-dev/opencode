use serde_json::{Map, Value};

use crate::schema::LlmRequest;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub store: Option<bool>,
    pub metadata: Option<Map<String, Value>>,
    pub safety_identifier: Option<String>,
    pub reasoning_effort: Option<String>,
    pub text_verbosity: Option<String>,
    pub service_tier: Option<String>,
    pub parallel_tool_calls: Option<bool>,
}

pub fn resolve(request: &LlmRequest) -> Options {
    let empty = Map::new();
    let input = request.provider_options.as_ref().unwrap_or(&empty);
    Options {
        store: input.get("store").and_then(Value::as_bool),
        metadata: metadata(input),
        safety_identifier: string(input, "safetyIdentifier"),
        reasoning_effort: string(input, "reasoningEffort"),
        text_verbosity: string(input, "textVerbosity"),
        service_tier: string(input, "serviceTier"),
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

fn metadata(input: &Map<String, Value>) -> Option<Map<String, Value>> {
    let metadata = input.get("metadata")?.as_object()?;
    metadata.values().all(Value::is_string).then(|| metadata.clone())
}
