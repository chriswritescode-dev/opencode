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

fn collect_error_tags(value: &Value, tags: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                let name = reference.rsplit('/').next().unwrap();
                let tag = name.strip_suffix("Encoded").unwrap_or_else(|| panic!("unexpected response schema {name}"));
                tags.insert(tag.to_owned());
            }
            for item in object.values() {
                collect_error_tags(item, tags);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_error_tags(item, tags);
            }
        }
        _ => {}
    }
}

#[test]
fn every_route_reports_the_pinned_openapi_responses() {
    let document: Value = serde_json::from_str(OPENAPI_JSON).unwrap();
    let mut checked = 0;
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, key) in METHODS {
            let Some(operation) = item.as_object().unwrap().get(key) else {
                continue;
            };
            checked += 1;
            let operation_id = operation["operationId"].as_str().unwrap();
            let route = route(operation_id).unwrap_or_else(|| panic!("no route for operation {operation_id}"));
            assert_eq!(route.method, method, "wrong method for operation {operation_id}");
            assert_eq!(route.path, path.as_str(), "wrong path for operation {operation_id}");

            let responses = operation["responses"].as_object().unwrap();
            let success: Vec<u16> = responses
                .keys()
                .filter_map(|status| status.parse::<u16>().ok())
                .filter(|status| (200..300).contains(status))
                .collect();
            assert_eq!(success.len(), 1, "operation {operation_id} has success statuses {success:?}");
            assert_eq!(route.success_status, success[0], "wrong success status for operation {operation_id}");

            let mut expected: BTreeSet<(String, u16)> = BTreeSet::new();
            for (status, response) in responses {
                let Ok(status) = status.parse::<u16>() else {
                    continue;
                };
                if (200..300).contains(&status) {
                    continue;
                }
                let mut tags = BTreeSet::new();
                collect_error_tags(response, &mut tags);
                for tag in tags {
                    expected.insert((tag, status));
                }
            }
            let actual: BTreeSet<(String, u16)> =
                route.errors.iter().map(|(tag, status)| ((*tag).to_owned(), *status)).collect();
            assert_eq!(
                actual,
                expected,
                "wrong errors for operation {operation_id}: document only {:?}, routes only {:?}",
                expected.difference(&actual).collect::<Vec<_>>(),
                actual.difference(&expected).collect::<Vec<_>>()
            );
        }
    }
    assert_eq!(checked, 178, "expected 178 operations in the document, found {checked}");
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
