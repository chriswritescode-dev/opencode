use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use http::StatusCode;
use opencode_contract::codec::{encode_json, write_value};
use opencode_contract::generated::operations::session_message_list;
use opencode_contract::generated::requests::{SessionMessagesQueryOrder, SessionMessagesQueryType};
use opencode_contract::generated::{
    InvalidCursorError, InvalidCursorErrorTag, SessionMessageCompaction, SessionMessageInfo, SessionMessagesResponse,
    SessionMessagesResponseCursor,
};
use opencode_contract::request::RawRequest;
use opencode_session::{Cursor, Direction, MessagesInput, Order};
use serde_json::Value;

use crate::handlers::{defect, invalid, session};
use crate::response;
use crate::services::GlobalServices;

pub fn list(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_message_list::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let query = match session_message_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    if query.cursor.is_some() && query.order.is_some() {
        return invalid_cursor("Cursor cannot be combined with order");
    }
    let cursor = match query.cursor.as_ref().and_then(|cursor| cursor.as_ref()) {
        Some(raw_cursor) => match decode_cursor(raw_cursor) {
            Some(cursor) => Some(cursor),
            None => return invalid_cursor("Invalid cursor"),
        },
        None => None,
    };
    let order = match &cursor {
        Some(cursor) => cursor.order,
        None => query.order.flatten().map(order_of).unwrap_or(Order::Desc),
    };
    let limit = query.limit.flatten().unwrap_or(50.0) as i64;
    let r#type = query.r#type.flatten().map(type_name);
    if let Err(error) = sessions.get(&params.session_id.0) {
        return session::error_response(error);
    }
    let input = MessagesInput {
        session_id: params.session_id.0,
        limit: Some(limit),
        order: Some(order),
        r#type: r#type.map(str::to_owned),
        cursor: cursor.map(|cursor| Cursor { id: cursor.id, direction: cursor.direction }),
    };
    let data = match sessions.store().messages(&input) {
        Ok(data) => data,
        Err(error) => return defect::response("failed to list session messages", error),
    };
    let previous = data.first().map(|message| Some(encode_cursor(message_id(message), order, Direction::Previous)));
    let next = data.last().map(|message| Some(encode_cursor(message_id(message), order, Direction::Next)));
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&SessionMessagesResponse { data, cursor: SessionMessagesResponseCursor { previous, next } }),
    )
}

struct MessageCursor {
    id: String,
    order: Order,
    direction: Direction,
}

fn decode_cursor(raw: &str) -> Option<MessageCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(raw).or_else(|_| URL_SAFE.decode(raw)).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    if !id.starts_with("msg_") {
        return None;
    }
    let order = match object.get("order")?.as_str()? {
        "asc" => Order::Asc,
        "desc" => Order::Desc,
        _ => return None,
    };
    let direction = match object.get("direction")?.as_str()? {
        "previous" => Direction::Previous,
        "next" => Direction::Next,
        _ => return None,
    };
    Some(MessageCursor { id: id.to_owned(), order, direction })
}

fn encode_cursor(id: &str, order: Order, direction: Direction) -> String {
    let mut out = String::from("{");
    let mut first = true;
    write_value(&mut out, &mut first, "id", &id.to_owned());
    write_value(&mut out, &mut first, "order", &order_wire(order).to_owned());
    write_value(&mut out, &mut first, "direction", &direction_wire(direction).to_owned());
    out.push('}');
    URL_SAFE_NO_PAD.encode(out)
}

fn invalid_cursor(message: &str) -> response::Response {
    response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidCursorError {
            tag: InvalidCursorErrorTag::InvalidCursorError,
            message: message.to_owned(),
        }),
    )
}

fn order_of(order: SessionMessagesQueryOrder) -> Order {
    match order {
        SessionMessagesQueryOrder::Asc => Order::Asc,
        SessionMessagesQueryOrder::Desc => Order::Desc,
    }
}

fn order_wire(order: Order) -> &'static str {
    match order {
        Order::Asc => "asc",
        Order::Desc => "desc",
    }
}

fn direction_wire(direction: Direction) -> &'static str {
    match direction {
        Direction::Previous => "previous",
        Direction::Next => "next",
    }
}

fn type_name(r#type: SessionMessagesQueryType) -> &'static str {
    match r#type {
        SessionMessagesQueryType::AgentSwitched => "agent-switched",
        SessionMessagesQueryType::ModelSwitched => "model-switched",
        SessionMessagesQueryType::LocationSwitched => "location-switched",
        SessionMessagesQueryType::User => "user",
        SessionMessagesQueryType::Synthetic => "synthetic",
        SessionMessagesQueryType::System => "system",
        SessionMessagesQueryType::Skill => "skill",
        SessionMessagesQueryType::Shell => "shell",
        SessionMessagesQueryType::Assistant => "assistant",
        SessionMessagesQueryType::Compaction => "compaction",
        SessionMessagesQueryType::Idle => "idle",
    }
}

fn message_id(message: &SessionMessageInfo) -> &str {
    match message {
        SessionMessageInfo::AgentSelected(message) => &message.id.0,
        SessionMessageInfo::ModelSelected(message) => &message.id.0,
        SessionMessageInfo::LocationSwitched(message) => &message.id.0,
        SessionMessageInfo::User(message) => &message.id.0,
        SessionMessageInfo::Synthetic(message) => &message.id.0,
        SessionMessageInfo::System(message) => &message.id.0,
        SessionMessageInfo::Skill(message) => &message.id.0,
        SessionMessageInfo::Shell(message) => &message.id.0,
        SessionMessageInfo::Assistant(message) => &message.id.0,
        SessionMessageInfo::Compaction(message) => match message {
            SessionMessageCompaction::Running(message) => &message.id.0,
            SessionMessageCompaction::Completed(message) => &message.id.0,
            SessionMessageCompaction::Failed(message) => &message.id.0,
        },
        SessionMessageInfo::Idle(message) => &message.id.0,
    }
}
