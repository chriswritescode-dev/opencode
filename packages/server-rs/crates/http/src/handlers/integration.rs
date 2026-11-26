use http::StatusCode;
use opencode_catalog::integration::{
    Attempt, AttemptError, AttemptMode, AttemptStatus, AttemptTime, AuthorizationError, CommandAttempt,
    CommandAttemptStatus, CommandConnectInput, CommandError, ExternalInput, KeyInput, OAuthConnectInput,
};
use opencode_config::policy::{self, Effect};
use opencode_contract::codec::{JsonMap, encode_json, write_value};
use opencode_contract::generated::operations::{
    experimental_integration_wellknown_add, integration_command_cancel, integration_command_connect,
    integration_command_status, integration_connect_external, integration_connect_key, integration_get,
    integration_list, integration_oauth_cancel, integration_oauth_complete, integration_oauth_connect,
    integration_oauth_status,
};
use opencode_contract::generated::{
    FormAnswer, FormValue, IntegrationAttemptID, IntegrationAttemptNotFoundError, IntegrationAttemptNotFoundErrorTag,
    IntegrationGetResponse, IntegrationID, IntegrationInfo, IntegrationListResponse, IntegrationMethod,
    IntegrationMethodID, IntegrationMethodNotFoundError, IntegrationMethodNotFoundErrorTag, IntegrationNotFoundError,
    IntegrationNotFoundErrorTag,
};
use opencode_contract::request::RawRequest;
use serde_json::{Value, json};

use crate::handlers::{integration_policy, invalid};
use crate::response;
use crate::services::{GlobalServices, LocationServices, service_unavailable};

pub async fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = integration_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let integrations = match services.integrations.list().await {
        Ok(integrations) => integrations,
        Err(error) => return service_unavailable("integration", error),
    };
    let statements = policy::statements(&services.config.entries());
    let data = integrations
        .into_iter()
        .filter(|integration| {
            integration_policy::is_mcp(integration.metadata.as_ref())
                || policy::decision(&statements, policy::Action::IntegrationConnect, &integration.id.0) != Effect::Deny
        })
        .collect();
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&IntegrationListResponse { location: services.public_ref(), data }),
    )
}

pub async fn get(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let integration = match require_integration(services, &integration_id).await {
        Ok(integration) => integration,
        Err(response) => return response,
    };
    response::bytes(
        StatusCode::OK,
        "application/json",
        encode_json(&IntegrationGetResponse { location: services.public_ref(), data: integration }),
    )
}

pub async fn wellknown_add(
    raw: &RawRequest,
    services: &LocationServices,
    globals: &GlobalServices,
) -> response::Response {
    let body = match experimental_integration_wellknown_add::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(body.url.trim_end_matches('/').to_owned());
    if let Err(response) = require_connectable(services, &integration_id).await {
        return response;
    }
    if let Err(error) = globals.wellknown.add(&body.url).await {
        return invalid::kind(error.to_string(), "well_known_discovery");
    }
    services.integrations.reload().await;
    response::empty(StatusCode::NO_CONTENT)
}

pub async fn connect_key(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_connect_key::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match integration_connect_key::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    if let Err(response) = require_connectable(services, &integration_id).await {
        return response;
    }
    if let Err(response) = require_integration(services, &integration_id).await {
        return response;
    }
    let input = KeyInput {
        integration_id,
        key: body.key,
        answer: body.answer.flatten().map(form_answer),
        label: body.label.flatten(),
    };
    match services.integrations.connection().key(input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => authorization(&error),
    }
}

pub async fn connect_external(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_connect_external::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match integration_connect_external::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    if let Err(response) = require_connectable(services, &integration_id).await {
        return response;
    }
    if let Err(response) = require_integration(services, &integration_id).await {
        return response;
    }
    let input = ExternalInput {
        integration_id,
        method_id: IntegrationMethodID(body.method_id.0),
        answer: body.answer.flatten().map(form_answer),
        label: body.label.flatten(),
    };
    match services.integrations.connection().external(input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => authorization(&error),
    }
}

pub async fn oauth_connect(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_oauth_connect::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match integration_oauth_connect::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    if let Err(response) = require_connectable(services, &integration_id).await {
        return response;
    }
    let input = OAuthConnectInput {
        integration_id,
        method_id: IntegrationMethodID(body.method_id.0),
        answer: body.answer.flatten().map(form_answer),
        label: body.label.flatten(),
    };
    match services.integrations.oauth().connect(input).await {
        Ok(attempt) => attempt_response(services, &attempt),
        Err(error) => attempt_error(&error, "OAuth"),
    }
}

pub async fn oauth_status(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_oauth_status::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let attempt_id = IntegrationAttemptID(params.attempt_id.0);
    if let Err(response) = require_integration(services, &integration_id).await {
        return response;
    }
    match services.integrations.oauth().status(&integration_id, &attempt_id).await {
        Ok(status) => payload(services, &oauth_status_value(&status)),
        Err(error) => attempt_error(&error, "OAuth"),
    }
}

pub async fn oauth_complete(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_oauth_complete::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match integration_oauth_complete::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let attempt_id = IntegrationAttemptID(params.attempt_id.0);
    if let Err(response) = require_integration(services, &integration_id).await {
        return response;
    }
    let code = body.code.flatten();
    match services.integrations.oauth().complete(&integration_id, &attempt_id, code.as_deref()).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => attempt_error(&error, "OAuth"),
    }
}

pub async fn oauth_cancel(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_oauth_cancel::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let attempt_id = IntegrationAttemptID(params.attempt_id.0);
    services.integrations.oauth().cancel(&integration_id, &attempt_id).await;
    response::empty(StatusCode::NO_CONTENT)
}

pub async fn command_connect(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_command_connect::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match integration_command_connect::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let method_id = IntegrationMethodID(body.method_id.0);
    if let Err(response) = require_connectable(services, &integration_id).await {
        return response;
    }
    let integration = match require_integration(services, &integration_id).await {
        Ok(integration) => integration,
        Err(response) => return response,
    };
    let command = integration
        .methods
        .iter()
        .any(|method| matches!(method, IntegrationMethod::Command(method) if method.id == method_id));
    if !command {
        return method_not_found(&integration_id, &method_id);
    }
    let input = CommandConnectInput { integration_id, method_id, label: body.label.flatten() };
    match services.integrations.command().connect(input).await {
        Ok(attempt) => command_attempt_response(services, &attempt),
        Err(error) => command_error(&error),
    }
}

pub async fn command_status(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_command_status::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let attempt_id = IntegrationAttemptID(params.attempt_id.0);
    if let Err(response) = require_integration(services, &integration_id).await {
        return response;
    }
    match services.integrations.command().status(&integration_id, &attempt_id).await {
        Ok(status) => payload(services, &command_status_value(&status)),
        Err(error) => command_error(&error),
    }
}

pub async fn command_cancel(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match integration_command_cancel::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let integration_id = IntegrationID(params.integration_id.0);
    let attempt_id = IntegrationAttemptID(params.attempt_id.0);
    services.integrations.command().cancel(&integration_id, &attempt_id).await;
    response::empty(StatusCode::NO_CONTENT)
}

#[allow(clippy::result_large_err)]
async fn require_connectable(
    services: &LocationServices,
    integration_id: &IntegrationID,
) -> Result<(), response::Response> {
    integration_policy::require_connectable(integration_id, &services.integrations, &services.config).await
}

#[allow(clippy::result_large_err)]
async fn require_integration(
    services: &LocationServices,
    integration_id: &IntegrationID,
) -> Result<IntegrationInfo, response::Response> {
    match services.integrations.get(integration_id).await {
        Ok(Some(integration)) => Ok(integration),
        Ok(None) => Err(integration_not_found(integration_id)),
        Err(error) => Err(service_unavailable("integration", error)),
    }
}

fn attempt_response(services: &LocationServices, attempt: &Attempt) -> response::Response {
    let mut data = json!({
        "attemptID": attempt.attempt_id.0,
        "url": attempt.url,
        "instructions": attempt.instructions,
        "mode": match attempt.mode {
            AttemptMode::Auto => "auto",
            AttemptMode::Code => "code",
        },
    });
    if attempt.paste {
        data["paste"] = Value::Bool(true);
    }
    data["time"] = time_value(attempt.time);
    payload(services, &data)
}

fn command_attempt_response(services: &LocationServices, attempt: &CommandAttempt) -> response::Response {
    let data = json!({ "attemptID": attempt.attempt_id.0, "time": time_value(attempt.time) });
    payload(services, &data)
}

fn oauth_status_value(status: &AttemptStatus) -> Value {
    match status {
        AttemptStatus::Pending { time } => json!({ "status": "pending", "time": time_value(*time) }),
        AttemptStatus::Complete { time } => json!({ "status": "complete", "time": time_value(*time) }),
        AttemptStatus::Failed { message, time } => {
            json!({ "status": "failed", "message": message, "time": time_value(*time) })
        }
        AttemptStatus::Expired { time } => json!({ "status": "expired", "time": time_value(*time) }),
    }
}

fn command_status_value(status: &CommandAttemptStatus) -> Value {
    match status {
        CommandAttemptStatus::Pending { message, time } => {
            let mut value = json!({ "status": "pending" });
            if let Some(message) = message {
                value["message"] = Value::String(message.clone());
            }
            value["time"] = time_value(*time);
            value
        }
        CommandAttemptStatus::Complete { time } => json!({ "status": "complete", "time": time_value(*time) }),
        CommandAttemptStatus::Failed { message, time } => {
            json!({ "status": "failed", "message": message, "time": time_value(*time) })
        }
        CommandAttemptStatus::Expired { time } => json!({ "status": "expired", "time": time_value(*time) }),
    }
}

fn form_answer(answer: opencode_contract::generated::requests::FormAnswer) -> FormAnswer {
    JsonMap(answer.0.into_iter().map(|(key, value)| (key, form_value(value))).collect())
}

fn form_value(value: opencode_contract::generated::requests::FormValue) -> FormValue {
    match value {
        opencode_contract::generated::requests::FormValue::String(value) => FormValue::String(value),
        opencode_contract::generated::requests::FormValue::Number(value) => FormValue::Number(value.0),
        opencode_contract::generated::requests::FormValue::String2(_) => FormValue::Number(f64::INFINITY),
        opencode_contract::generated::requests::FormValue::String3(_) => FormValue::Number(f64::NEG_INFINITY),
        opencode_contract::generated::requests::FormValue::String4(_) => FormValue::Number(f64::NAN),
        opencode_contract::generated::requests::FormValue::Bool(value) => FormValue::Bool(value),
        opencode_contract::generated::requests::FormValue::Array(value) => FormValue::Array(value),
    }
}

fn time_value(time: AttemptTime) -> Value {
    json!({ "created": time.created, "expires": time.expires })
}

fn payload(services: &LocationServices, data: &Value) -> response::Response {
    let mut out = String::from("{");
    let mut first = true;
    write_value(&mut out, &mut first, "location", &services.public_ref());
    write_value(&mut out, &mut first, "data", data);
    out.push('}');
    response::bytes(StatusCode::OK, "application/json", out)
}

fn authorization(error: &AuthorizationError) -> response::Response {
    let message = error.cause().map(ToString::to_string).unwrap_or_default();
    if message.trim().is_empty() {
        return invalid::kind("Authentication failed", "integration_authorization");
    }
    invalid::kind(message, "integration_authorization")
}

fn attempt_error(error: &AttemptError, label: &str) -> response::Response {
    match error {
        AttemptError::AttemptNotFound { integration_id, attempt_id } => {
            attempt_not_found(label, integration_id, attempt_id)
        }
        AttemptError::CodeRequired { .. } => {
            invalid::kind("Authorization code is required", "integration_code_required")
        }
        AttemptError::MethodNotFound { integration_id, method_id } => method_not_found(integration_id, method_id),
        AttemptError::Authorization(error) => authorization(error),
    }
}

fn command_error(error: &CommandError) -> response::Response {
    match error {
        CommandError::AttemptNotFound { integration_id, attempt_id } => {
            attempt_not_found("Command", integration_id, attempt_id)
        }
        CommandError::MethodNotFound { integration_id, method_id } => method_not_found(integration_id, method_id),
        CommandError::Authorization(error) => authorization(error),
    }
}

fn integration_not_found(integration_id: &IntegrationID) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&IntegrationNotFoundError {
            tag: IntegrationNotFoundErrorTag::IntegrationNotFoundError,
            integration_id: integration_id.0.clone(),
            message: format!("Integration not found: {}", integration_id.0),
        }),
    )
}

fn attempt_not_found(
    label: &str,
    integration_id: &IntegrationID,
    attempt_id: &IntegrationAttemptID,
) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&IntegrationAttemptNotFoundError {
            tag: IntegrationAttemptNotFoundErrorTag::IntegrationAttemptNotFoundError,
            integration_id: integration_id.0.clone(),
            attempt_id: attempt_id.0.clone(),
            message: format!("{label} attempt not found: {}", attempt_id.0),
        }),
    )
}

fn method_not_found(integration_id: &IntegrationID, method_id: &IntegrationMethodID) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&IntegrationMethodNotFoundError {
            tag: IntegrationMethodNotFoundErrorTag::IntegrationMethodNotFoundError,
            integration_id: integration_id.0.clone(),
            method_id: method_id.0.clone(),
            message: format!("Integration method not found: {}", method_id.0),
        }),
    )
}
