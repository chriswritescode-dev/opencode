use std::collections::BTreeSet;
use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::open_responses::{self, ProviderAdapter};
use crate::protocols::shared;
use crate::protocols::utils::media_type::detect_media_type;
use crate::protocols::utils::responses_hosted_tools::{self, Definition, Definitions};
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{AiError, ContentPart, LlmEvent, Message, ToolContent, ToolResultPart, ToolResultValue};

pub struct MetaResponses;

const ID: &str = "meta-responses";
const NAME: &str = "Meta Responses";

const ADAPTER: ProviderAdapter = ProviderAdapter {
    id: ID,
    name: NAME,
    native_tool: Some(native_tool),
    lower_media: None,
    restore_hosted_tool_item: Some(restore_hosted_tool_item),
};

static DEFINITIONS: LazyLock<Definitions> = LazyLock::new(|| {
    let mut definitions = responses_hosted_tools::common();
    definitions.insert(
        "image_generation_call",
        Definition { name: "image_generation", input: empty_input, result: Some(image_result) },
    );
    definitions
});

struct State {
    state: open_responses::State,
    completed: BTreeSet<String>,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event = open_responses::parse_event(ID, &frame)?;
        let event = open_responses::normalize(&self.state, &event);
        let kind = event.get("type").and_then(Value::as_str);
        let event_item_id = item_id(&event);
        if kind == Some("response.output_item.done")
            && event_item_id.as_ref().is_some_and(|id| self.completed.contains(id))
        {
            return Ok(Vec::new());
        }
        let mut completed = self.completed.clone();
        let mut events = Vec::new();
        if kind == Some("response.completed") {
            let original_output_items = self.state.output_items.clone();
            let output = event
                .get("response")
                .and_then(|response| response.get("output"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for (index, item) in output.iter().enumerate() {
                let mut done = Map::new();
                done.insert("type".to_string(), Value::String("response.output_item.done".to_string()));
                done.insert("item".to_string(), item.clone());
                done.insert("output_index".to_string(), Value::from(index));
                let done = open_responses::normalize(&self.state, &Value::Object(done));
                let Some(done_id) = item_id(&done) else {
                    continue;
                };
                if completed.contains(&done_id) {
                    continue;
                }
                if original_output_items.get(&(index as i64)).is_some_and(|streamed| completed.contains(streamed)) {
                    continue;
                }
                events.extend(on_event(&mut self.state, &done)?);
                completed.insert(done_id);
            }
        }
        events.extend(on_event(&mut self.state, &event)?);
        if kind == Some("response.output_item.done")
            && let Some(id) = event_item_id
        {
            completed.insert(id);
        }
        self.completed = completed;
        Ok(events)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        open_responses::parse_event(ID, frame).map(|event| open_responses::terminal(&event)).unwrap_or(false)
    }
}

impl Protocol for MetaResponses {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
        let mut rewritten = request.request.clone();
        for message in rewritten.messages.iter_mut() {
            let Message::Assistant(message) = message else {
                continue;
            };
            for part in message.content.iter_mut() {
                let ContentPart::ToolResult(part) = part else {
                    continue;
                };
                rewrite_image_result(part, &key);
            }
        }
        let body = open_responses::from_request_with_adapter(&rewritten, model, &ADAPTER, None)?;
        Ok(relocate_videos(body))
    }

    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        Some(open_responses::PATH.to_string())
    }

    fn framing(&self) -> crate::route::framing::Definition {
        crate::route::framing::sse()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(State { state: open_responses::initial(request, context, &ADAPTER), completed: BTreeSet::new() })
    }
}

fn rewrite_image_result(part: &mut ToolResultPart, key: &str) {
    if part.provider_executed != Some(true) || part.name != "image_generation" {
        return;
    }
    if !matches!(part.result, ToolResultValue::Content { .. }) {
        return;
    }
    let item_id = part
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key))
        .and_then(|metadata| metadata.get("itemId"))
        .and_then(Value::as_str);
    if item_id != Some(part.id.as_str()) {
        return;
    }
    let mut value = Map::new();
    value.insert("type".to_string(), Value::String("image_generation_call".to_string()));
    value.insert("id".to_string(), Value::String(part.id.clone()));
    value.insert("status".to_string(), Value::String("completed".to_string()));
    value.insert("result".to_string(), Value::Null);
    part.result = ToolResultValue::Json { value: Value::Object(value) };
}

fn relocate_videos(body: Value) -> Value {
    let mut body = body;
    let Some(object) = body.as_object_mut() else {
        return body;
    };
    let Some(input) = object.get_mut("input").map(Value::take) else {
        return body;
    };
    let Value::Array(items) = input else {
        object.insert("input".to_string(), input);
        return body;
    };
    let mut relocated = Vec::with_capacity(items.len());
    let mut videos = Vec::new();
    for mut item in items {
        let is_output = matches!(
            item.get("type").and_then(Value::as_str),
            Some("function_call_output" | "custom_tool_call_output")
        );
        if !is_output {
            flush_videos(&mut videos, &mut relocated);
            relocated.push(item);
            continue;
        }
        if item.get("output").is_some_and(Value::is_string) {
            relocated.push(item);
            continue;
        }
        let Some(output) = item.get_mut("output").map(Value::take) else {
            relocated.push(item);
            continue;
        };
        let Value::Array(parts) = output else {
            if let Some(object) = item.as_object_mut() {
                object.insert("output".to_string(), output);
            }
            relocated.push(item);
            continue;
        };
        let length = parts.len();
        let mut content = Vec::new();
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("input_video") {
                videos.push(part);
            } else {
                content.push(part);
            }
        }
        let output = if content.is_empty() && length > 0 {
            Value::String("Video attached in the following user message.".to_string())
        } else {
            Value::Array(content)
        };
        if let Some(object) = item.as_object_mut() {
            object.insert("output".to_string(), output);
        }
        relocated.push(item);
    }
    flush_videos(&mut videos, &mut relocated);
    object.insert("input".to_string(), Value::Array(relocated));
    body
}

fn flush_videos(videos: &mut Vec<Value>, input: &mut Vec<Value>) {
    if videos.is_empty() {
        return;
    }
    let mut message = Map::new();
    message.insert("type".to_string(), Value::String("message".to_string()));
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert("content".to_string(), Value::Array(std::mem::take(videos)));
    input.push(Value::Object(message));
}

fn native_tool(native: &Map<String, Value>) -> Result<Value, AiError> {
    let meta = native.get("meta").and_then(Value::as_object).ok_or_else(native_error)?;
    match meta.get("type").and_then(Value::as_str) {
        Some("web_search") => lower_web_search(meta),
        Some("image_generation") => lower_image_generation(meta),
        _ => Err(native_error()),
    }
}

fn lower_web_search(meta: &Map<String, Value>) -> Result<Value, AiError> {
    let search = decode_web_search(meta)?;
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("web_search".to_string()));
    if let Some(size) = search.search_context_size {
        object.insert("search_context_size".to_string(), Value::String(size));
    }
    if let Some(location) = search.user_location {
        object.insert("user_location".to_string(), location);
    }
    Ok(Value::Object(object))
}

fn lower_image_generation(meta: &Map<String, Value>) -> Result<Value, AiError> {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("image_generation".to_string()));
    for key in ["size", "output_format", "reasoning_strength"] {
        if let Some(value) = optional_string(meta.get(key))? {
            object.insert(key.to_string(), Value::String(value));
        }
    }
    for key in ["enable_image_search", "enable_web_search", "enable_shell"] {
        match meta.get(key) {
            None => {}
            Some(Value::Bool(value)) => {
                object.insert(key.to_string(), Value::Bool(*value));
            }
            Some(_) => return Err(native_error()),
        }
    }
    Ok(Value::Object(object))
}

pub(crate) struct WebSearch {
    pub search_context_size: Option<String>,
    pub user_location: Option<Value>,
}

pub(crate) fn decode_web_search(meta: &Map<String, Value>) -> Result<WebSearch, AiError> {
    if meta.get("type").and_then(Value::as_str) != Some("web_search") {
        return Err(shared::invalid_request("Meta native web search tool is invalid"));
    }
    Ok(WebSearch {
        search_context_size: optional_string(meta.get("search_context_size"))?,
        user_location: optional_user_location(meta.get("user_location"))?,
    })
}

fn optional_user_location(value: Option<&Value>) -> Result<Option<Value>, AiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(native_error)?;
    if object.get("type").and_then(Value::as_str) != Some("approximate") {
        return Err(native_error());
    }
    let mut location = Map::new();
    location.insert("type".to_string(), Value::String("approximate".to_string()));
    for key in ["city", "region", "country", "timezone"] {
        if let Some(value) = optional_string(object.get(key))? {
            location.insert(key.to_string(), Value::String(value));
        }
    }
    Ok(Some(Value::Object(location)))
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(native_error()),
    }
}

fn native_error() -> AiError {
    shared::invalid_request("Meta Responses native tool is invalid")
}

fn restore_hosted_tool_item(value: &Value) -> Option<Value> {
    image_item(value).map(|_| value.clone())
}

fn image_item(value: &Value) -> Option<&Map<String, Value>> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("image_generation_call") {
        return None;
    }
    object.get("id")?.as_str()?;
    if !shared::optional_string(object, "status") || !shared::optional_string(object, "output_format") {
        return None;
    }
    if !shared::optional_null_string(object, "result") {
        return None;
    }
    Some(object)
}

fn empty_input(_item: &Value) -> Value {
    Value::Object(Map::new())
}

fn image_result(item: &Value) -> Result<ToolResultValue, AiError> {
    let Some(object) = image_item(item) else {
        return Err(shared::event_error(
            ID,
            "Meta returned an invalid image item",
            Some(crate::json::encode_body(item)),
        ));
    };
    if let Some(error) = object.get("error").filter(|error| !error.is_null()) {
        return Ok(ToolResultValue::Error { value: error.clone() });
    }
    let Some(result) = object.get("result").and_then(Value::as_str).filter(|result| !result.is_empty()) else {
        return Err(shared::event_error(
            ID,
            "Meta returned an image without data",
            Some(crate::json::encode_body(item)),
        ));
    };
    let Ok(data) = BASE64_STANDARD.decode(result) else {
        return Err(shared::event_error(
            ID,
            "Meta returned invalid image base64",
            Some(crate::json::encode_body(item)),
        ));
    };
    let mime = match object.get("output_format").and_then(Value::as_str) {
        Some(format) => format!("image/{format}"),
        None => detect_media_type(&data).unwrap_or("application/octet-stream").to_string(),
    };
    Ok(ToolResultValue::Content {
        value: vec![ToolContent::File { uri: format!("data:{mime};base64,{result}"), mime, name: None }],
    })
}

fn item_id(event: &Value) -> Option<String> {
    event.get("item")?.get("id")?.as_str().map(str::to_string)
}

fn on_event(state: &mut open_responses::State, event: &Value) -> Result<Vec<LlmEvent>, AiError> {
    let kind = event.get("type").and_then(Value::as_str);
    let item = event.get("item").filter(|item| item.is_object());
    if kind == Some("response.output_item.done")
        && let Some(item) = item
        && item.get("type").and_then(Value::as_str).is_some_and(|kind| DEFINITIONS.contains_key(kind))
    {
        return responses_hosted_tools::on_done(state, item, &DEFINITIONS);
    }
    let mut events = open_responses::step(state, event)?;
    let Some(item) = item else {
        return Ok(events);
    };
    if kind != Some("response.output_item.done") || item.get("type").and_then(Value::as_str) != Some("message") {
        return Ok(events);
    }
    let annotations = message_annotations(event, item)?;
    if annotations.is_empty() {
        return Ok(events);
    }
    let key = state.provider_metadata_key.clone();
    for event in events.iter_mut() {
        let LlmEvent::TextEnd { provider_metadata, .. } = event else {
            continue;
        };
        let metadata = provider_metadata.get_or_insert_with(Map::new);
        let entry = metadata.entry(key.clone()).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Some(object) = entry.as_object_mut() {
            object.insert("annotations".to_string(), Value::Array(annotations.clone()));
        }
    }
    Ok(events)
}

fn message_annotations(event: &Value, item: &Value) -> Result<Vec<Value>, AiError> {
    let invalid =
        || shared::event_error(ID, "Meta returned invalid message annotations", Some(crate::json::encode_body(event)));
    let content = item.get("content").and_then(Value::as_array).ok_or_else(invalid)?;
    let mut annotations = Vec::new();
    for part in content {
        let part = part.as_object().ok_or_else(invalid)?;
        match part.get("annotations") {
            None => {}
            Some(Value::Array(values)) => {
                for value in values {
                    if !value.is_object() {
                        return Err(invalid());
                    }
                    annotations.push(value.clone());
                }
            }
            Some(_) => return Err(invalid()),
        }
    }
    Ok(annotations)
}
