use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    session_create, session_get, session_inbox_list, session_list, session_prompt, session_remove, session_update,
};
use opencode_contract::generated::requests;
use opencode_contract::generated::{
    ConflictError, ConflictErrorTag, InvalidCursorError, InvalidCursorErrorTag, ModelRef, PermissionEffect,
    PermissionRule, PermissionRuleset, PromptAgentAttachment, PromptMention, ProviderID, SessionCreateResponse,
    SessionGetResponse, SessionInboxDelivery, SessionInboxListResponse, SessionNotFoundError, SessionNotFoundErrorTag,
    SessionPromptResponse,
};
use opencode_contract::request::RawRequest;
use opencode_location::location::LocationRef;
use opencode_session::{CreateInput, PromptInput, SessionError, UpdateInput};

use crate::handlers::{defect, invalid};
use crate::response;
use crate::services::GlobalServices;

pub async fn create(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let body = match session_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let location = body
        .location
        .flatten()
        .map(|location| LocationRef::new(location.directory.0, None))
        .unwrap_or_else(|| LocationRef::new(services.start_directory.clone(), None))
        .canonical();
    let input = CreateInput {
        id: body.id.flatten().map(|id| id.0),
        parent_id: body.parent_id.flatten().map(|id| id.0),
        title: body.title.flatten(),
        agent: body.agent.flatten().map(|agent| agent.0),
        model: body.model.flatten().map(|model| ModelRef {
            id: model.id,
            provider_id: ProviderID(model.provider_id.0),
            variant: model.variant,
        }),
        location: Some(location),
        metadata: body.metadata.flatten(),
        permissions: body.permissions.flatten().map(contract_permissions),
    };
    match sessions.create(input).await {
        Ok(data) => response::bytes(StatusCode::OK, "application/json", encode_json(&SessionCreateResponse { data })),
        Err(error) => error_response(error),
    }
}

pub fn get(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match sessions.get(&params.session_id.0) {
        Ok(data) => response::bytes(StatusCode::OK, "application/json", encode_json(&SessionGetResponse { data })),
        Err(error) => error_response(error),
    }
}

pub fn list(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let query = match session_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    match sessions.list(&query) {
        Ok(list) => response::bytes(StatusCode::OK, "application/json", encode_json(&list)),
        Err(error) => error_response(error),
    }
}

pub async fn update(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = UpdateInput {
        title: body.title.flatten(),
        metadata: body.metadata.flatten(),
        permissions: body.permissions.flatten().map(contract_permissions),
        archived: body.archived.flatten(),
    };
    match sessions.update(&params.session_id.0, input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

pub async fn remove(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_remove::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match sessions.remove(&params.session_id.0).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

pub fn prompt(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_prompt::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_prompt::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    if body.files.as_ref().is_some_and(|files| !files.is_empty())
        || body.skills.as_ref().is_some_and(|skills| !skills.is_empty())
    {
        let body = serde_json::json!({
            "_tag": "UnknownError",
            "message": "opencode-server does not implement prompt attachments yet",
        });
        return response::json(StatusCode::NOT_IMPLEMENTED, &body);
    }
    let input = PromptInput {
        id: body.id.flatten().map(|id| id.0),
        text: body.text,
        agents: body.agents.map(contract_agents),
        metadata: body.metadata,
        delivery: body.delivery.flatten().map(contract_delivery).unwrap_or(SessionInboxDelivery::Steer),
        resume: body.resume.flatten().unwrap_or(true),
    };
    match sessions.prompt(&params.session_id.0, input) {
        Ok(data) => response::bytes(StatusCode::OK, "application/json", encode_json(&SessionPromptResponse { data })),
        Err(error) => error_response(error),
    }
}

pub fn inbox(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let params = match session_inbox_list::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    if let Err(error) = sessions.get(&params.session_id.0) {
        return error_response(error);
    }
    match sessions.store().inbox(&params.session_id.0) {
        Ok(data) => {
            response::bytes(StatusCode::OK, "application/json", encode_json(&SessionInboxListResponse { data }))
        }
        Err(error) => defect::response("failed to list session inbox", error),
    }
}

fn contract_delivery(delivery: requests::SessionInboxDelivery) -> SessionInboxDelivery {
    match delivery {
        requests::SessionInboxDelivery::Steer => SessionInboxDelivery::Steer,
        requests::SessionInboxDelivery::Queue => SessionInboxDelivery::Queue,
    }
}

fn contract_agents(agents: Vec<requests::PromptAgentAttachment>) -> Vec<PromptAgentAttachment> {
    agents
        .into_iter()
        .map(|agent| PromptAgentAttachment {
            name: agent.name,
            mention: agent.mention.map(|mention| PromptMention {
                start: mention.start,
                end: mention.end,
                text: mention.text,
            }),
        })
        .collect()
}

fn contract_permissions(rules: Vec<requests::PermissionRule>) -> PermissionRuleset {
    rules
        .into_iter()
        .map(|rule| PermissionRule {
            action: rule.action,
            resource: rule.resource,
            effect: match rule.effect {
                requests::PermissionEffect::Allow => PermissionEffect::Allow,
                requests::PermissionEffect::Deny => PermissionEffect::Deny,
                requests::PermissionEffect::Ask => PermissionEffect::Ask,
            },
        })
        .collect()
}

pub(crate) fn error_response(error: SessionError) -> response::Response {
    match error {
        SessionError::NotFound(id) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&SessionNotFoundError {
                tag: SessionNotFoundErrorTag::SessionNotFoundError,
                session_id: id.clone(),
                message: format!("Session not found: {id}"),
            }),
        ),
        SessionError::InvalidCursor => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&InvalidCursorError {
                tag: InvalidCursorErrorTag::InvalidCursorError,
                message: "Invalid cursor".to_owned(),
            }),
        ),
        SessionError::Conflict { message_id } => response::bytes(
            StatusCode::CONFLICT,
            "application/json",
            encode_json(&ConflictError {
                tag: ConflictErrorTag::ConflictError,
                message: format!("Prompt message ID conflicts with an existing durable record: {message_id}"),
                resource: Some(Some(message_id)),
            }),
        ),
        SessionError::Internal(message) => defect::response("failed to handle session request", message),
    }
}
