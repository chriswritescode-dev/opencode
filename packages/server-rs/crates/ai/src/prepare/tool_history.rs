use serde_json::Value;

use crate::schema::{ContentPart, Message, ToolCallPart, ToolContent, ToolMessage, ToolResultPart, ToolResultValue};

const EMPTY_TOOL_OUTPUT: &str = "(no tool output)";
const MISSING_TOOL_RESULT: &str = "Tool result missing";

pub fn normalize_tool_history(messages: Vec<Message>) -> Vec<Message> {
    let mut normalized: Vec<Message> = Vec::with_capacity(messages.len());
    let mut pending: Vec<(String, ToolCallPart)> = Vec::new();
    let mut held: Vec<Message> = Vec::new();

    for message in messages {
        if matches!(message, Message::User(_) | Message::Assistant(_)) {
            append_missing_results(&mut normalized, &mut pending, &mut held);
        }

        if let Message::Tool(tool) = &message {
            if let Some(tool) = normalize_tool_message(tool, &mut pending) {
                normalized.push(Message::Tool(tool));
            }
            release_held(&pending, &mut normalized, &mut held);
            continue;
        }

        if matches!(message, Message::System(_)) && !pending.is_empty() {
            held.push(message);
            continue;
        }

        if let Message::Assistant(assistant) = &message {
            for part in &assistant.content {
                if let ContentPart::ToolCall(call) = part
                    && call.provider_executed != Some(true)
                {
                    set_pending(&mut pending, call.id.clone(), call.clone());
                }
            }
        }

        normalized.push(message);
    }

    append_missing_results(&mut normalized, &mut pending, &mut held);
    normalized
}

fn set_pending(pending: &mut Vec<(String, ToolCallPart)>, id: String, call: ToolCallPart) {
    match pending.iter_mut().find(|(existing, _)| existing == &id) {
        Some(entry) => entry.1 = call,
        None => pending.push((id, call)),
    }
}

fn release_held(pending: &[(String, ToolCallPart)], normalized: &mut Vec<Message>, held: &mut Vec<Message>) {
    if !pending.is_empty() {
        return;
    }
    normalized.append(held);
}

fn append_missing_results(
    normalized: &mut Vec<Message>,
    pending: &mut Vec<(String, ToolCallPart)>,
    held: &mut Vec<Message>,
) {
    if !pending.is_empty() {
        let content = pending
            .iter()
            .map(|(_, call)| {
                ContentPart::ToolResult(ToolResultPart {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    namespace: call.namespace.clone(),
                    result: ToolResultValue::Error { value: Value::String(MISSING_TOOL_RESULT.to_string()) },
                    provider_executed: None,
                    cache: None,
                    provider_metadata: None,
                })
            })
            .collect();
        normalized.push(Message::Tool(ToolMessage { id: None, content }));
    }
    pending.clear();
    release_held(pending, normalized, held);
}

fn normalize_tool_message(message: &ToolMessage, pending: &mut Vec<(String, ToolCallPart)>) -> Option<ToolMessage> {
    let content = message
        .content
        .iter()
        .map(|part| {
            let ContentPart::ToolResult(result) = part else {
                return part.clone();
            };
            if result.provider_executed == Some(true) {
                return part.clone();
            }
            let position = pending.iter().position(|(id, _)| id == &result.id);
            let call = position.map(|position| pending.remove(position).1);
            ContentPart::ToolResult(normalize_tool_result(result, call.as_ref()))
        })
        .collect::<Vec<_>>();
    if content.is_empty() {
        return None;
    }
    Some(ToolMessage { id: message.id.clone(), content })
}

fn normalize_tool_result(part: &ToolResultPart, call: Option<&ToolCallPart>) -> ToolResultPart {
    let mut named = part.clone();
    if let Some(call) = call
        && (part.name != call.name || part.namespace != call.namespace)
    {
        named.name = call.name.clone();
        named.namespace = call.namespace.clone();
    }
    match &named.result {
        ToolResultValue::Text { value } if value.as_str() == Some("") => {
            named.result = ToolResultValue::Text { value: Value::String(EMPTY_TOOL_OUTPUT.to_string()) };
        }
        ToolResultValue::Error { value } if value.as_str() == Some("") => {
            named.result = ToolResultValue::Error { value: Value::String(EMPTY_TOOL_OUTPUT.to_string()) };
        }
        ToolResultValue::Content { value } => {
            let filtered = value
                .iter()
                .filter(|item| !matches!(item, ToolContent::Text { text } if text.is_empty()))
                .cloned()
                .collect::<Vec<_>>();
            if filtered.is_empty() {
                named.result = ToolResultValue::Text { value: Value::String(EMPTY_TOOL_OUTPUT.to_string()) };
            } else if filtered.len() != value.len() {
                named.result = ToolResultValue::Content { value: filtered };
            }
        }
        _ => {}
    }
    named
}
