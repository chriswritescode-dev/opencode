use crate::route::model::Model;
use crate::schema::{ContentPart, EffortPart, LlmRequest, Message};

pub fn effort_update(message: &Message) -> Option<&EffortPart> {
    let Message::System(message) = message else {
        return None;
    };
    if message.content.len() != 1 {
        return None;
    }
    match message.content.first() {
        Some(ContentPart::Effort(part)) => Some(part),
        _ => None,
    }
}

pub fn strip_effort_updates(request: LlmRequest) -> LlmRequest {
    let mut request = request;
    request.messages.retain(|message| effort_update(message).is_none());
    request
}

pub fn apply_effort_updates(model: &Model, request: LlmRequest) -> LlmRequest {
    if model.route.protocol.supports_effort_updates(model, &request) {
        return request;
    }
    strip_effort_updates(request)
}

pub fn resolve_effort_updates(request: &LlmRequest, current: Option<&str>) -> (LlmRequest, Option<String>) {
    let updates = request.messages.iter().filter_map(effort_update).collect::<Vec<_>>();
    if updates.is_empty() {
        return (request.clone(), current.map(str::to_string));
    }
    if updates.last().and_then(|update| update.effort.as_deref()) != current {
        return (strip_effort_updates(request.clone()), current.map(str::to_string));
    }
    (request.clone(), updates.first().and_then(|update| update.previous.clone()))
}
