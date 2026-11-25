use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    session_create, session_get, session_list, session_remove, session_update,
};
use opencode_contract::generated::requests;
use opencode_contract::generated::{
    InvalidCursorError, InvalidCursorErrorTag, ModelRef, PermissionEffect, PermissionRule, PermissionRuleset,
    ProviderID, SessionCreateResponse, SessionGetResponse, SessionNotFoundError, SessionNotFoundErrorTag,
};
use opencode_contract::request::RawRequest;
use opencode_location::location::LocationRef;
use opencode_session::{CreateInput, SessionError, UpdateInput};

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

fn error_response(error: SessionError) -> response::Response {
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
        SessionError::Internal(message) => defect::response("failed to handle session request", message),
    }
}
