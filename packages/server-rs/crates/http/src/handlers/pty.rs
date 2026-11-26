use std::sync::Arc;

use bytes::Bytes;
use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::stream::{SplitSink, SplitStream};
use http::{Request, StatusCode};
use hyper::upgrade::OnUpgrade;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    pty_connect, pty_connect_token, pty_create, pty_get, pty_list, pty_remove, pty_update,
};
use opencode_contract::generated::{
    ForbiddenError, ForbiddenErrorTag, PtyConnectTokenResponse, PtyCreateResponse, PtyGetResponse, PtyListResponse,
    PtyNotFoundError, PtyNotFoundErrorTag, PtyUpdateResponse,
};
use opencode_contract::request::RawRequest;
use opencode_location::lifecycle::RetainGuard;
use opencode_location::map::Lease;
use opencode_terminal::pty::{self, AttachInput, Attachment, Pty, PtyError};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;

use crate::app::App;
use crate::cors;
use crate::handlers::{defect, invalid, location_ref};
use crate::response;
use crate::services::LocationServices;
use crate::ticket::{TicketScope, TicketStore};
use crate::websocket::{self, Socket};

const CONNECT_TOKEN_HEADER: &str = "x-opencode-ticket";
const CONNECT_TOKEN_HEADER_VALUE: &str = "1";
const TICKET_QUERY: &str = "ticket";

pub fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = pty_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&PtyListResponse { location: location_ref(services), data: services.pty.list() }),
    )
}

pub fn create(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = pty_create::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let body = match pty_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = pty::CreateInput { cwd: body.cwd.or_else(|| Some(services.info.directory.clone())), ..body };
    match services.pty.create(input) {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&PtyCreateResponse { location: location_ref(services), data }),
        ),
        Err(error) => error_response("failed to create the PTY", error),
    }
}

pub fn get(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match pty_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = pty_get::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.pty.get(&params.pty_id.0) {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&PtyGetResponse { location: location_ref(services), data }),
        ),
        Err(error) => error_response("failed to get the PTY", error),
    }
}

pub fn update(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match pty_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = pty_update::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let body = match pty_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    match services.pty.update(&params.pty_id.0, body) {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&PtyUpdateResponse { location: location_ref(services), data }),
        ),
        Err(error) => error_response("failed to update the PTY", error),
    }
}

pub fn remove(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match pty_remove::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = pty_remove::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.pty.remove(&params.pty_id.0) {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response("failed to remove the PTY", error),
    }
}

pub fn connect_token(
    raw: &RawRequest,
    services: &LocationServices,
    tickets: &TicketStore,
    allowed_cors: &[String],
) -> response::Response {
    let params = match pty_connect_token::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = pty_connect_token::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    if header(raw, CONNECT_TOKEN_HEADER) != Some(CONNECT_TOKEN_HEADER_VALUE)
        || !cors::is_allowed_request_origin(header(raw, "origin"), header(raw, "host"), allowed_cors)
    {
        return forbidden("Invalid PTY connect token request");
    }
    let pty_id = params.pty_id.0;
    if let Err(error) = services.pty.get(&pty_id) {
        return error_response("failed to get the PTY", error);
    }
    let data = tickets.issue(TicketScope {
        target: format!("pty:{pty_id}"),
        directory: services.info.directory.clone(),
        workspace_id: services.info.workspace_id.clone(),
    });
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&PtyConnectTokenResponse { location: location_ref(services), data }),
    )
}

pub async fn connect(
    app: &App,
    request: &Request<Bytes>,
    raw: &RawRequest,
    upgrade: Option<OnUpgrade>,
) -> response::Response {
    let params = match pty_connect::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if !cors::is_allowed_request_origin(header(raw, "origin"), header(raw, "host"), app.cors()) {
        return response::empty(StatusCode::FORBIDDEN);
    }
    let Some(services) = app.services() else {
        return crate::services::services_unavailable();
    };
    let reference = opencode_location::resolve::request_ref(raw, &services.start_directory).canonical();
    if let Some(ticket) = query_value(&raw.query, TICKET_QUERY).filter(|ticket| !ticket.is_empty()) {
        let scope = TicketScope {
            target: format!("pty:{}", params.pty_id.0),
            directory: reference.directory.clone(),
            workspace_id: reference.workspace_id.clone(),
        };
        if !app.tickets().consume(&scope, &ticket) {
            return response::empty(StatusCode::FORBIDDEN);
        }
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let pty_id = params.pty_id.0;
    match lease.services().pty.get(&pty_id) {
        Ok(_) => {}
        Err(PtyError::NotFound { .. }) => return response::empty(StatusCode::NOT_FOUND),
        Err(error) => return defect::response("failed to get the PTY", error),
    }
    let Some(upgrade) = upgrade else {
        return response::empty(StatusCode::BAD_REQUEST);
    };
    let cursor = cursor(&raw.query);
    let pty = lease.services().pty.clone();
    let retain = lease.services().lifecycle.retain();
    websocket::accept(request.headers(), upgrade, move |socket| async move {
        run(socket, retain, lease, pty, pty_id, cursor).await;
    })
}

enum Outbound {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

async fn run(
    socket: Socket,
    _retain: RetainGuard,
    lease: Lease<LocationServices>,
    pty: Arc<Pty>,
    pty_id: String,
    cursor: Option<i64>,
) {
    let services = lease.services().clone();
    let (outbox, receiver) = unbounded_channel();
    let attachment = attach(&pty, &pty_id, cursor, &outbox);
    let (sink, stream) = socket.split();
    let mut writer = Box::pin(write_loop(sink, receiver));
    let mut reader = Box::pin(read_loop(stream, attachment.as_ref()));
    let shutdown = services.lifecycle.await_shutdown();
    tokio::pin!(shutdown);
    tokio::select! {
        _ = &mut writer => {}
        _ = &mut reader => {
            let _ = outbox.send(Outbound::Close(1000, String::new()));
            let _ = writer.await;
        }
        _ = &mut shutdown => {
            let _ = outbox.send(Outbound::Close(1012, "location shut down".to_owned()));
            let _ = writer.await;
        }
    }
    if let Some(attachment) = &attachment {
        attachment.detach();
    }
    drop(lease);
}

fn attach(pty: &Pty, pty_id: &str, cursor: Option<i64>, outbox: &UnboundedSender<Outbound>) -> Option<Attachment> {
    let data_outbox = outbox.clone();
    let end_outbox = outbox.clone();
    match pty.attach(
        pty_id,
        AttachInput {
            cursor,
            on_data: Arc::new(move |chunk| {
                let _ = data_outbox.send(Outbound::Text(chunk));
            }),
            on_end: Arc::new(move |_| {
                let _ = end_outbox.send(Outbound::Close(1000, String::new()));
            }),
        },
    ) {
        Ok(attachment) => {
            for chunk in pty::protocol::chunks(&attachment.replay) {
                let _ = outbox.send(Outbound::Text(chunk));
            }
            let _ = outbox.send(Outbound::Binary(pty::protocol::meta_frame(attachment.cursor as i64)));
            attachment.activate();
            Some(attachment)
        }
        Err(PtyError::NotFound { .. }) => {
            let _ = outbox.send(Outbound::Close(4404, "session not found".to_owned()));
            None
        }
        Err(PtyError::Exited { .. }) => {
            let _ = outbox.send(Outbound::Close(4404, "session exited".to_owned()));
            None
        }
        Err(error) => {
            log::warn!("failed to attach to the PTY {pty_id}: {error}");
            let _ = outbox.send(Outbound::Close(1011, "attach failed".to_owned()));
            None
        }
    }
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

async fn read_loop(mut stream: SplitStream<Socket>, attachment: Option<&Attachment>) {
    while let Some(item) = stream.next().await {
        let Ok(message) = item else {
            return;
        };
        match message {
            Message::Text(text) => {
                if let Some(attachment) = attachment {
                    attachment.write(text.to_string());
                }
            }
            Message::Binary(data) => {
                if let Some(attachment) = attachment
                    && let Some(decoded) = pty::protocol::decode_input(&data)
                {
                    attachment.write(decoded);
                }
            }
            Message::Close(_) => return,
            _ => {}
        }
    }
}

fn error_response(context: &str, error: PtyError) -> response::Response {
    match error {
        PtyError::NotFound { pty_id } => not_found(&pty_id),
        other => defect::response(context, other),
    }
}

fn not_found(pty_id: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&PtyNotFoundError {
            tag: PtyNotFoundErrorTag::PtyNotFoundError,
            pty_id: pty_id.to_owned(),
            message: format!("PTY session not found: {pty_id}"),
        }),
    )
}

fn forbidden(message: &str) -> response::Response {
    response::bytes(
        StatusCode::FORBIDDEN,
        "application/json",
        encode_json(&ForbiddenError { tag: ForbiddenErrorTag::ForbiddenError, message: message.to_owned() }),
    )
}

fn header<'a>(raw: &'a RawRequest, name: &str) -> Option<&'a str> {
    raw.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

fn query_value(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
}

fn cursor(query: &str) -> Option<i64> {
    let text = query_value(query, "cursor")?;
    let trimmed = text.trim();
    let value = if trimmed.is_empty() { 0.0 } else { trimmed.parse::<f64>().ok()? };
    if !value.is_finite() || value.fract() != 0.0 || !(-1.0..=9_007_199_254_740_991.0).contains(&value) {
        return None;
    }
    Some(value as i64)
}
