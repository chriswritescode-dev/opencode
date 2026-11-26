use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::ExperimentalUsageLimitsResponse;
use opencode_contract::generated::operations::experimental_usage_limits;
use opencode_contract::request::RawRequest;

use crate::handlers::invalid;
use crate::response;
use crate::services::{LocationServices, service_unavailable};

pub async fn limits(raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = experimental_usage_limits::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    match services.usage_limits.get().await {
        Ok(data) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalUsageLimitsResponse { location: services.public_ref(), data }),
        ),
        Err(error) => service_unavailable("opencode", error),
    }
}
