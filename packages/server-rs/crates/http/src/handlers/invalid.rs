use http::StatusCode;

use crate::response;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Params,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Query => "Query",
            Kind::Params => "Params",
        }
    }
}

pub fn request(kind: Kind, error: opencode_contract::codec::DecodeError) -> response::Response {
    let body = serde_json::json!({
        "_tag": "InvalidRequestError",
        "message": error.to_string(),
        "kind": kind.label(),
    });
    response::json(StatusCode::BAD_REQUEST, &body)
}
