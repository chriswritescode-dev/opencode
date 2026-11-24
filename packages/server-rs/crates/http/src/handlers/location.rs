use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::location_get;
use opencode_contract::generated::{
    AbsolutePath, LocationPublicInfo, LocationPublicInfoProject, ProjectID, ServiceUnavailableError,
    ServiceUnavailableErrorTag,
};
use opencode_contract::request::RawRequest;

use crate::app::App;
use crate::handlers::invalid;
use crate::response;
use crate::services::GlobalServices;

pub async fn get(app: &App, raw: &RawRequest) -> response::Response {
    if let Err(error) = location_get::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let info = &lease.services().info;
    let body = LocationPublicInfo {
        directory: AbsolutePath(info.directory.clone()),
        project: LocationPublicInfoProject {
            id: ProjectID(info.project.id.clone()),
            directory: AbsolutePath(info.project.directory.clone()),
            canonical: AbsolutePath(info.project.canonical.clone()),
        },
    };
    response::bytes(StatusCode::OK, "application/json", encode_json(&body))
}

pub async fn reload(services: &GlobalServices) -> response::Response {
    match services.locations.reload().await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => response::bytes(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            encode_json(&ServiceUnavailableError {
                tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
                message: error.to_string(),
                service: Some(Some("location".to_owned())),
            }),
        ),
    }
}
