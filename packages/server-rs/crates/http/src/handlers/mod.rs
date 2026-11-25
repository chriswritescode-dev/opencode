use opencode_contract::codec::{Decode, Encode, decode_json, encode_json};
use opencode_contract::generated::{AbsolutePath, LocationPublicRef};

use crate::services::LocationServices;

pub mod config;
pub mod debug;
pub mod defect;
pub mod event;
pub mod form;
pub mod invalid;
pub mod location;
pub mod permission;
pub mod project;
pub mod registry;
pub mod session_log;

pub(crate) fn location_ref(services: &LocationServices) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(services.info.directory.clone()) }
}

pub(crate) fn bridge<T: Encode + ?Sized, U: Decode>(value: &T) -> U {
    decode_json(&encode_json(value)).expect("a generated contract value round trips")
}
