use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::anthropic_messages::{self, AnthropicMessages};
use crate::protocols::shared::invalid_request;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub struct ZaiMessages;

const ID: &str = "zai-messages";

impl Protocol for ZaiMessages {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let thinking = decode_options(request.request.provider_options.as_ref())?;
        let mut request = request.request.clone();
        if let Some(options) = request.provider_options.as_mut() {
            options.remove("thinking");
        }
        let mut body = anthropic_messages::from_request(&request, model)?;
        if let Some(thinking) = thinking
            && let Some(object) = body.as_object_mut()
        {
            object.insert("thinking".to_string(), Value::Object(thinking));
        }
        Ok(body)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        AnthropicMessages::default().default_path(model, body)
    }

    fn framing(&self) -> Definition {
        AnthropicMessages::default().framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        AnthropicMessages::default().new_state(request, context)
    }
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Option<Map<String, Value>>, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("effort"), "effort")?;
    optional_thinking(input.get("thinking"))
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Map<String, Value>>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(option_error("thinking.type"));
    };
    Ok(Some(Map::from_iter([("type".to_string(), Value::String(kind.clone()))])))
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("zai-messages providerOptions {field} is invalid"))
}
