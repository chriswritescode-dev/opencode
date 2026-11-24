use bytes::Bytes;
use futures_util::StreamExt;
use http::StatusCode;
use opencode_bus::sse;
use opencode_bus::{LogInput, LogItem};
use opencode_contract::generated::operations::experimental_session_log;
use opencode_contract::generated::{DURABLE_EVENTS, SessionNotFoundError, SessionNotFoundErrorTag};
use opencode_contract::request::RawRequest;

use crate::handlers::invalid;
use crate::response;
use crate::state::ServerState;

pub fn log(state: &ServerState, raw: &RawRequest) -> response::Response {
    let params = match experimental_session_log::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let query = match experimental_session_log::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let session_id = params.session_id.0;
    match state.store.get(&session_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let error = SessionNotFoundError {
                tag: SessionNotFoundErrorTag::SessionNotFoundError,
                session_id: session_id.clone(),
                message: format!("Session not found: {session_id}"),
            };
            return response::bytes(
                StatusCode::NOT_FOUND,
                "application/json",
                opencode_contract::codec::encode_json(&error),
            );
        }
        Err(error) => {
            let body = serde_json::json!({
                "_tag": "UnknownError",
                "message": error.to_string(),
            });
            return response::json(StatusCode::INTERNAL_SERVER_ERROR, &body);
        }
    }
    let after = query.after.flatten().map(|seq| seq.0 as i64);
    let follow = query.follow.flatten().unwrap_or(false);
    let stream = state.bus.log(LogInput { aggregate_id: session_id, after, follow }).filter_map(|item| async move {
        match item {
            Ok(item) => keep(&item).then(|| Ok(Bytes::from(sse::data_frame(&item.json())))),
            Err(error) => Some(Ok(Bytes::from(sse::event_frame("effect/http-api/stream/failure", &cause(&error))))),
        }
    });
    response::stream(StatusCode::OK, "text/event-stream", &[], stream)
}

fn keep(item: &LogItem) -> bool {
    match item {
        LogItem::Synced { .. } => true,
        LogItem::Event(event) => DURABLE_EVENTS.iter().any(|(r#type, ..)| *r#type == event.r#type()),
    }
}

fn cause(error: &opencode_bus::BusError) -> String {
    serde_json::json!([{
        "_tag": "Die",
        "defect": {
            "name": "Error",
            "message": error.to_string(),
        },
    }])
    .to_string()
}
