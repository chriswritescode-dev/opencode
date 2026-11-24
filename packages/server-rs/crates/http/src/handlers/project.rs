use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{project_list, project_update};
use opencode_contract::generated::{
    AbsolutePath, Project, ProjectCommands, ProjectID, ProjectIcon, ProjectNotFoundError, ProjectNotFoundErrorTag,
    ProjectTime, ProjectVcs,
};
use opencode_contract::request::RawRequest;
use opencode_location::project::{CommandsInput, IconInput, ProjectError, UpdateInput};

use crate::handlers::invalid;
use crate::response;
use crate::services::GlobalServices;

pub async fn list(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let query = match project_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let archived = query.archived.flatten().unwrap_or(false);
    match services.projects.list(archived).await {
        Ok(projects) => {
            let projects = projects.into_iter().map(contract_project).collect::<Vec<_>>();
            response::bytes(StatusCode::OK, "application/json", encode_json(&projects))
        }
        Err(error) => unknown_error(error),
    }
}

pub async fn update(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let params = match project_update::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let body = match project_update::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    let input = UpdateInput {
        project_id: params.project_id.0,
        canonical: body.canonical.map(|path| path.0),
        name: body.name,
        icon: body.icon.map(|icon| IconInput { r#override: icon.r#override, color: icon.color }),
        commands: body.commands.map(|commands| CommandsInput { start: commands.start }),
    };
    match services.projects.update(input).await {
        Ok(project) => response::bytes(StatusCode::OK, "application/json", encode_json(&contract_project(project))),
        Err(ProjectError::NotFound(id)) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&ProjectNotFoundError {
                tag: ProjectNotFoundErrorTag::ProjectNotFoundError,
                project_id: id.clone(),
                message: format!("Project not found: {id}"),
            }),
        ),
        Err(error) => unknown_error(error),
    }
}

fn contract_project(project: opencode_location::project::Project) -> Project {
    Project {
        id: ProjectID(project.id),
        canonical: AbsolutePath(project.canonical),
        vcs: project.vcs.map(ProjectVcs),
        name: project.name,
        icon: project.icon.map(|icon| ProjectIcon { url: icon.url, r#override: icon.r#override, color: icon.color }),
        commands: project.commands.map(|commands| ProjectCommands { start: commands.start }),
        time: ProjectTime {
            created: project.time.created as f64,
            updated: project.time.updated as f64,
            active: project.time.active as f64,
            archived: project.time.archived.map(|archived| archived as f64),
        },
        sandboxes: project.sandboxes,
    }
}

fn unknown_error(error: ProjectError) -> response::Response {
    response::json(
        StatusCode::INTERNAL_SERVER_ERROR,
        &serde_json::json!({ "_tag": "UnknownError", "message": error.to_string() }),
    )
}
