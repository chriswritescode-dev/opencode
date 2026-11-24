use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::extraction::{ExtractionRequest, ExtractionResponse};
use crate::schema::{AiError, ProviderId, ProviderMetadata, Usage};

pub const ID: &str = "cloudflare-run-extraction";
pub const NAME: &str = "Cloudflare Run";
const PROVIDER: &str = "cloudflare-workers-ai";

pub fn protocol() -> InlineProtocol<ExtractionRequest, ExtractionResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: Vec::new(),
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

fn body(request: &ExtractionRequest) -> Result<Body, AiError> {
    let provider_options = request.provider_options.clone().unwrap_or_default();
    let http = request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default();
    let mut value = crate::json::merge_records([&provider_options, &http]).unwrap_or_default();
    value.insert("model".to_string(), Value::String(request.model.id.clone()));
    value.insert("state".to_string(), Value::String(request.input.clone()));
    let mut fields = Map::new();
    for (name, field) in &request.fields {
        let Some(field) = field.as_object() else {
            fields.insert(name.clone(), field.clone());
            continue;
        };
        let mut entries = Map::new();
        let mut null_threshold = None;
        for (key, entry) in field {
            if key == "nullThreshold" {
                null_threshold = Some(entry.clone());
                continue;
            }
            entries.insert(key.clone(), entry.clone());
        }
        if let Some(null_threshold) = null_threshold {
            entries.insert("null_threshold".to_string(), null_threshold);
        }
        fields.insert(name.clone(), Value::Object(entries));
    }
    value.insert("fields".to_string(), Value::Object(fields));
    if let Some(reference_date) = &request.reference_date {
        value.insert("reference_date".to_string(), Value::String(reference_date.clone()));
    }
    Ok(Body::Json { value, query: None })
}

#[derive(Deserialize)]
struct Envelope {
    result: EnvelopeResult,
}

#[derive(Deserialize)]
struct EnvelopeResult {
    model: String,
    answers: Map<String, Value>,
    #[serde(default)]
    truncated: Option<bool>,
    #[serde(default)]
    state_text: Option<String>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
}

fn decode(text: &TextResponse, context: &DecodeContext<ExtractionRequest>) -> Result<ExtractionResponse, AiError> {
    let identity = identity();
    let envelope: Envelope = identity.decode_json(text)?;
    let result = envelope.result;
    let invalid = |message: String| identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()));
    let mut answers = Map::new();
    for (name, field) in &context.request.fields {
        let Some(answer) = result.answers.get(name) else {
            return Err(invalid(format!("{NAME} returned no answer for {name}")));
        };
        let Some(field_type) = field.get("type").and_then(Value::as_str) else {
            answers.insert(name.clone(), answer.clone());
            continue;
        };
        let decoded = decode_answer(field_type, answer)
            .map_err(|()| invalid(format!("{NAME} returned an invalid answer for {name}")))?;
        answers.insert(name.clone(), decoded);
    }
    let mut providers = Map::new();
    let mut metadata = Map::new();
    metadata.insert("model".to_string(), Value::String(result.model));
    if let Some(truncated) = result.truncated {
        metadata.insert("truncated".to_string(), Value::Bool(truncated));
    }
    if let Some(state_text) = result.state_text {
        metadata.insert("stateText".to_string(), Value::String(state_text));
    }
    providers.insert(PROVIDER.to_string(), Value::Object(metadata));
    let provider_metadata: ProviderMetadata = providers;
    let usage = result.usage.map(|usage| {
        let input = usage.get("input_tokens").and_then(Value::as_u64);
        let output = usage.get("output_tokens").and_then(Value::as_u64);
        Usage {
            context_tokens: None,
            input_tokens: input,
            output_tokens: output,
            non_cached_input_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: crate::protocols::shared::total_tokens(input, output, None),
            cost: None,
            provider_metadata: Some(provider_metadata_for(PROVIDER, Value::Object(usage))),
        }
    });
    Ok(ExtractionResponse { answers, usage, provider_metadata: Some(provider_metadata) })
}

fn provider_metadata_for(key: &str, value: Value) -> ProviderMetadata {
    let mut providers = Map::new();
    providers.insert(key.to_string(), value);
    providers
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn decode_answer(field_type: &str, value: &Value) -> Result<Value, ()> {
    match field_type {
        "text" | "date" | "time" | "datetime" => string_answer(value),
        "number" | "duration" => number_answer(value),
        "list" => list_answer(value),
        "estimate" => estimate_answer(value),
        "forecast" => forecast_answer(value),
        _ => Ok(value.clone()),
    }
}

fn string_answer(raw: &Value) -> Result<Value, ()> {
    let object = raw.as_object().ok_or(())?;
    let mut answer = Map::new();
    let value = object.get("value").ok_or(())?;
    if !(value.is_null() || value.is_string()) {
        return Err(());
    }
    answer.insert("value".to_string(), value.clone());
    if let Some(guess) = object.get("guess") {
        if !guess.is_string() {
            return Err(());
        }
        answer.insert("guess".to_string(), guess.clone());
    }
    add_number(object, "p_null", "nullProbability", &mut answer)?;
    add_number(object, "confidence", "confidence", &mut answer)?;
    if let Some(span) = object.get("span") {
        answer.insert("span".to_string(), span_value(span)?);
    }
    Ok(Value::Object(answer))
}

fn number_answer(raw: &Value) -> Result<Value, ()> {
    let object = raw.as_object().ok_or(())?;
    let mut answer = Map::new();
    let value = object.get("value").ok_or(())?;
    if !(value.is_null() || value.is_number()) {
        return Err(());
    }
    answer.insert("value".to_string(), value.clone());
    if let Some(guess) = object.get("guess") {
        if !guess.is_number() {
            return Err(());
        }
        answer.insert("guess".to_string(), guess.clone());
    }
    add_number(object, "p_null", "nullProbability", &mut answer)?;
    add_number(object, "confidence", "confidence", &mut answer)?;
    if let Some(span) = object.get("span") {
        answer.insert("span".to_string(), span_value(span)?);
    }
    Ok(Value::Object(answer))
}

fn list_answer(raw: &Value) -> Result<Value, ()> {
    let object = raw.as_object().ok_or(())?;
    let mut answer = Map::new();
    let value = object.get("value").ok_or(())?;
    if value.is_null() {
        answer.insert("value".to_string(), Value::Null);
    } else {
        let items = value.as_array().ok_or(())?;
        if !items.iter().all(|item| item.is_string() || item.is_number()) {
            return Err(());
        }
        answer.insert("value".to_string(), Value::Array(items.clone()));
    }
    if let Some(spans) = object.get("spans") {
        let spans = spans.as_array().ok_or(())?;
        let spans = spans.iter().map(span_value).collect::<Result<Vec<_>, ()>>()?;
        answer.insert("spans".to_string(), Value::Array(spans));
    }
    add_number(object, "confidence", "confidence", &mut answer)?;
    Ok(Value::Object(answer))
}

fn estimate_answer(raw: &Value) -> Result<Value, ()> {
    let object = raw.as_object().ok_or(())?;
    let mut answer = Map::new();
    let value = required_number_or_date(object, "value")?;
    answer.insert("value".to_string(), value);
    if let Some(mean) = object.get("mean") {
        if !is_number_or_date(mean) {
            return Err(());
        }
        answer.insert("mean".to_string(), mean.clone());
    }
    answer.insert("median".to_string(), required_number_or_date(object, "median")?);
    answer.insert("p10".to_string(), required_number_or_date(object, "p10")?);
    answer.insert("p90".to_string(), required_number_or_date(object, "p90")?);
    add_number(object, "p_below", "belowProbability", &mut answer)?;
    add_number(object, "p_above", "aboveProbability", &mut answer)?;
    if let Some(quantiles) = object.get("quantiles") {
        let items = quantiles.as_array().ok_or(())?;
        if !items.iter().all(is_number_or_date) {
            return Err(());
        }
        answer.insert("quantiles".to_string(), Value::Array(items.clone()));
    }
    Ok(Value::Object(answer))
}

fn forecast_answer(raw: &Value) -> Result<Value, ()> {
    let object = raw.as_object().ok_or(())?;
    let mut answer = Map::new();
    let value = object.get("value").and_then(Value::as_array).ok_or(())?;
    if !value.iter().all(is_number_or_date) {
        return Err(());
    }
    answer.insert("value".to_string(), Value::Array(value.clone()));
    let steps = object.get("steps").and_then(Value::as_array).ok_or(())?;
    let steps = steps.iter().map(estimate_answer).collect::<Result<Vec<_>, ()>>()?;
    answer.insert("steps".to_string(), Value::Array(steps));
    Ok(Value::Object(answer))
}

fn is_number_or_date(value: &Value) -> bool {
    value.is_number() || value.is_string()
}

fn required_number_or_date(object: &Map<String, Value>, key: &str) -> Result<Value, ()> {
    let value = object.get(key).ok_or(())?;
    if !is_number_or_date(value) {
        return Err(());
    }
    Ok(value.clone())
}

fn add_number(object: &Map<String, Value>, key: &str, name: &str, answer: &mut Map<String, Value>) -> Result<(), ()> {
    let Some(value) = object.get(key) else {
        return Ok(());
    };
    if !value.is_number() {
        return Err(());
    }
    answer.insert(name.to_string(), value.clone());
    Ok(())
}

fn span_value(value: &Value) -> Result<Value, ()> {
    let items = value.as_array().ok_or(())?;
    if items.len() != 2 || !items.iter().all(Value::is_i64) {
        return Err(());
    }
    Ok(Value::Array(items.clone()))
}
