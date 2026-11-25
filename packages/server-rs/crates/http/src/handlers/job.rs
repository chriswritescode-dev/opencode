use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::ExperimentalJobListResponse;
use opencode_contract::generated::operations::experimental_job_list;
use opencode_contract::request::RawRequest;

use crate::handlers::{invalid, session};
use crate::response;
use crate::services::GlobalServices;

pub async fn list(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let Some(sessions) = services.sessions() else {
        return crate::services::services_unavailable();
    };
    let query = match experimental_job_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    if let Some(Some(id)) = query.session_id
        && let Err(error) = sessions.get(&id.0)
    {
        return session::error_response(error);
    }
    response::bytes(StatusCode::OK, "application/json", encode_json(&ExperimentalJobListResponse { data: Vec::new() }))
}
