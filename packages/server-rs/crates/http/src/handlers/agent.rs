use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::agent_list;
use opencode_contract::generated::{AbsolutePath, AgentListResponse, LocationPublicRef};
use opencode_contract::request::RawRequest;

use crate::app::App;
use crate::handlers::invalid;
use crate::response;

pub async fn list(app: &App, raw: &RawRequest) -> response::Response {
    let lease = match app.location(raw).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    if let Err(error) = agent_list::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let data = lease.services().agents.read().expect("the agents lock is never poisoned").list();
    let location = LocationPublicRef { directory: AbsolutePath(lease.services().info.directory.clone()) };
    response::bytes(StatusCode::OK, "application/json", encode_json(&AgentListResponse { location, data }))
}
