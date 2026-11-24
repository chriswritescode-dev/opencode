use http::StatusCode;

use crate::response;

pub fn query(error: opencode_contract::codec::DecodeError) -> response::Response {
    let body = serde_json::json!({
        "_tag": "InvalidRequestError",
        "message": error.to_string(),
        "kind": "Query",
    });
    response::json(StatusCode::BAD_REQUEST, &body)
}
