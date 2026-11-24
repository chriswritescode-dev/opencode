use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::protocols::anthropic_messages::{self, AnthropicMessagesPlain};
use crate::protocols::shared::{fit_thinking_budget, invalid_request};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub struct AlibabaMessages;

const ID: &str = "alibaba-messages";

const FOLLOWING_FIELDS: [&str; 8] = [
    "output_config",
    "cache_control",
    "container",
    "inference_geo",
    "metadata",
    "service_tier",
    "speed",
    "context_management",
];

impl Protocol for AlibabaMessages {
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
        let Some(thinking) = thinking else {
            return Ok(body);
        };
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        let max_tokens = object.get("max_tokens").and_then(Value::as_number);
        let mut value = Map::new();
        value.insert("type".to_string(), Value::String(thinking.kind));
        if let Some(budget) = thinking.budget {
            value.insert("budget_tokens".to_string(), Value::Number(fit_thinking_budget(&budget, max_tokens, 1)));
        }
        insert_thinking(object, Value::Object(value));
        Ok(body)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        AnthropicMessagesPlain::default().default_path(model, body)
    }

    fn framing(&self) -> Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        AnthropicMessagesPlain::default().new_state(request, context)
    }
}

struct Thinking {
    kind: String,
    budget: Option<Number>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Option<Thinking>, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("effort"), "effort")?;
    optional_thinking(input.get("thinking"))
}

fn optional_thinking(value: Option<&Value>) -> Result<Option<Thinking>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| option_error("thinking"))?;
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(option_error("thinking.type"));
    };
    let budget_tokens = optional_int(object.get("budgetTokens"), "thinking.budgetTokens")?;
    let budget_tokens = match budget_tokens {
        Some(budget) => Some(budget),
        None => optional_int(object.get("budget_tokens"), "thinking.budget_tokens")?,
    };
    Ok(Some(Thinking { kind: kind.clone(), budget: budget_tokens }))
}

fn insert_thinking(object: &mut Map<String, Value>, thinking: Value) {
    let mut rebuilt = Map::new();
    let mut pending = Some(thinking);
    for (key, value) in std::mem::take(object) {
        if FOLLOWING_FIELDS.contains(&key.as_str())
            && let Some(thinking) = pending.take()
        {
            rebuilt.insert("thinking".to_string(), thinking);
        }
        rebuilt.insert(key, value);
    }
    if let Some(thinking) = pending {
        rebuilt.insert("thinking".to_string(), thinking);
    }
    *object = rebuilt;
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_int(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(value) => match value.as_number() {
            Some(number) if number.is_i64() || number.is_u64() => Ok(Some(number.clone())),
            _ => Err(option_error(field)),
        },
    }
}

fn option_error(field: &str) -> AiError {
    invalid_request(format!("alibaba-messages providerOptions {field} is invalid"))
}
