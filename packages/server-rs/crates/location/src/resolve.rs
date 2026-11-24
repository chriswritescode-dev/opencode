use opencode_contract::codec::{Decode, Path};
use opencode_contract::generated::{
    InvalidRequestError, InvalidRequestErrorTag, SessionID, SessionNotFoundError, SessionNotFoundErrorTag,
};
use opencode_contract::request::{RawRequest, location_directory};

use crate::location::LocationRef;

pub fn request_ref(request: &RawRequest, start_directory: &str) -> LocationRef {
    let directory = location_directory(request).unwrap_or_else(|| start_directory.to_owned());
    LocationRef::new(directory, None)
}

pub trait SessionLookup {
    fn location(&self, id: &SessionID) -> Result<Option<LocationRef>, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionLocationError {
    InvalidRequest(InvalidRequestError),
    SessionNotFound(SessionNotFoundError),
    Lookup(String),
}

impl std::fmt::Display for SessionLocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionLocationError::InvalidRequest(error) => formatter.write_str(&error.message),
            SessionLocationError::SessionNotFound(error) => formatter.write_str(&error.message),
            SessionLocationError::Lookup(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SessionLocationError {}

pub fn resolve_session_location(lookup: &dyn SessionLookup, raw_id: &str) -> Result<LocationRef, SessionLocationError> {
    let id = SessionID::decode(&serde_json::Value::String(raw_id.to_owned()), &mut Path::new()).map_err(|_| {
        SessionLocationError::InvalidRequest(InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message: "Invalid session ID".to_owned(),
            kind: None,
            field: Some(Some("sessionID".to_owned())),
        })
    })?;
    match lookup.location(&id) {
        Ok(Some(location)) => Ok(location),
        Ok(None) => Err(SessionLocationError::SessionNotFound(SessionNotFoundError {
            tag: SessionNotFoundErrorTag::SessionNotFoundError,
            session_id: id.0.clone(),
            message: format!("Session not found: {}", id.0),
        })),
        Err(message) => Err(SessionLocationError::Lookup(message)),
    }
}
