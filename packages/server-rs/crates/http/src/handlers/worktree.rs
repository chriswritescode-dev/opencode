use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{worktree_create, worktree_list, worktree_refresh, worktree_remove};
use opencode_contract::generated::{
    AbsolutePath, ProjectNotFoundError, ProjectNotFoundErrorTag, WorktreeDirectory, WorktreeErrorData,
    WorktreeErrorName, WorktreeErrorTag, WorktreeInfo,
};
use opencode_contract::request::RawRequest;
use opencode_vcs::worktree::{CreateInput, RemoveInput, WorktreeError};

use crate::handlers::invalid;
use crate::response;
use crate::services::GlobalServices;

pub async fn list(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let query = match worktree_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    match services.worktree.list(&query.project_id.0).await {
        Ok(worktrees) => {
            let body = worktrees
                .into_iter()
                .map(|worktree| WorktreeDirectory {
                    directory: AbsolutePath(worktree.directory),
                    strategy: worktree.strategy,
                })
                .collect::<Vec<_>>();
            response::bytes(StatusCode::OK, "application/json", encode_json(&body))
        }
        Err(error @ WorktreeError::ProjectNotFound(_)) => error_response(error),
        Err(error) => crate::handlers::defect::response("failed to list worktrees", error),
    }
}

pub async fn create(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let body = match worktree_create::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = CreateInput {
        project_id: body.project_id.0,
        from: body.from.map(|from| from.0),
        branch: body.branch.map(|branch| branch.0),
        directory: body.directory.map(|directory| directory.0),
        name: body.name,
    };
    match services.worktree.create(input).await {
        Ok(info) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&WorktreeInfo { directory: AbsolutePath(info.directory) }),
        ),
        Err(error) => error_response(error),
    }
}

pub async fn remove(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let body = match worktree_remove::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = RemoveInput { project_id: body.project_id.0, directory: body.directory.0, force: body.force };
    match services.worktree.remove(input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

pub async fn refresh(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let body = match worktree_refresh::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    match services.worktree.refresh(&body.project_id.0).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => error_response(error),
    }
}

fn error_response(error: WorktreeError) -> response::Response {
    match &error {
        WorktreeError::ProjectNotFound(project_id) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&ProjectNotFoundError {
                tag: ProjectNotFoundErrorTag::ProjectNotFoundError,
                project_id: project_id.clone(),
                message: format!("Project not found: {project_id}"),
            }),
        ),
        WorktreeError::Storage(_) | WorktreeError::Message(_) => {
            crate::handlers::defect::response("failed to handle worktree request", &error)
        }
        error => response::bytes(
            StatusCode::BAD_REQUEST,
            "application/json",
            encode_json(&opencode_contract::generated::WorktreeError {
                tag: WorktreeErrorTag::WorktreeError,
                name: WorktreeErrorName::WorktreeError,
                data: WorktreeErrorData { message: message(error), force_required: error.force_required().map(Some) },
            }),
        ),
    }
}

fn message(error: &WorktreeError) -> String {
    match error {
        WorktreeError::SourceDirectoryNotFound { directory: Some(directory), .. } => format!(
            "Cannot create a worktree from {directory}: it is not a checkout of this project that supports worktrees, such as a git repository"
        ),
        WorktreeError::SourceDirectoryNotFound { project_id, directory: None } => {
            format!("Worktree source not found for project: {project_id}")
        }
        WorktreeError::DestinationExists { directory } => format!("Worktree destination already exists: {directory}"),
        WorktreeError::DirectoryUnavailable { directory } => format!("Worktree directory unavailable: {directory}"),
        WorktreeError::InvalidDirectory { directory } => format!("Invalid worktree directory: {directory}"),
        WorktreeError::StrategyUnavailable { strategy } => format!("Worktree strategy unavailable: {strategy}"),
        WorktreeError::Git(error) => error.message.clone(),
        WorktreeError::Operation { message, .. } => message.clone(),
        error => error.to_string(),
    }
}
