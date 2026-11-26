use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{InvalidRequestError, InvalidRequestErrorTag};

use crate::response;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Params,
    Body,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Query => "Query",
            Kind::Params => "Params",
            Kind::Body => "Payload",
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

pub fn kind(message: impl Into<String>, kind: &str) -> response::Response {
    response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message: message.into(),
            kind: Some(Some(kind.to_owned())),
            field: None,
        }),
    )
}
