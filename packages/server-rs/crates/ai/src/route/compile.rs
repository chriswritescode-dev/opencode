use serde_json::Value;

use crate::ExecutionContext;
use crate::route::endpoint::PathPart;
use crate::route::executor::Middleware;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::transport::http::{HttpPrepared, TransportPrepareInput, prepare};
use crate::schema::{
    AiError, AiErrorReason, ContentPart, LlmRequest, MediaSource, Message, ToolContent, ToolResultValue,
};

#[derive(Clone, Default)]
pub struct StreamOptions {
    pub http: Option<Middleware>,
}

pub struct Compiled {
    pub resolved: ResolvedRequest,
    pub body: Value,
    pub prepared: HttpPrepared,
}

pub fn compile(
    model: &Model,
    request: LlmRequest,
    options: &StreamOptions,
    context: &ExecutionContext,
) -> Result<Compiled, AiError> {
    reject_local_files(&request)?;
    let request = crate::prepare::prepare(model, request);
    let resolved = ResolvedRequest { request, model: model.clone() };
    let body = resolved.model.route.protocol.build_body(&resolved, &resolved.model, context)?;
    let mut endpoint = resolved.model.route.endpoint.clone();
    if matches!(endpoint.path, PathPart::Default) {
        endpoint.path =
            PathPart::Static(resolved.model.route.protocol.default_path(&resolved.model, &body).unwrap_or_default());
    }
    let prepared = prepare(
        TransportPrepareInput {
            endpoint,
            request: resolved.request.clone(),
            body: body.clone(),
            auth: resolved.model.route.auth.clone(),
            encode_body: None,
            headers: None,
            middleware: options.http.clone(),
            framing: resolved.model.route.protocol.framing(),
        },
        context.env.as_ref(),
    )?;
    Ok(Compiled { resolved, body, prepared })
}

fn reject_local_files(request: &LlmRequest) -> Result<(), AiError> {
    for message in &request.messages {
        for part in message_content(message) {
            if local_references(part).any(is_local_file) {
                return Err(local_file_error());
            }
        }
    }
    Ok(())
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn local_references(part: &ContentPart) -> impl Iterator<Item = &str> {
    let references: Vec<&str> = match part {
        ContentPart::Media(part) => match &part.media.source {
            MediaSource::Url { url, .. } => vec![url],
            MediaSource::Ref { id, .. } => vec![id],
            MediaSource::Bytes { .. } | MediaSource::Base64 { .. } => Vec::new(),
        },
        ContentPart::ToolResult(part) => match &part.result {
            ToolResultValue::Content { value } => value
                .iter()
                .filter_map(|item| match item {
                    ToolContent::File { uri, .. } => Some(uri.as_str()),
                    ToolContent::Text { .. } => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    references.into_iter()
}

fn is_local_file(reference: &str) -> bool {
    reference.trim_start().to_ascii_lowercase().starts_with("file:")
}

fn local_file_error() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "Local file references must be read with Media.file(path) before sending".to_string(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}
