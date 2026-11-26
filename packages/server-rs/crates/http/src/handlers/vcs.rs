use http::StatusCode;
use opencode_bus::{Definition, PublishOptions};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{vcs_base, vcs_branch_list, vcs_diff, vcs_get, vcs_init, vcs_status};
use opencode_contract::generated::{
    AbsolutePath, ConflictError, ConflictErrorTag, InvalidRequestError, InvalidRequestErrorTag, LocationPublicRef,
    LocationRef, ServiceUnavailableError, ServiceUnavailableErrorTag, VcsBaseResponse, VcsBranchListResponse,
    VcsDiffResponse, VcsGetResponse, VcsInitNotSupportedError, VcsInitNotSupportedErrorTag, VcsStatusResponse,
    WorkspaceID,
};
use opencode_contract::request::RawRequest;
use opencode_location::project::ResolveOptions;
use opencode_vcs::provider::BranchOptions;
use opencode_vcs::vcs::{DiffOptions, InitializeError};

use crate::app::App;
use crate::handlers::{defect, invalid};
use crate::response;

pub async fn init(app: &App, raw: &RawRequest) -> response::Response {
    let query = match vcs_init::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let Some(services) = app.services() else {
        return crate::services::services_unavailable();
    };
    let provider_id = query.provider.flatten().unwrap_or_else(|| "git".to_owned());
    let directory = lease.services().info.project.directory.clone();
    let location_directory = lease.services().info.directory.clone();
    let workspace_id = lease.services().info.workspace_id.clone();
    let environment = lease.services().environment.clone();
    let reference = opencode_location::location::LocationRef::new(location_directory.clone(), workspace_id.clone());
    if let Err(error) = lease.services().vcs.initialize(&provider_id).await {
        return initialize_error(&provider_id, &directory, error);
    }
    drop(lease);
    let resolved = match services.projects.resolve(environment.as_ref(), &directory, ResolveOptions::persist()).await {
        Ok(resolved) => resolved,
        Err(error) => return defect::response("failed to resolve the project after vcs init", error),
    };
    if resolved.vcs.is_none() {
        return unavailable(&provider_id, "VCS initialization failed");
    }
    services.locations.invalidate(&reference).await;
    if let Err(error) = services.bus.publish(
        &Definition::ephemeral("worktree.updated"),
        serde_json::json!({ "projectID": resolved.id }),
        PublishOptions {
            location: Some(LocationRef {
                directory: AbsolutePath(location_directory),
                workspace_id: workspace_id.map(WorkspaceID),
            }),
            ..Default::default()
        },
    ) {
        log::warn!("failed to publish the worktree update: {error}");
    }
    response::empty(StatusCode::NO_CONTENT)
}

pub async fn get(app: &App, raw: &RawRequest) -> response::Response {
    if let Err(error) = vcs_get::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let body = VcsGetResponse {
        location: location_ref(&lease.services().info.directory),
        data: lease.services().vcs.info().await,
    };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn base(app: &App, raw: &RawRequest) -> response::Response {
    if let Err(error) = vcs_base::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let data = match lease.services().vcs.base().await {
        Ok(data) => data,
        Err(error) => return unavailable("vcs", &error.message),
    };
    let body = VcsBaseResponse { location: location_ref(&lease.services().info.directory), data };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn status(app: &App, raw: &RawRequest) -> response::Response {
    if let Err(error) = vcs_status::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let body = VcsStatusResponse {
        location: location_ref(&lease.services().info.directory),
        data: lease.services().vcs.status().await,
    };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn branch_list(app: &App, raw: &RawRequest) -> response::Response {
    let query = match vcs_branch_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let limit = query.limit.flatten().map_or(50.0, |value| value.min(100.0)) as usize;
    let data =
        lease.services().vcs.branches(BranchOptions { search: query.search.flatten(), limit: Some(limit) }).await;
    let body = VcsBranchListResponse { location: location_ref(&lease.services().info.directory), data };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn diff(app: &App, raw: &RawRequest) -> response::Response {
    let query = match vcs_diff::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let options = DiffOptions {
        context: query.context.flatten().map(|value| value as i32),
        base: query.base.flatten(),
        untracked: query.untracked.flatten(),
    };
    let data = match lease.services().vcs.diff(query.mode, options).await {
        Ok(data) => data,
        Err(error) => return unavailable("vcs", &error.message),
    };
    let body = VcsDiffResponse { location: location_ref(&lease.services().info.directory), data };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

fn location_ref(directory: &str) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(directory.to_owned()) }
}

fn unavailable(service: &str, message: &str) -> response::Response {
    response::bytes(
        StatusCode::SERVICE_UNAVAILABLE,
        "application/json",
        encode_json(&ServiceUnavailableError {
            tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
            message: message.to_owned(),
            service: Some(Some(service.to_owned())),
        }),
    )
}

fn initialize_error(provider_id: &str, directory: &str, error: InitializeError) -> response::Response {
    match error {
        InitializeError::Missing => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&InvalidRequestError {
                tag: InvalidRequestErrorTag::InvalidRequestError,
                message: "Project directory does not exist".to_owned(),
                kind: None,
                field: Some(Some("location".to_owned())),
            }),
        ),
        InitializeError::Conflict => response::bytes(
            StatusCode::CONFLICT,
            "application/json",
            encode_json(&ConflictError {
                tag: ConflictErrorTag::ConflictError,
                message: "Project already has version control".to_owned(),
                resource: Some(Some(directory.to_owned())),
            }),
        ),
        InitializeError::Unknown => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&InvalidRequestError {
                tag: InvalidRequestErrorTag::InvalidRequestError,
                message: "Unknown VCS provider".to_owned(),
                kind: None,
                field: Some(Some("provider".to_owned())),
            }),
        ),
        InitializeError::Unsupported => response::bytes(
            StatusCode::NOT_IMPLEMENTED,
            "application/json",
            encode_json(&VcsInitNotSupportedError {
                tag: VcsInitNotSupportedErrorTag::VcsInitNotSupportedError,
                provider_id: provider_id.to_owned(),
                message: "VCS provider does not support initialization".to_owned(),
            }),
        ),
        InitializeError::Failed => unavailable(provider_id, "VCS initialization failed"),
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use http_body_util::BodyExt;
    use opencode_vcs::vcs::InitializeError;

    use super::initialize_error;

    #[tokio::test]
    async fn unsupported_initializers_map_to_501() {
        let response = initialize_error("read-only", "/tmp/project", InitializeError::Unsupported);
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = response.into_body().collect().await.expect("a body").to_bytes();
        assert_eq!(
            std::str::from_utf8(&bytes).expect("utf-8"),
            r#"{"_tag":"VcsInitNotSupportedError","providerID":"read-only","message":"VCS provider does not support initialization"}"#
        );
    }
}
