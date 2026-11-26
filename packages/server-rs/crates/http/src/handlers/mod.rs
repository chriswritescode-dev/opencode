use opencode_contract::codec::{Decode, Encode, decode_json, encode_json};
use opencode_contract::generated::{AbsolutePath, LocationPublicRef};
use opencode_contract::request::RawRequest;

use crate::services::LocationServices;

pub mod config;
pub mod debug;
pub mod defect;
pub mod event;
pub mod form;
pub mod fs;
pub mod invalid;
pub mod job;
pub mod location;
pub mod message;
pub mod model;
pub mod permission;
pub mod persistent_pty;
pub mod plugin;
pub mod project;
pub mod pty;
pub mod registry;
pub mod session;
pub mod session_log;
pub mod vcs;
pub mod worktree;

pub(crate) fn header<'a>(raw: &'a RawRequest, name: &str) -> Option<&'a str> {
    raw.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

pub(crate) fn query_value(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
}

pub(crate) fn location_ref(services: &LocationServices) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(services.info.directory.clone()) }
}

pub(crate) fn bridge<T: Encode + ?Sized, U: Decode>(value: &T) -> U {
    decode_json(&encode_json(value)).expect("a generated contract value round trips")
}
