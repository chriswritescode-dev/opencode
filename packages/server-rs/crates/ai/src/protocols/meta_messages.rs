use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::anthropic_messages::{self, AnthropicMessagesPlain};
use crate::protocols::meta_responses::decode_web_search;
use crate::protocols::shared;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::AiError;

pub struct MetaMessages;

const ID: &str = "meta-messages";

impl Protocol for MetaMessages {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let mut body = anthropic_messages::from_request(&request.request, model)?;
        let definitions = shared::flatten_tools(&request.request.tools);
        relocate_videos(&mut body);
        if let Some(object) = body.as_object_mut()
            && let Some(Value::Array(tools)) = object.get_mut("tools")
        {
            for (index, tool) in tools.iter_mut().enumerate() {
                let Some(native) = definitions.get(index).and_then(|tool| tool.native.as_ref()) else {
                    continue;
                };
                let meta = native.get("meta").and_then(Value::as_object).ok_or_else(native_error)?;
                let search = decode_web_search(meta)?;
                if search.search_context_size.is_some() {
                    return Err(shared::invalid_request("Meta Messages does not support searchContextSize"));
                }
                let mut replacement = Map::new();
                replacement.insert("type".to_string(), Value::String("web_search".to_string()));
                replacement.insert("name".to_string(), Value::String("web_search".to_string()));
                if let Some(location) = search.user_location {
                    replacement.insert("user_location".to_string(), location);
                }
                *tool = Value::Object(replacement);
            }
        }
        Ok(body)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        AnthropicMessagesPlain::default().default_path(model, body)
    }

    fn framing(&self) -> Definition {
        AnthropicMessagesPlain::default().framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        AnthropicMessagesPlain::default().new_state(request, context)
    }
}

fn relocate_videos(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        if object.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(Value::Array(content)) = object.get_mut("content") else {
            continue;
        };
        let mut videos = Vec::new();
        for block in content.iter_mut() {
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(Value::Array(items)) = block.get_mut("content") else {
                continue;
            };
            let mut kept = Vec::new();
            for item in std::mem::take(items) {
                if item.get("type").and_then(Value::as_str) == Some("video") {
                    videos.push(item);
                } else {
                    kept.push(item);
                }
            }
            *items = kept;
        }
        if !videos.is_empty() {
            content.extend(videos);
        }
    }
}

fn native_error() -> AiError {
    shared::invalid_request("Meta Messages native tool is invalid")
}
