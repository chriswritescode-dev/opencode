use serde_json::{Map, Value};

use crate::json::deep_merge;
use crate::prepare::{fold_tool_updates, strip_effort_updates};
use crate::protocols::utils::open_responses_options;
use crate::protocols::{open_responses, openai_freeform, openai_responses, shared};
use crate::route::endpoint::EndpointInput;
use crate::route::model::Model;
use crate::schema::{AiError, LlmRequest};

#[derive(Debug)]
pub struct CompactRequest {
    pub url: String,
    pub body: Value,
}

pub fn compact_request(request: &LlmRequest, model: &Model) -> Result<CompactRequest, AiError> {
    let folded = fold_tool_updates(model, strip_effort_updates(request.clone()));
    let native = open_responses::lower_conversation(&folded, model, &openai_responses::ADAPTER, None)?;
    let generation = open_responses::lower_generation(request, model, &open_responses_options::resolve(request));
    let tools = if folded.tools.is_empty() {
        None
    } else {
        let custom = openai_freeform::tools(&folded);
        Some(
            folded
                .tools
                .iter()
                .map(|entry| openai_responses::lower_tool_entry(entry, &custom))
                .collect::<Result<Vec<Value>, AiError>>()?,
        )
    };
    let mut base = Map::new();
    for key in ["model", "input"] {
        if let Some(value) = native.get(key) {
            base.insert(key.to_string(), value.clone());
        }
    }
    for key in ["service_tier", "reasoning", "text", "include", "parallel_tool_calls"] {
        if let Some(value) = generation.get(key) {
            base.insert(key.to_string(), value.clone());
        }
    }
    if let Some(tools) = tools {
        base.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(value) = generation.get("prompt_cache_key") {
        base.insert("prompt_cache_key".to_string(), value.clone());
    }
    let base = Value::Object(base);
    let merged = match request.http.as_ref().and_then(|http| http.body.as_ref()) {
        Some(overlay) => deep_merge(&base, overlay),
        None => base,
    };
    let body = project(&merged)?;
    let native = Value::Object(native);
    let mut url = model.route.endpoint.render(&EndpointInput { request, body: &native })?;
    let path = format!("{}/compact", url.path().strip_suffix('/').unwrap_or(url.path()));
    url.set_path(&path);
    Ok(CompactRequest { url: url.to_string(), body })
}

fn project(merged: &Value) -> Result<Value, AiError> {
    let source = merged.as_object().ok_or_else(|| expected("body", "an object"))?;
    let mut body = Map::new();
    let model = source.get("model").and_then(Value::as_str).ok_or_else(|| expected("model", "a string"))?;
    body.insert("model".to_string(), Value::String(model.to_string()));
    let input = source.get("input").and_then(Value::as_array).ok_or_else(|| expected("input", "an array"))?;
    body.insert("input".to_string(), Value::Array(input.clone()));
    push_nullable_string(&mut body, source, "instructions")?;
    push_nullable_string(&mut body, source, "previous_response_id")?;
    push_nullable_string(&mut body, source, "service_tier")?;
    push_object(&mut body, source, "reasoning")?;
    push_object(&mut body, source, "text")?;
    push_string_array(&mut body, source, "include")?;
    push_bool(&mut body, source, "parallel_tool_calls")?;
    push_object_array(&mut body, source, "tools")?;
    push_nullable_string(&mut body, source, "prompt_cache_key")?;
    push_nullable_string(&mut body, source, "prompt_cache_retention")?;
    push_cache_options(&mut body, source)?;
    Ok(Value::Object(body))
}

fn push_nullable_string(body: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) -> Result<(), AiError> {
    match source.get(key) {
        None => Ok(()),
        Some(Value::Null) => {
            body.insert(key.to_string(), Value::Null);
            Ok(())
        }
        Some(Value::String(value)) => {
            body.insert(key.to_string(), Value::String(value.clone()));
            Ok(())
        }
        Some(_) => Err(expected(key, "a string or null")),
    }
}

fn push_object(body: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) -> Result<(), AiError> {
    match source.get(key) {
        None => Ok(()),
        Some(Value::Object(value)) => {
            body.insert(key.to_string(), Value::Object(value.clone()));
            Ok(())
        }
        Some(_) => Err(expected(key, "an object")),
    }
}

fn push_string_array(body: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) -> Result<(), AiError> {
    match source.get(key) {
        None => Ok(()),
        Some(Value::Array(values)) if values.iter().all(Value::is_string) => {
            body.insert(key.to_string(), Value::Array(values.clone()));
            Ok(())
        }
        Some(_) => Err(expected(key, "an array of strings")),
    }
}

fn push_object_array(body: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) -> Result<(), AiError> {
    match source.get(key) {
        None => Ok(()),
        Some(Value::Array(values)) if values.iter().all(Value::is_object) => {
            body.insert(key.to_string(), Value::Array(values.clone()));
            Ok(())
        }
        Some(_) => Err(expected(key, "an array of objects")),
    }
}

fn push_bool(body: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) -> Result<(), AiError> {
    match source.get(key) {
        None => Ok(()),
        Some(Value::Bool(value)) => {
            body.insert(key.to_string(), Value::Bool(*value));
            Ok(())
        }
        Some(_) => Err(expected(key, "a boolean")),
    }
}

fn push_cache_options(body: &mut Map<String, Value>, source: &Map<String, Value>) -> Result<(), AiError> {
    let options = match source.get("prompt_cache_options") {
        None => return Ok(()),
        Some(Value::Null) => {
            body.insert("prompt_cache_options".to_string(), Value::Null);
            return Ok(());
        }
        Some(Value::Object(options)) => options,
        Some(_) => return Err(expected("prompt_cache_options", "an object or null")),
    };
    let mut projected = Map::new();
    for key in ["mode", "ttl"] {
        if let Some(value) = options.get(key) {
            let Some(value) = value.as_str() else {
                return Err(expected(key, "a string"));
            };
            projected.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    body.insert("prompt_cache_options".to_string(), Value::Object(projected));
    Ok(())
}

fn expected(key: &str, expectation: &str) -> AiError {
    shared::invalid_request(format!("OpenAI Responses invalid /compact body: {key} expected {expectation}"))
}
