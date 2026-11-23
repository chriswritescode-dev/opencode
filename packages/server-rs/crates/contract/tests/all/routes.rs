use opencode_contract::OPENAPI_JSON;
use opencode_contract::generated::ROUTES;
use opencode_contract::route::{BodyKind, Method, Scope, Stability, SuccessKind, route};
use serde_json::Value;
use std::collections::BTreeSet;

const METHODS: [(Method, &str); 8] = [
    (Method::Get, "get"),
    (Method::Put, "put"),
    (Method::Post, "post"),
    (Method::Delete, "delete"),
    (Method::Options, "options"),
    (Method::Head, "head"),
    (Method::Patch, "patch"),
    (Method::Trace, "trace"),
];

fn count_of(predicate: impl Fn(&opencode_contract::Route) -> bool) -> usize {
    ROUTES.iter().filter(|route| predicate(route)).count()
}

#[test]
fn the_route_table_matches_the_pinned_openapi() {
    assert_eq!(ROUTES.len(), 178, "expected 178 routes, found {}", ROUTES.len());

    let operation_ids: BTreeSet<&str> = ROUTES.iter().map(|route| route.operation_id).collect();
    assert_eq!(operation_ids.len(), 178, "expected 178 unique operation ids, found {}", operation_ids.len());

    let targets: BTreeSet<(&str, &str)> = ROUTES.iter().map(|route| (route.method.as_str(), route.path)).collect();
    assert_eq!(targets.len(), 178, "expected 178 unique method and path pairs, found {}", targets.len());

    let document: Value = serde_json::from_str(OPENAPI_JSON).unwrap();
    let mut operations = 0;
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, key) in METHODS {
            let Some(operation) = item.as_object().unwrap().get(key) else {
                continue;
            };
            operations += 1;
            let operation_id = operation["operationId"].as_str().unwrap();
            let route = route(operation_id).unwrap_or_else(|| panic!("no route for operation {operation_id}"));
            assert_eq!(route.method, method, "wrong method for operation {operation_id}");
            assert_eq!(route.method.as_str(), key.to_uppercase(), "wrong method for operation {operation_id}");
            assert_eq!(route.path, path.as_str(), "wrong path for operation {operation_id}");
            assert_eq!(route.tag, operation["tags"][0].as_str().unwrap(), "wrong tag for operation {operation_id}");
        }
    }
    assert_eq!(operations, 178, "expected 178 operations in the document, found {operations}");
}

#[test]
fn the_route_table_has_the_expected_shape() {
    let experimental = count_of(|route| route.stability == Stability::Experimental);
    assert_eq!(experimental, 62, "expected 62 experimental routes, found {experimental}");
    let stable = count_of(|route| route.stability == Stability::Stable);
    assert_eq!(stable, 116, "expected 116 stable routes, found {stable}");

    let websocket = count_of(|route| route.success == SuccessKind::WebSocket);
    assert_eq!(websocket, 4, "expected 4 websocket routes, found {websocket}");
    let sse = count_of(|route| route.success == SuccessKind::Sse);
    assert_eq!(sse, 2, "expected 2 sse routes, found {sse}");
    let empty = count_of(|route| route.success == SuccessKind::Empty);
    assert_eq!(empty, 52, "expected 52 empty success routes, found {empty}");

    let binary = count_of(|route| route.body == BodyKind::Binary);
    assert_eq!(binary, 1, "expected 1 binary body route, found {binary}");

    let location = count_of(|route| route.scope == Scope::Location);
    assert_eq!(location, 66, "expected 66 location scoped routes, found {location}");
    let session = count_of(|route| route.scope == Scope::Session);
    assert_eq!(session, 10, "expected 10 session scoped routes, found {session}");
    let form = count_of(|route| route.scope == Scope::Form);
    assert_eq!(form, 5, "expected 5 form scoped routes, found {form}");
    let global = count_of(|route| route.scope == Scope::Global);
    assert_eq!(global, 97, "expected 97 global scoped routes, found {global}");

    let wildcards = count_of(|route| route.wildcard);
    assert!(wildcards >= 1, "expected at least one wildcard route, found {wildcards}");
    assert!(route("fs.read").expect("fs.read route is missing").wildcard, "expected fs.read to be a wildcard route");
}
