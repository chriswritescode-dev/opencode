use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::open_responses::{self, ProviderAdapter};
use crate::protocols::shared;
use crate::protocols::utils::responses_hosted_tools::{self, Definition, Definitions};
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{AiError, LlmEvent, LlmRequest, MediaPart};

pub struct XaiResponses;

const ID: &str = "xai-responses";
const NAME: &str = "xAI Responses";

const ADAPTER: ProviderAdapter = ProviderAdapter {
    id: ID,
    name: NAME,
    native_tool: None,
    lower_media: Some(lower_media),
    restore_hosted_tool_item: Some(restore_hosted_tool_item),
};

static DEFINITIONS: LazyLock<Definitions> = LazyLock::new(|| {
    let mut definitions = responses_hosted_tools::common();
    definitions.insert(
        "x_search_call",
        Definition { name: "x_search", input: responses_hosted_tools::action_input, result: None },
    );
    definitions
        .insert("image_generation_call", Definition { name: "image_generation", input: empty_input, result: None });
    definitions
});

struct State {
    state: open_responses::State,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = open_responses::parse_event(ID, &frame)?;
        let event = open_responses::normalize(&self.state, &event);
        if event.get("type").and_then(Value::as_str) == Some("response.output_item.done")
            && let Some(item) = event.get("item").filter(|item| item.is_object())
            && item.get("type").and_then(Value::as_str).is_some_and(|kind| DEFINITIONS.contains_key(kind))
        {
            return responses_hosted_tools::on_done(&mut self.state, item, &DEFINITIONS);
        }
        open_responses::step(&mut self.state, &event)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        open_responses::parse_event(ID, frame).map(|event| open_responses::terminal(&event)).unwrap_or(false)
    }
}

impl Protocol for XaiResponses {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        if request.request.provider_options.as_ref().is_some_and(|options| options.contains_key("contextManagement")) {
            return Err(shared::unsupported_operation(
                "in-band-compaction",
                "xAI requires explicit compaction through ai.compact; automatic context management is not supported",
                Some(model.provider.clone()),
                Some(model.route.id.clone()),
            ));
        }
        let min_p = decode_options(request.request.provider_options.as_ref())?;
        let mut body = open_responses::from_request_with_adapter(&request.request, model, &ADAPTER, None)?;
        let Some(object) = body.as_object_mut() else {
            return Ok(body);
        };
        object.remove("presence_penalty");
        object.remove("frequency_penalty");
        if let Some(top_k) = request.request.generation.top_k.clone() {
            object.insert("top_k".to_string(), Value::Number(top_k));
        }
        if let Some(min_p) = min_p {
            object.insert("min_p".to_string(), Value::Number(min_p));
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

fn lower_media(part: &MediaPart, _request: &LlmRequest) -> Option<Value> {
    if shared::media_kind(&shared::media_type(&part.media)) != "video" {
        return None;
    }
    let filename = part.filename.clone().unwrap_or_else(|| "file".to_string());
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("input_file".to_string()));
    object.insert("filename".to_string(), Value::String(filename));
    if let Some(url) = shared::media_url(&part.media) {
        object.insert("file_url".to_string(), Value::String(url.to_string()));
        return Some(Value::Object(object));
    }
    let inline = shared::inline_media(&part.media)?;
    object.insert("file_data".to_string(), Value::String(inline.data_url));
    Some(Value::Object(object))
}

fn restore_hosted_tool_item(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let kind = object.get("type").and_then(Value::as_str)?;
    object.get("id")?.as_str()?;
    if !optional_string(object, "status") {
        return None;
    }
    match kind {
        "x_search_call" => {
            if object.get("action").is_some_and(|action| !action.is_null() && !action.is_object()) {
                return None;
            }
        }
        "image_generation_call" => {}
        _ => return None,
    }
    Some(value.clone())
}

fn optional_string(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).is_none_or(Value::is_string)
}

fn empty_input(_item: &Value) -> Value {
    Value::Object(Map::new())
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Option<serde_json::Number>, AiError> {
    match input.and_then(|options| options.get("minP")) {
        None => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value.clone())),
        Some(_) => Err(shared::invalid_request("xai-responses providerOptions minP is invalid")),
    }
}
