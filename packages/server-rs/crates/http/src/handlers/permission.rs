use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    permission_request_list, permission_saved_list, permission_saved_remove, permission_saved_update,
    session_permission_create, session_permission_get, session_permission_list, session_permission_reply,
};
use opencode_contract::generated::{
    self, ConflictError, ConflictErrorTag, PermissionNotFoundError, PermissionNotFoundErrorTag, PermissionRequest,
    PermissionRequestListResponse, PermissionSavedListResponse, PermissionSavedNotFoundError,
    PermissionSavedNotFoundErrorTag, PermissionSavedUpdateResponse, SessionNotFoundError, SessionNotFoundErrorTag,
    SessionPermissionCreateResponse, SessionPermissionCreateResponseData, SessionPermissionGetResponse,
    SessionPermissionListResponse,
};
use opencode_contract::request::RawRequest;
use opencode_permission::saved::SavedError;
use opencode_permission::service::{AssertInput, PermissionError, ReplyInput};

use crate::app::App;
use crate::handlers::{bridge, defect, invalid, location_ref};
use crate::response;
use crate::services::{GlobalServices, LocationServices};

pub fn request_list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = permission_request_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&PermissionRequestListResponse {
            location: location_ref(services),
            data: services.permissions.list(),
        }),
    )
}

pub fn saved_list(raw: &RawRequest, global: &GlobalServices, services: &LocationServices) -> response::Response {
    let query = match permission_saved_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let project_id = query.project_id.flatten().map_or_else(|| services.info.project.id.clone(), |id| id.0);
    match global.saved.list(Some(&project_id)) {
        Ok(data) => {
            response::bytes(StatusCode::OK, "application/json", encode_json(&PermissionSavedListResponse { data }))
        }
        Err(error) => defect::response("failed to list the saved permissions", error),
    }
}

pub fn saved_update(raw: &RawRequest, global: &GlobalServices) -> response::Response {
    let params = match permission_saved_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match permission_saved_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let action = body.action.flatten();
    let resource = body.resource.flatten();
    match global.saved.update(&params.id.0, action.as_deref(), resource.as_deref()) {
        Ok(data) => {
            response::bytes(StatusCode::OK, "application/json", encode_json(&PermissionSavedUpdateResponse { data }))
        }
        Err(SavedError::NotFound { id }) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&PermissionSavedNotFoundError {
                tag: PermissionSavedNotFoundErrorTag::PermissionSavedNotFoundError,
                message: format!("Saved permission not found: {}", id.0),
                id: id.0,
            }),
        ),
        Err(SavedError::Conflict { id, action, resource }) => response::bytes(
            StatusCode::CONFLICT,
            "application/json",
            encode_json(&ConflictError {
                tag: ConflictErrorTag::ConflictError,
                message: format!("A saved permission already allows {action} for {resource}"),
                resource: Some(Some(id.0)),
            }),
        ),
        Err(error) => defect::response("failed to update the saved permission", error),
    }
}

pub fn saved_remove(raw: &RawRequest, global: &GlobalServices) -> response::Response {
    let params = match permission_saved_remove::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match global.saved.remove(&params.id.0) {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => defect::response("failed to remove the saved permission", error),
    }
}

pub async fn session_create(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_permission_create::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_permission_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let lease = match app.session_location(&params.session_id.0).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let input = AssertInput {
        id: body.id.flatten().map(|id| generated::PermissionID(id.0)),
        session_id: generated::SessionID(params.session_id.0.clone()),
        agent: body.agent.flatten().map(|id| generated::AgentID(id.0)),
        action: body.action,
        resources: body.resources,
        save: body.save,
        metadata: body.metadata,
        source: body.source.map(|source| bridge(&source)),
        unattended: None,
    };
    match lease.services().permissions.ask(input).await {
        Ok(result) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&SessionPermissionCreateResponse {
                data: SessionPermissionCreateResponseData { id: result.id, effect: result.effect },
            }),
        ),
        Err(PermissionError::SessionNotFound(id)) => session_not_found(&id.0),
        Err(error) => defect::response("failed to create the permission request", error),
    }
}

pub async fn session_list(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_permission_list::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let lease = match app.session_location(&params.session_id.0).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let data = lease.services().permissions.for_session(&params.session_id.0);
    response::bytes(StatusCode::OK, "application/json", encode_json(&SessionPermissionListResponse { data }))
}

pub async fn session_get(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_permission_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let lease = match app.session_location(&params.session_id.0).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    match owned_request(lease.services(), &params.session_id.0, &params.request_id.0) {
        Ok(data) => {
            response::bytes(StatusCode::OK, "application/json", encode_json(&SessionPermissionGetResponse { data }))
        }
        Err(response) => response,
    }
}

pub async fn session_reply(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_permission_reply::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_permission_reply::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let lease = match app.session_location(&params.session_id.0).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    if let Err(response) = owned_request(lease.services(), &params.session_id.0, &params.request_id.0) {
        return response;
    }
    let input = ReplyInput {
        request_id: generated::PermissionID(params.request_id.0),
        reply: bridge(&body.decision),
        message: body.message.flatten(),
    };
    match lease.services().permissions.reply(input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(PermissionError::NotFound(id)) => permission_not_found(&id.0),
        Err(error) => defect::response("failed to reply to the permission request", error),
    }
}

#[allow(clippy::result_large_err)]
fn owned_request(
    services: &LocationServices,
    session_id: &str,
    request_id: &str,
) -> Result<PermissionRequest, response::Response> {
    match services.permissions.get(request_id) {
        Some(request) if request.session_id.0 == session_id => Ok(request),
        _ => Err(permission_not_found(request_id)),
    }
}

fn permission_not_found(id: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&PermissionNotFoundError {
            tag: PermissionNotFoundErrorTag::PermissionNotFoundError,
            request_id: id.to_owned(),
            message: format!("Permission request not found: {id}"),
        }),
    )
}

fn session_not_found(id: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&SessionNotFoundError {
            tag: SessionNotFoundErrorTag::SessionNotFoundError,
            session_id: id.to_owned(),
            message: format!("Session not found: {id}"),
        }),
    )
}
