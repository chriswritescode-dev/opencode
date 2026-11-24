use std::fmt::Display;

use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{UnknownError, UnknownErrorTag};

use crate::response;

pub fn response(context: &str, error: impl Display) -> response::Response {
    let reference = reference();
    log::error!("{context} ref={reference}: {error}");
    response::bytes(
        StatusCode::INTERNAL_SERVER_ERROR,
        "application/json",
        encode_json(&UnknownError {
            tag: UnknownErrorTag::UnknownError,
            message: "Server failed to handle the request. Check server logs for details.".to_owned(),
            r#ref: Some(Some(reference)),
        }),
    )
}

pub fn reference() -> String {
    let token = rand::random::<[u8; 4]>();
    let hex = token.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("err_{hex}")
}
