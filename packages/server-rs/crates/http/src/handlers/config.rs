use std::path::{Path, PathBuf};

use http::StatusCode;
use opencode_config::service::ConfigService;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{config_get, experimental_config_update};
use opencode_contract::request::RawRequest;

use crate::handlers::{defect, invalid};
use crate::response;
use crate::services::GlobalServices;

pub fn get(raw: &RawRequest, config: &ConfigService) -> response::Response {
    if let Err(error) = config_get::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    response::bytes(StatusCode::OK, "application/json", encode_json(&opencode_config::handlers::config_get(config)))
}

pub fn shells(services: &GlobalServices) -> response::Response {
    let path = services.var("PATH");
    let git_bash = services.var("OPENCODE_GIT_BASH_PATH").map(PathBuf::from);
    let shells = opencode_config::handlers::config_shells_with(
        &services.paths.bin,
        git_bash.as_deref(),
        Path::new("/etc/shells"),
        path.as_deref(),
    );
    response::bytes(StatusCode::OK, "application/json", encode_json(&shells))
}

pub async fn update(raw: &RawRequest, config: &ConfigService) -> response::Response {
    let body = match experimental_config_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    match opencode_config::handlers::experimental_config_update(config, body).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => defect::response("failed to update config", error),
    }
}
