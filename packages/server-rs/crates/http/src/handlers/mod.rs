use opencode_contract::codec::{Decode, Encode, decode_json, encode_json};
use opencode_contract::generated::{AbsolutePath, LocationPublicRef};

use crate::services::LocationServices;

pub mod config;
pub mod credential;
pub mod debug;
pub mod defect;
pub mod event;
pub mod form;
pub mod integration;
pub mod integration_policy;
pub mod invalid;
pub mod job;
pub mod location;
pub mod message;
pub mod model;
pub mod permission;
pub mod plugin;
pub mod project;
pub mod provider;
pub mod registry;
pub mod session;
pub mod session_log;

pub(crate) fn location_ref(services: &LocationServices) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(services.info.directory.clone()) }
}

pub(crate) fn bridge<T: Encode + ?Sized, U: Decode>(value: &T) -> U {
    decode_json(&encode_json(value)).expect("a generated contract value round trips")
}
