use std::collections::HashMap;

use http::StatusCode;
use opencode_catalog::credential::{CreateInput, CredentialError, Info, Update};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    credential_activate, credential_create, credential_remove, credential_update,
};
use opencode_contract::generated::{
    ConflictError, ConflictErrorTag, CredentialCreateResponse, CredentialEntry, CredentialExternal,
    CredentialExternalType, CredentialID, CredentialKey, CredentialKeyType, CredentialListResponse, CredentialOAuth,
    CredentialOAuthType, CredentialValue, IntegrationID, IntegrationMethodID, requests,
};
use opencode_contract::request::RawRequest;
use opencode_location::location::LocationRef;
use opencode_location::map::Lease;

use crate::handlers::{integration_policy, invalid};
use crate::response;
use crate::services::{GlobalServices, LocationServices, location_error, service_unavailable};

pub async fn list(globals: &GlobalServices) -> response::Response {
    match globals.credentials.all() {
        Ok(credentials) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&CredentialListResponse { data: entries(credentials) }),
        ),
        Err(error) => service_unavailable("credential", error),
    }
}

pub async fn create(raw: &RawRequest, globals: &GlobalServices) -> response::Response {
    let body = match credential_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let lease = match global_location(globals).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let services = lease.services();
    let integration_id = IntegrationID(body.integration_id.0);
    if let Err(response) =
        integration_policy::require_connectable(&integration_id, &services.integrations, &services.config).await
    {
        return response;
    }
    let created = match globals.credentials.create(CreateInput {
        id: body.id.map(|id| CredentialID(id.0)),
        integration_id,
        value: contract_value(body.value),
        label: body.label,
        activate: body.activate,
    }) {
        Ok(created) => created,
        Err(CredentialError::Conflict(id)) => {
            return response::bytes(
                StatusCode::CONFLICT,
                "application/json",
                encode_json(&ConflictError {
                    tag: ConflictErrorTag::ConflictError,
                    message: format!("Credential already exists: {}", id.0),
                    resource: Some(Some(id.0)),
                }),
            );
        }
        Err(error) => return service_unavailable("credential", error),
    };
    let entry = match globals.credentials.list(&created.integration_id) {
        Ok(credentials) => entries(credentials).into_iter().find(|entry| entry.id == created.id),
        Err(error) => return service_unavailable("credential", error),
    };
    let data = entry.unwrap_or(CredentialEntry {
        id: created.id,
        integration_id: created.integration_id,
        label: created.label,
        active: false,
        value: created.value,
    });
    response::bytes(StatusCode::OK, "application/json", encode_json(&CredentialCreateResponse { data }))
}

pub async fn update(raw: &RawRequest, globals: &GlobalServices) -> response::Response {
    let params = match credential_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match credential_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    match globals
        .credentials
        .update(&CredentialID(params.credential_id.0), Update { label: Some(body.label), value: None })
    {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => service_unavailable("credential", error),
    }
}

pub async fn activate(raw: &RawRequest, globals: &GlobalServices) -> response::Response {
    let params = match credential_activate::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    match globals.credentials.activate(&CredentialID(params.credential_id.0)) {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => service_unavailable("credential", error),
    }
}

pub async fn remove(raw: &RawRequest, globals: &GlobalServices) -> response::Response {
    let params = match credential_remove::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let lease = match global_location(globals).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    match lease.services().integrations.connection().remove(&CredentialID(params.credential_id.0)).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => service_unavailable("integration", error),
    }
}

#[allow(clippy::result_large_err)]
async fn global_location(globals: &GlobalServices) -> Result<Lease<LocationServices>, response::Response> {
    let reference = LocationRef::new(globals.paths.config.to_string_lossy().into_owned(), None);
    globals.locations.get(&reference).await.map_err(location_error)
}

fn contract_value(value: requests::CredentialValue) -> CredentialValue {
    match value {
        requests::CredentialValue::Key(key) => {
            CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: key.key, metadata: key.metadata })
        }
        requests::CredentialValue::Oauth(oauth) => CredentialValue::Oauth(CredentialOAuth {
            r#type: CredentialOAuthType::Oauth,
            method_id: IntegrationMethodID(oauth.method_id.0),
            refresh: oauth.refresh,
            access: oauth.access,
            expires: oauth.expires,
            metadata: oauth.metadata,
        }),
        requests::CredentialValue::External(external) => CredentialValue::External(CredentialExternal {
            r#type: CredentialExternalType::External,
            method_id: IntegrationMethodID(external.method_id.0),
            metadata: external.metadata,
        }),
    }
}

fn entries(credentials: Vec<Info>) -> Vec<CredentialEntry> {
    let selected = credentials
        .iter()
        .map(|credential| (credential.integration_id.0.clone(), credential.id.0.clone()))
        .collect::<HashMap<_, _>>();
    credentials
        .into_iter()
        .map(|credential| {
            let active = selected.get(&credential.integration_id.0) == Some(&credential.id.0);
            CredentialEntry {
                id: credential.id,
                integration_id: credential.integration_id,
                label: credential.label,
                active,
                value: credential.value,
            }
        })
        .collect()
}
