use serde_json::{Map, Value};

use crate::protocols::{openai_responses, shared};
use crate::route::endpoint::EndpointInput;
use crate::route::model::Model;
use crate::schema::{AiError, LlmRequest};

#[derive(Debug)]
pub struct CheckpointRequest {
    pub url: String,
    pub body: Value,
}

pub fn checkpoint_request(request: &LlmRequest, model: &Model) -> Result<CheckpointRequest, AiError> {
    let overlay = request.http.as_ref().and_then(|http| http.body.as_ref());
    if let Some(overlay) = overlay {
        if ["input", "previous_response_id", "conversation"].iter().any(|key| overlay.get(*key).is_some()) {
            return Err(shared::invalid_request(
                "Trigger compaction requires complete canonical history, not an input or continuation override",
            ));
        }
        if overlay.get("stream").is_some_and(|stream| stream != &Value::Bool(true)) {
            return Err(shared::invalid_request("Trigger compaction requires a streamed response"));
        }
    }
    let mut body = openai_responses::from_request(request, model)?;
    let mut input = body.get("input").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut trigger = Map::new();
    trigger.insert("type".to_string(), Value::String("compaction_trigger".to_string()));
    input.push(Value::Object(trigger));
    body["input"] = Value::Array(input);
    let url = model.route.endpoint.render(&EndpointInput { request, body: &body })?;
    Ok(CheckpointRequest { url: url.to_string(), body })
}
