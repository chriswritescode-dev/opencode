use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{agent_get, agent_list, command_list, reference_list, skill_list};
use opencode_contract::generated::{
    AbsolutePath, AgentGetResponse, AgentListResponse, AgentNotFoundError, AgentNotFoundErrorTag, CommandListResponse,
    LocationPublicRef, ReferenceListResponse, SkillListResponse,
};
use opencode_contract::request::RawRequest;

use crate::handlers::{defect, invalid};
use crate::response;
use crate::services::LocationServices;

pub async fn agent_list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = agent_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.agents.list().await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&AgentListResponse { location: location(services), data }),
        ),
        Err(error) => defect::response("failed to list agents", error),
    }
}

pub async fn agent_get(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let params = match agent_get::decode_params(raw) {
        Ok(params) => params,
        Err(error) => return invalid::request(invalid::Kind::Params, error),
    };
    let id = params.agent_id.0;
    match services.agents.get(&id).await {
        Ok(Some(data)) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&AgentGetResponse { location: location(services), data }),
        ),
        Ok(None) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&AgentNotFoundError {
                tag: AgentNotFoundErrorTag::AgentNotFoundError,
                agent_id: id.clone(),
                message: format!("Agent not found: {id}"),
            }),
        ),
        Err(error) => defect::response("failed to get the agent", error),
    }
}

pub async fn command_list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = command_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.commands.list().await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&CommandListResponse { location: location(services), data }),
        ),
        Err(error) => defect::response("failed to list commands", error),
    }
}

pub async fn skill_list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = skill_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.skills.list().await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&SkillListResponse { location: location(services), data }),
        ),
        Err(error) => defect::response("failed to list skills", error),
    }
}

pub async fn reference_list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = reference_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.references.list().await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ReferenceListResponse { location: location(services), data }),
        ),
        Err(error) => defect::response("failed to list references", error),
    }
}

fn location(services: &LocationServices) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(services.info.directory.clone()) }
}
