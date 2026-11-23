use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::open_responses::{self, ProviderAdapter};
use crate::protocols::shared;
use crate::protocols::utils::responses_hosted_tools::{self, Definition, Definitions};
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{AiError, LlmEvent};

pub struct AlibabaResponses;

const ID: &str = "alibaba-responses";
const NAME: &str = "Alibaba Responses";

const ADAPTER: ProviderAdapter = ProviderAdapter {
    id: ID,
    name: NAME,
    native_tool: Some(native_tool),
    lower_media: None,
    restore_hosted_tool_item: Some(restore_hosted_tool_item),
};

static DEFINITIONS: LazyLock<Definitions> = LazyLock::new(|| {
    let mut definitions = Definitions::new();
    definitions.insert(
        "web_search_call",
        Definition { name: "web_search", input: responses_hosted_tools::action_input, result: None },
    );
    definitions.insert(
        "code_interpreter_call",
        Definition { name: "code_interpreter", input: code_interpreter_input, result: None },
    );
    definitions
        .insert("web_extractor_call", Definition { name: "web_extractor", input: web_extractor_input, result: None });
    definitions
});

struct State {
    state: open_responses::State,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = open_responses::parse_event(ID, &frame)?;
        let event = open_responses::normalize(&self.state, &event);
        let kind = event.get("type").and_then(Value::as_str);
        if kind == Some("response.output_item.done")
            && let Some(item) = event.get("item").filter(|item| item.is_object())
        {
            if item.get("type").and_then(Value::as_str) == Some("web_extractor_call") {
                let Some(restored) = restore_hosted_tool_item(item) else {
                    return Err(shared::event_error(
                        ID,
                        "Alibaba returned an invalid web extraction item",
                        Some(crate::json::encode_body(&event)),
                    ));
                };
                return responses_hosted_tools::on_done(&mut self.state, &restored, &DEFINITIONS);
            }
            if item.get("type").and_then(Value::as_str).is_some_and(|kind| DEFINITIONS.contains_key(kind)) {
                return responses_hosted_tools::on_done(&mut self.state, item, &DEFINITIONS);
            }
        }
        open_responses::step(&mut self.state, &event)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        open_responses::parse_event(ID, frame).map(|event| open_responses::terminal(&event)).unwrap_or(false)
    }
}

impl Protocol for AlibabaResponses {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let options = decode_options(request.request.provider_options.as_ref())?;
        let mut body = open_responses::from_request_with_adapter(&request.request, model, &ADAPTER, None)?;
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        if let Some(enable_thinking) = options.enable_thinking {
            object.insert("enable_thinking".to_string(), Value::Bool(enable_thinking));
        }
        if let Some(previous_response_id) = options.previous_response_id {
            object.insert("previous_response_id".to_string(), Value::String(previous_response_id));
        }
        if let Some(conversation) = options.conversation {
            object.insert("conversation".to_string(), Value::String(conversation));
        }
        if let Some(choice) = object
            .get("tool_choice")
            .filter(|choice| choice.get("type").and_then(Value::as_str) == Some("function"))
            .cloned()
        {
            let mut allowed = Map::new();
            allowed.insert("type".to_string(), Value::String("allowed_tools".to_string()));
            allowed.insert("mode".to_string(), Value::String("required".to_string()));
            allowed.insert("tools".to_string(), Value::Array(vec![choice]));
            object.insert("tool_choice".to_string(), Value::Object(allowed));
        }
        Ok(body)
    }

    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        Some(open_responses::PATH.to_string())
    }

    fn framing(&self) -> crate::route::framing::Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(State { state: open_responses::initial(request, context, &ADAPTER) })
    }
}

fn native_tool(native: &Map<String, Value>) -> Result<Value, AiError> {
    let kind = native.get("alibaba").and_then(|value| value.get("type")).and_then(Value::as_str);
    match kind {
        Some(kind @ ("web_search" | "web_extractor" | "code_interpreter")) => {
            let mut object = Map::new();
            object.insert("type".to_string(), Value::String(kind.to_string()));
            Ok(Value::Object(object))
        }
        _ => Err(shared::invalid_request(format!("{NAME} native tool is invalid"))),
    }
}

fn restore_hosted_tool_item(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("web_extractor_call") {
        return None;
    }
    object.get("id")?.as_str()?;
    if let Some(urls) = object.get("urls") {
        let urls = urls.as_array()?;
        if !urls.iter().all(Value::is_string) {
            return None;
        }
    }
    if let Some(goal) = object.get("goal")
        && !goal.is_string()
    {
        return None;
    }
    Some(value.clone())
}

fn code_interpreter_input(item: &Value) -> Value {
    let mut object = Map::new();
    if let Some(code) = item.get("code").filter(|code| !code.is_null()) {
        object.insert("code".to_string(), code.clone());
    }
    Value::Object(object)
}

fn web_extractor_input(item: &Value) -> Value {
    let mut object = Map::new();
    if let Some(urls) = item.get("urls").filter(|urls| !urls.is_null()) {
        object.insert("urls".to_string(), urls.clone());
    }
    if let Some(goal) = item.get("goal").filter(|goal| !goal.is_null()) {
        object.insert("goal".to_string(), goal.clone());
    }
    Value::Object(object)
}

struct Options {
    enable_thinking: Option<bool>,
    previous_response_id: Option<String>,
    conversation: Option<String>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    optional_string(input.get("reasoningEffort"), "reasoningEffort")?;
    optional_bool(input.get("store"), "store")?;
    Ok(Options {
        enable_thinking: optional_bool(input.get("enableThinking"), "enableThinking")?,
        previous_response_id: optional_string(input.get("previousResponseId"), "previousResponseId")?,
        conversation: optional_string(input.get("conversation"), "conversation")?,
    })
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_bool(value: Option<&Value>, field: &str) -> Result<Option<bool>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(option_error(field)),
    }
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("alibaba-responses providerOptions {field} is invalid"))
}
