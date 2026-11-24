use opencode_contract::generated::{InvalidRequestErrorTag, SessionID, SessionNotFoundErrorTag};
use opencode_contract::request::RawRequest;
use opencode_location::location::LocationRef;
use opencode_location::resolve::{SessionLocationError, SessionLookup, request_ref, resolve_session_location};

struct FakeSessions {
    entries: Vec<(&'static str, LocationRef)>,
    error: Option<String>,
}

impl SessionLookup for FakeSessions {
    fn location(&self, id: &SessionID) -> Result<Option<LocationRef>, String> {
        if let Some(message) = &self.error {
            return Err(message.clone());
        }
        Ok(self.entries.iter().find(|(key, _)| *key == id.0).map(|(_, location)| location.clone()))
    }
}

fn sessions() -> FakeSessions {
    FakeSessions { entries: vec![("ses_found", LocationRef::new("/session/dir", None))], error: None }
}

fn raw_request(query: &str, headers: Vec<(String, String)>) -> RawRequest {
    RawRequest { path_params: Vec::new(), query: query.to_owned(), headers, body: Vec::new() }
}

fn directory_header(value: &str) -> Vec<(String, String)> {
    vec![("x-opencode-directory".to_owned(), value.to_owned())]
}

#[test]
fn query_directory_wins_over_the_header() {
    let request = raw_request("location[directory]=/from/query", directory_header("/from/header"));
    assert_eq!(request_ref(&request, "/start"), LocationRef::new("/from/query", None));
}

#[test]
fn empty_query_directory_falls_through_to_the_header() {
    let request = raw_request("location[directory]=", directory_header("/from/header"));
    assert_eq!(request_ref(&request, "/start"), LocationRef::new("/from/header", None));
}

#[test]
fn header_directory_is_percent_decoded() {
    let request = raw_request("", directory_header("/tmp/my%20dir"));
    assert_eq!(request_ref(&request, "/start"), LocationRef::new("/tmp/my dir", None));
}

#[test]
fn header_directory_keeps_a_bad_escape() {
    let request = raw_request("", directory_header("/tmp/%ZZ"));
    assert_eq!(request_ref(&request, "/start"), LocationRef::new("/tmp/%ZZ", None));
}

#[test]
fn missing_directory_uses_the_start_directory() {
    let request = raw_request("", Vec::new());
    assert_eq!(request_ref(&request, "/start"), LocationRef::new("/start", None));
}

#[test]
fn found_session_returns_its_location() {
    let lookup = sessions();
    assert_eq!(resolve_session_location(&lookup, "ses_found"), Ok(LocationRef::new("/session/dir", None)));
}

#[test]
fn invalid_session_id_is_an_invalid_request() {
    let lookup = sessions();
    let error = resolve_session_location(&lookup, "nope").expect_err("invalid session id");
    let SessionLocationError::InvalidRequest(error) = error else {
        panic!("unexpected error: {error:?}");
    };
    assert_eq!(error.tag, InvalidRequestErrorTag::InvalidRequestError);
    assert_eq!(error.message, "Invalid session ID");
    assert_eq!(error.kind, None);
    assert_eq!(error.field, Some(Some("sessionID".to_owned())));
}

#[test]
fn missing_session_is_not_found() {
    let lookup = sessions();
    let error = resolve_session_location(&lookup, "ses_missing").expect_err("missing session");
    let SessionLocationError::SessionNotFound(error) = error else {
        panic!("unexpected error: {error:?}");
    };
    assert_eq!(error.tag, SessionNotFoundErrorTag::SessionNotFoundError);
    assert_eq!(error.session_id, "ses_missing");
    assert_eq!(error.message, "Session not found: ses_missing");
}

#[test]
fn lookup_failure_is_a_lookup_error() {
    let lookup = FakeSessions { entries: Vec::new(), error: Some("storage offline".to_owned()) };
    let error = resolve_session_location(&lookup, "ses_found").expect_err("lookup failure");
    let SessionLocationError::Lookup(message) = error else {
        panic!("unexpected error: {error:?}");
    };
    assert_eq!(message, "storage offline");
}
