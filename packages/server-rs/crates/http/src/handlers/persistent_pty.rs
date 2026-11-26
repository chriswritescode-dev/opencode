use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::stream::{SplitSink, SplitStream};
use http::{Request, StatusCode};
use hyper::upgrade::OnUpgrade;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    experimental_persistent_pty_connect, experimental_persistent_pty_connect_token, experimental_persistent_pty_create,
    experimental_persistent_pty_get, experimental_persistent_pty_list, experimental_persistent_pty_read,
    experimental_persistent_pty_remove, experimental_persistent_pty_snapshot, experimental_persistent_pty_update,
};
use opencode_contract::generated::{
    ExperimentalPersistentPtyConnectTokenResponse, ExperimentalPersistentPtyCreateResponse,
    ExperimentalPersistentPtyGetResponse, ExperimentalPersistentPtyHandoffResponse,
    ExperimentalPersistentPtyListResponse, ExperimentalPersistentPtyReadResponse,
    ExperimentalPersistentPtySnapshotResponse, ExperimentalPersistentPtyUpdateResponse,
};
use opencode_contract::generated::{
    ForbiddenError, ForbiddenErrorTag, InvalidRequestError, InvalidRequestErrorTag, PersistentPtyHandoff,
    PersistentPtyHandoffExpiresAt, PersistentPtyHandoffExpiresAtNumber, PtyNotFoundError, PtyNotFoundErrorTag,
    ServiceUnavailableError, ServiceUnavailableErrorTag,
};
use opencode_contract::request::RawRequest;
use opencode_terminal::daemon::StreamEvent;
use opencode_terminal::daemon::protocol::AttachmentRole;
use opencode_terminal::persistent::{self, Attachment, PersistentPty, PersistentPtyError};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;

use crate::app::App;
use crate::cors;
use crate::handlers::{header, invalid, query_value};
use crate::response;
use crate::services::GlobalServices;
use crate::ticket::TicketScope;
use crate::websocket::{self, Socket};

const CONNECT_TOKEN_HEADER: &str = "x-opencode-ticket";
const CONNECT_TOKEN_HEADER_VALUE: &str = "1";
const TICKET_QUERY: &str = "ticket";

pub async fn read(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_read::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let query = match experimental_persistent_pty_read::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let lines = query.lines.flatten().map(|lines| lines.0 as u16);
    match services.persistent_pty.read(&params.session_id.0, lines).await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyReadResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn list(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_list::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match services.persistent_pty.list(Some(&params.session_id.0)).await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyListResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn create(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_create::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match experimental_persistent_pty_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = persistent::CreateInput {
        command: body.command,
        args: body.args,
        cwd: body.cwd,
        title: body.title,
        env: body.env.0.into_iter().collect(),
        cols: body.size.as_ref().map_or(80, |size| size.cols as u16),
        rows: body.size.as_ref().map_or(24, |size| size.rows as u16),
    };
    match services.persistent_pty.create(&params.session_id.0, input).await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyCreateResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn shutdown(services: &GlobalServices) -> response::Response {
    match services.persistent_pty.shutdown().await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

pub async fn handoff(services: &GlobalServices) -> response::Response {
    match services.persistent_pty.handoff().await {
        Ok(handoff) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyHandoffResponse {
                handoff: handoff.map(|handoff| PersistentPtyHandoff {
                    directory: handoff.directory.to_string_lossy().into_owned(),
                    instance_id: handoff.instance_id,
                    ticket: handoff.ticket,
                    expires_at: PersistentPtyHandoffExpiresAt::Number(PersistentPtyHandoffExpiresAtNumber(
                        handoff.expires_at as f64,
                    )),
                }),
            }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn get(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match services.persistent_pty.get(&params.pty_id.0).await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyGetResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn update(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match experimental_persistent_pty_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let pty_id = &params.pty_id.0;
    if let Some(size) = &body.size
        && let Err(error) = services
            .persistent_pty
            .resize(pty_id, size.cols as u16, size.rows as u16, body.attachment_id.as_deref())
            .await
    {
        return error_response(error);
    }
    let result = match body.title {
        None => services.persistent_pty.get(pty_id).await,
        Some(title) => services.persistent_pty.rename(pty_id, title.as_deref()).await,
    };
    match result {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtyUpdateResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn snapshot(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_snapshot::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match services.persistent_pty.snapshot(&params.pty_id.0).await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalPersistentPtySnapshotResponse { data }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn remove(raw: &RawRequest, services: &GlobalServices) -> response::Response {
    let params = match experimental_persistent_pty_remove::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match services.persistent_pty.remove(&params.pty_id.0).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

pub async fn connect_token(raw: &RawRequest, services: &GlobalServices, app: &App) -> response::Response {
    let params = match experimental_persistent_pty_connect_token::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if header(raw, CONNECT_TOKEN_HEADER) != Some(CONNECT_TOKEN_HEADER_VALUE)
        || !cors::is_allowed_request_origin(header(raw, "origin"), header(raw, "host"), app.cors())
    {
        return forbidden("Invalid persistent PTY connect token request");
    }
    let pty_id = params.pty_id.0;
    if let Err(error) = services.persistent_pty.get(&pty_id).await {
        return error_response(error);
    }
    let data = app.tickets().issue(persistent_scope(&pty_id));
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&ExperimentalPersistentPtyConnectTokenResponse { data }),
    )
}

pub async fn connect(
    app: &App,
    request: &Request<Bytes>,
    raw: &RawRequest,
    upgrade: Option<OnUpgrade>,
) -> response::Response {
    let params = match experimental_persistent_pty_connect::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if !cors::is_allowed_request_origin(header(raw, "origin"), header(raw, "host"), app.cors()) {
        return response::empty(StatusCode::FORBIDDEN);
    }
    if let Some(ticket) = query_value(&raw.query, TICKET_QUERY).filter(|ticket| !ticket.is_empty())
        && !app.tickets().consume(&persistent_scope(&params.pty_id.0), &ticket)
    {
        return response::empty(StatusCode::FORBIDDEN);
    }
    let Some(cursor) = cursor(&raw.query) else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    let role = match query_value(&raw.query, "role").as_deref() {
        Some("observer") => AttachmentRole::Observer,
        _ => AttachmentRole::Controller,
    };
    let framed = query_value(&raw.query, "input_protocol").as_deref() == Some("1");
    let attachment_id = query_value(&raw.query, "attachment_id").unwrap_or_else(opencode_local::id::v4);
    let takeover = query_value(&raw.query, "takeover").as_deref() == Some("true");
    let Some(services) = app.services() else {
        return crate::services::services_unavailable();
    };
    let Some(upgrade) = upgrade else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    let service = services.persistent_pty.clone();
    let pty_id = params.pty_id.0;
    websocket::accept(request.headers(), upgrade, move |socket| async move {
        run(socket, service, pty_id, cursor, role, framed, attachment_id, takeover).await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    socket: Socket,
    service: Arc<PersistentPty>,
    pty_id: String,
    cursor: u64,
    role: AttachmentRole,
    framed: bool,
    attachment_id: String,
    takeover: bool,
) {
    let (sink, stream) = socket.split();
    let (outbox, receiver) = unbounded_channel();
    let attachment = attach(&service, &pty_id, cursor, &attachment_id, role, takeover, &outbox).await;
    let mut writer = Box::pin(write_loop(sink, receiver));
    let Some(attachment) = attachment else {
        let _ = outbox.send(Outbound::Close(4404, "terminal unavailable".to_owned()));
        let _ = writer.await;
        return;
    };
    let attached = json!({
        "type": "attached",
        "attachmentID": attachment_id,
        "inputProtocol": if framed { 1 } else { 0 },
        "info": info_value(&attachment.info),
        "role": match attachment.role {
            AttachmentRole::Controller => "controller",
            AttachmentRole::Observer => "observer",
        },
        "generation": attachment.generation,
        "replay": {
            "requestedOffset": attachment.replay.requested_offset,
            "availableOffset": attachment.replay.available_offset,
            "endOffset": attachment.replay.end_offset,
            "truncated": attachment.replay.truncated,
        },
    });
    let _ = outbox.send(Outbound::Text(attached.to_string()));
    if !attachment.replay.data.is_empty() {
        let _ = outbox.send(Outbound::Binary(attachment.replay.data.clone()));
    }
    let _ = outbox.send(Outbound::Text(
        json!({ "type": "replay_complete", "endOffset": attachment.replay.end_offset }).to_string(),
    ));
    attachment.activate();
    let cols = attachment.info.size.cols as u16;
    let rows = attachment.info.size.rows as u16;
    let mut reader = Box::pin(read_loop(stream, service, pty_id, attachment_id, cols, rows, framed));
    tokio::select! {
        _ = &mut writer => {}
        _ = &mut reader => {
            let _ = outbox.send(Outbound::Close(1000, String::new()));
            let _ = writer.await;
        }
    }
    attachment.detach();
}

fn attach(
    service: &PersistentPty,
    pty_id: &str,
    cursor: u64,
    attachment_id: &str,
    role: AttachmentRole,
    takeover: bool,
    outbox: &UnboundedSender<Outbound>,
) -> impl std::future::Future<Output = Option<Attachment>> {
    let events = outbox.clone();
    let on_event = Arc::new(move |event: StreamEvent| {
        if let Some(message) = event_message(event) {
            let _ = events.send(message);
        }
    });
    let ended = outbox.clone();
    let on_end = Arc::new(move || {
        let _ = ended.send(Outbound::Close(1000, String::new()));
    });
    let service = service.clone();
    let pty_id = pty_id.to_owned();
    let attachment_id = attachment_id.to_owned();
    async move {
        match service
            .attach(&pty_id, persistent::AttachInput { cursor, attachment_id, role, takeover, on_event, on_end })
            .await
        {
            Ok(attachment) => Some(attachment),
            Err(PersistentPtyError::NotFound { .. } | PersistentPtyError::Unavailable { .. }) => None,
            Err(error) => {
                log::warn!("failed to attach to the persistent PTY {pty_id}: {error}");
                None
            }
        }
    }
}

fn event_message(event: StreamEvent) -> Option<Outbound> {
    match event {
        StreamEvent::Output { data, .. } => Some(Outbound::Binary(data)),
        StreamEvent::Resized { cols, rows, generation, checkpoint } => Some(Outbound::Text(
            json!({
                "type": "resized",
                "cols": cols,
                "rows": rows,
                "generation": generation,
                "checkpoint": STANDARD.encode(checkpoint),
            })
            .to_string(),
        )),
        StreamEvent::Exited { exit_code, final_offset } => {
            let mut message = json!({ "type": "exited", "finalOffset": final_offset });
            if let Some(exit_code) = exit_code {
                message["exitCode"] = json!(exit_code);
            }
            Some(Outbound::Text(message.to_string()))
        }
        StreamEvent::ControllerChanged { attachment_id, generation } => {
            let mut message = json!({ "type": "controller_changed", "generation": generation });
            if let Some(attachment_id) = attachment_id {
                message["attachmentID"] = json!(attachment_id);
            }
            Some(Outbound::Text(message.to_string()))
        }
        StreamEvent::TitleChanged { title } => {
            Some(Outbound::Text(json!({ "type": "title_changed", "title": title }).to_string()))
        }
        StreamEvent::ForegroundProcessChanged { process } => {
            Some(Outbound::Text(json!({ "type": "foreground_process_changed", "process": process }).to_string()))
        }
    }
}

enum Outbound {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

async fn write_loop(mut sink: SplitSink<Socket, Message>, mut receiver: UnboundedReceiver<Outbound>) {
    while let Some(item) = receiver.recv().await {
        let message = match item {
            Outbound::Text(text) => Message::Text(text.into()),
            Outbound::Binary(data) => Message::Binary(data.into()),
            Outbound::Close(code, reason) => {
                let _ = sink.send(Message::Close(Some(websocket::close_frame(code, reason)))).await;
                return;
            }
        };
        if sink.send(message).await.is_err() {
            return;
        }
    }
}

async fn read_loop(
    mut stream: SplitStream<Socket>,
    service: Arc<PersistentPty>,
    pty_id: String,
    attachment_id: String,
    cols: u16,
    rows: u16,
    framed: bool,
) {
    while let Some(item) = stream.next().await {
        let Ok(message) = item else {
            return;
        };
        let data = match message {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(data) => data.to_vec(),
            Message::Close(_) => return,
            _ => continue,
        };
        if !framed {
            let _ = service.input(&pty_id, &attachment_id, cols, rows, &data).await;
            continue;
        }
        if data.len() < 5 {
            continue;
        }
        let kind = data[0];
        let cols = u16::from_be_bytes([data[1], data[2]]);
        let rows = u16::from_be_bytes([data[3], data[4]]);
        if (kind != 0 && kind != 1) || cols == 0 || rows == 0 {
            continue;
        }
        if kind == 0 {
            let _ = service.control(&pty_id, &attachment_id, cols, rows).await;
            continue;
        }
        let _ = service.input(&pty_id, &attachment_id, cols, rows, &data[5..]).await;
    }
}

fn info_value(info: &opencode_contract::generated::PersistentPtyInfo) -> Value {
    serde_json::from_str(&encode_json(info)).expect("a persistent PTY info round trips")
}

fn persistent_scope(pty_id: &str) -> TicketScope {
    TicketScope { target: format!("persistent-pty:{pty_id}"), directory: String::new(), workspace_id: None }
}

fn error_response(error: PersistentPtyError) -> response::Response {
    match error {
        PersistentPtyError::NotFound { pty_id } => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&PtyNotFoundError {
                tag: PtyNotFoundErrorTag::PtyNotFoundError,
                pty_id: pty_id.clone(),
                message: format!("PTY session not found: {pty_id}"),
            }),
        ),
        PersistentPtyError::InvalidCwd { message, .. } => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&InvalidRequestError {
                tag: InvalidRequestErrorTag::InvalidRequestError,
                message,
                kind: Some(Some("invalid_cwd".to_owned())),
                field: Some(Some("cwd".to_owned())),
            }),
        ),
        PersistentPtyError::Unavailable { message } => response::bytes(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            encode_json(&ServiceUnavailableError {
                tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
                message,
                service: Some(Some("opencode-pty".to_owned())),
            }),
        ),
        PersistentPtyError::Request { .. } | PersistentPtyError::ProcessNotFound { .. } => {
            crate::handlers::defect::response("failed to reach the persistent PTY daemon", error)
        }
    }
}

fn forbidden(message: &str) -> response::Response {
    response::bytes(
        StatusCode::FORBIDDEN,
        "application/json",
        encode_json(&ForbiddenError { tag: ForbiddenErrorTag::ForbiddenError, message: message.to_owned() }),
    )
}

fn cursor(query: &str) -> Option<u64> {
    let text = query_value(query, "cursor").unwrap_or_else(|| "0".to_owned());
    let trimmed = text.trim();
    let value = if trimmed.is_empty() { 0.0 } else { trimmed.parse::<f64>().ok()? };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&value) {
        return None;
    }
    Some(value as u64)
}
