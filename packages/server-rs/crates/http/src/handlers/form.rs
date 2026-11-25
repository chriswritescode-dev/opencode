use std::sync::Arc;

use http::StatusCode;
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::operations::{
    form_list, session_form_cancel, session_form_create, session_form_get, session_form_list, session_form_reply,
};
use opencode_contract::generated::{
    self, ConflictError, ConflictErrorTag, FormDetail, FormInfo, FormInfo1, FormListResponse, FormNotFoundError,
    FormNotFoundErrorTag, InvalidRequestError, InvalidRequestErrorTag, SessionFormCreateResponse,
    SessionFormGetResponse, SessionFormListResponse,
};
use opencode_contract::request::RawRequest;
use opencode_permission::form::{CreateInput, FormError, FormState};

use crate::app::App;
use crate::handlers::{bridge, invalid, location_ref};
use crate::response;
use crate::services::LocationServices;

pub fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = form_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let data = services.forms.list(None).iter().map(response_info).collect();
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&FormListResponse { location: location_ref(services), data }),
    )
}

pub async fn session_list(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_form_list::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let services = match services_for(app, raw, &params.session_id, true).await {
        Ok(Some(services)) => services,
        Ok(None) => {
            return response::bytes(
                StatusCode::OK,
                "application/json",
                encode_json(&SessionFormListResponse { data: Vec::new() }),
            );
        }
        Err(response) => return response,
    };
    let data = services.forms.list(Some(&params.session_id)).iter().map(response_info).collect();
    response::bytes(StatusCode::OK, "application/json", encode_json(&SessionFormListResponse { data }))
}

pub async fn session_create(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_form_create::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_form_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let services = match services_for(app, raw, &params.session_id, false).await {
        Ok(Some(services)) => services,
        Ok(None) => return crate::services::services_unavailable(),
        Err(response) => return response,
    };
    let input = CreateInput {
        id: body.id.flatten().map(|id| generated::FormID(id.0)),
        session_id: params.session_id,
        title: body.title,
        metadata: body.metadata,
        fields: bridge(&body.fields.0),
    };
    match services.forms.create(input) {
        Ok(form) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&SessionFormCreateResponse { data: response_info(&form) }),
        ),
        Err(FormError::AlreadyExists(id)) => conflict(&id),
        Err(FormError::InvalidForm { message }) => invalid_form(&message),
        Err(error) => crate::handlers::defect::response("failed to create the form", error),
    }
}

pub async fn session_get(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_form_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let form_id = generated::FormID(params.form_id.0);
    let services = match services_for(app, raw, &params.session_id, false).await {
        Ok(Some(services)) => services,
        Ok(None) => return crate::services::services_unavailable(),
        Err(response) => return response,
    };
    let info = match owned_form(&services, &params.session_id, &form_id.0) {
        Ok(info) => info,
        Err(response) => return response,
    };
    match services.forms.state(&form_id) {
        Ok(state) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&SessionFormGetResponse { data: detail(&info, &state) }),
        ),
        Err(FormError::NotFound(_)) => form_not_found(&form_id.0),
        Err(error) => crate::handlers::defect::response("failed to read the form state", error),
    }
}

pub async fn session_reply(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_form_reply::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match session_form_reply::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let form_id = generated::FormID(params.form_id.0);
    let services = match services_for(app, raw, &params.session_id, false).await {
        Ok(Some(services)) => services,
        Ok(None) => return crate::services::services_unavailable(),
        Err(response) => return response,
    };
    if let Err(response) = owned_form(&services, &params.session_id, &form_id.0) {
        return response;
    }
    match services.forms.reply(&form_id, bridge(&body.answer)) {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => form_settle_error(error, &form_id.0),
    }
}

pub async fn session_cancel(app: &App, raw: &RawRequest) -> response::Response {
    let params = match session_form_cancel::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let query = match session_form_cancel::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let form_id = generated::FormID(params.form_id.0);
    let services = match services_for(app, raw, &params.session_id, false).await {
        Ok(Some(services)) => services,
        Ok(None) => return crate::services::services_unavailable(),
        Err(response) => return response,
    };
    if let Err(response) = owned_form(&services, &params.session_id, &form_id.0) {
        return response;
    }
    match services.forms.cancel(&form_id, query.message.flatten()) {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => form_settle_error(error, &form_id.0),
    }
}

#[allow(clippy::result_large_err)]
async fn services_for(
    app: &App,
    raw: &RawRequest,
    session_id: &str,
    live_only: bool,
) -> Result<Option<Arc<LocationServices>>, response::Response> {
    if session_id == "global" {
        return app.location(raw).await.map(|lease| Some(lease.services().clone()));
    }
    if live_only {
        return app.session_location_live(session_id).await;
    }
    app.session_location(session_id).await.map(|lease| Some(lease.services().clone()))
}

#[allow(clippy::result_large_err)]
fn owned_form(services: &LocationServices, session_id: &str, form_id: &str) -> Result<FormInfo, response::Response> {
    match services.forms.get(&generated::FormID(form_id.to_owned())) {
        Ok(info) if info.session_id == session_id => Ok(info),
        _ => Err(form_not_found(form_id)),
    }
}

fn response_info(form: &FormInfo) -> FormInfo1 {
    decode_json(&encode_json(form)).expect("a form round trips")
}

fn detail(info: &FormInfo, state: &FormState) -> FormDetail {
    let info = response_info(info);
    FormDetail {
        id: info.id,
        session_id: info.session_id,
        title: info.title,
        metadata: info.metadata,
        fields: info.fields,
        state: response_state(state),
    }
}

fn response_state(state: &FormState) -> generated::FormState {
    match state {
        FormState::Pending => generated::FormState::Pending(generated::FormStatePending {
            status: generated::FormStatePendingStatus::Pending,
        }),
        FormState::Answered(answer) => generated::FormState::Answered(generated::FormStateAnswered {
            status: generated::FormStateAnsweredStatus::Answered,
            answer: decode_json(&encode_json(answer)).expect("a form answer round trips"),
        }),
        FormState::Cancelled(message) => generated::FormState::Cancelled(generated::FormStateCancelled {
            status: generated::FormStateCancelledStatus::Cancelled,
            message: message.clone(),
        }),
    }
}

fn form_not_found(id: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&FormNotFoundError {
            tag: FormNotFoundErrorTag::FormNotFoundError,
            id: id.to_owned(),
            message: format!("Form not found: {id}"),
        }),
    )
}

fn form_settle_error(error: FormError, id: &str) -> response::Response {
    match error {
        FormError::NotFound(_) => form_not_found(id),
        FormError::AlreadySettled(_) => response::bytes(
            StatusCode::CONFLICT,
            "application/json",
            encode_json(&generated::FormAlreadySettledError {
                tag: generated::FormAlreadySettledErrorTag::FormAlreadySettledError,
                id: id.to_owned(),
                message: format!("Form already settled: {id}"),
            }),
        ),
        FormError::InvalidAnswer { message, .. } => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&generated::FormInvalidAnswerError {
                tag: generated::FormInvalidAnswerErrorTag::FormInvalidAnswerError,
                id: id.to_owned(),
                message,
            }),
        ),
        error => crate::handlers::defect::response("failed to settle the form", error),
    }
}

fn conflict(id: &str) -> response::Response {
    response::bytes(
        StatusCode::CONFLICT,
        "application/json",
        encode_json(&ConflictError {
            tag: ConflictErrorTag::ConflictError,
            message: format!("Form already exists: {id}"),
            resource: Some(Some(id.to_owned())),
        }),
    )
}

fn invalid_form(message: &str) -> response::Response {
    response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message: message.to_owned(),
            kind: None,
            field: Some(Some("fields".to_owned())),
        }),
    )
}
