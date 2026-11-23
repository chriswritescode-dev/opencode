use http::Method;
use opencode_contract::generated::ROUTES;
use opencode_http::router::find;

fn sample_path(path: &str) -> (String, Vec<(String, String)>) {
    let mut params = Vec::new();
    let segments = path
        .split('/')
        .map(|segment| match segment.strip_prefix('{').and_then(|name| name.strip_suffix('}')) {
            Some(name) => {
                params.push((name.to_owned(), "value".to_owned()));
                "value"
            }
            None if segment == "*" => {
                params.push(("*".to_owned(), "rest".to_owned()));
                "rest"
            }
            None => segment,
        })
        .collect::<Vec<_>>();
    (segments.join("/"), params)
}

#[test]
fn every_route_is_found_with_sample_parameters() {
    for route in &ROUTES {
        let (path, params) = sample_path(route.path);
        let method = Method::from_bytes(route.method.as_str().as_bytes()).expect("a method");
        let found =
            find(&method, &path).unwrap_or_else(|| panic!("{} {} is not routed", route.method.as_str(), route.path));
        assert_eq!(found.route.operation_id, route.operation_id);
        assert_eq!(found.params, params);
    }
}

#[test]
fn static_segments_win_over_parameters() {
    let active = find(&Method::GET, "/api/session/active").expect("session.active");
    assert_eq!(active.route.operation_id, "session.active");
    let parameter = find(&Method::GET, "/api/session/value").expect("session.get");
    assert_eq!(parameter.route.operation_id, "session.get");
}

#[test]
fn head_falls_back_to_get() {
    let found = find(&Method::HEAD, "/api/info").expect("server.info");
    assert_eq!(found.route.operation_id, "server.info");
    let normalized = find(&Method::HEAD, "/API/INFO/").expect("server.info");
    assert_eq!(normalized.route.operation_id, "server.info");
}

#[test]
fn trailing_slashes_are_ignored() {
    let found = find(&Method::GET, "/api/info/").expect("server.info");
    assert_eq!(found.route.operation_id, "server.info");
    let wildcard = find(&Method::GET, "/api/fs/read/a/b/").expect("fs.read");
    assert_eq!(wildcard.params, vec![("*".to_owned(), "a/b".to_owned())]);
}

#[test]
fn duplicate_slashes_are_collapsed() {
    let found = find(&Method::GET, "/api//info").expect("server.info");
    assert_eq!(found.route.operation_id, "server.info");
    let nested = find(&Method::GET, "//api///session//value").expect("session.get");
    assert_eq!(nested.route.operation_id, "session.get");
    assert_eq!(nested.params, vec![("sessionID".to_owned(), "value".to_owned())]);
}

#[test]
fn static_segments_are_case_insensitive_and_parameters_keep_case() {
    let found = find(&Method::GET, "/API/Session/AbC").expect("session.get");
    assert_eq!(found.route.operation_id, "session.get");
    assert_eq!(found.params, vec![("sessionID".to_owned(), "AbC".to_owned())]);
}

#[test]
fn parameters_at_the_length_limit_match() {
    let limit = "a".repeat(100);
    let found = find(&Method::GET, &format!("/api/session/{limit}")).expect("session.get");
    assert_eq!(found.params, vec![("sessionID".to_owned(), limit)]);
    let encoded = find(&Method::GET, &format!("/api/session/{}", "%61".repeat(100))).expect("session.get");
    assert_eq!(encoded.params, vec![("sessionID".to_owned(), "a".repeat(100))]);
}

#[test]
fn parameters_over_the_length_limit_do_not_match() {
    let over = "a".repeat(101);
    assert!(find(&Method::GET, &format!("/api/session/{over}")).is_none());
    assert!(find(&Method::GET, &format!("/api/session/{}", "%61".repeat(101))).is_none());
}

#[test]
fn methods_must_match() {
    assert!(find(&Method::POST, "/api/info").is_none());
    assert!(find(&Method::DELETE, "/api/info").is_none());
}

#[test]
fn parameters_are_percent_decoded() {
    let found = find(&Method::GET, "/api/session/a%20b").expect("session.get");
    assert_eq!(found.route.operation_id, "session.get");
    assert_eq!(found.params, vec![("sessionID".to_owned(), "a b".to_owned())]);
}

#[test]
fn wildcards_capture_the_rest() {
    let found = find(&Method::GET, "/api/fs/read/a/b").expect("fs.read");
    assert_eq!(found.route.operation_id, "fs.read");
    assert_eq!(found.params, vec![("*".to_owned(), "a/b".to_owned())]);
    assert!(find(&Method::GET, "/api/fs/read").is_none());
    assert!(find(&Method::GET, "/api/fs/read/").is_none());
}

#[test]
fn too_many_segments_do_not_match() {
    assert!(find(&Method::GET, "/api/session/value/extra").is_none());
}

#[test]
fn unknown_paths_do_not_match() {
    assert!(find(&Method::GET, "/api/nope").is_none());
}
