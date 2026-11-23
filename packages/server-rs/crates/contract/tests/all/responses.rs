use opencode_contract::SuccessKind;
use opencode_contract::generated::ROUTES;
use opencode_contract::generated::corpus_roots;
use opencode_contract::generated::operations::OPERATION_ROUTES;
use serde_json::Value;
use std::collections::BTreeMap;

const CORPUS_JSON: &str = include_str!("../corpus/schemas.json");

fn corpus() -> Value {
    serde_json::from_str(CORPUS_JSON).unwrap()
}

fn typed(success: SuccessKind) -> bool {
    matches!(success, SuccessKind::Json | SuccessKind::Data | SuccessKind::Sse)
}

fn root(operation_id: &str) -> String {
    format!("operation:{operation_id}")
}

#[test]
fn every_operation_module_points_at_its_route() {
    assert_eq!(OPERATION_ROUTES.len(), ROUTES.len(), "expected one module per route");
    for (route, module) in ROUTES.iter().zip(OPERATION_ROUTES) {
        assert_eq!(route.operation_id, module.operation_id, "operation module order mismatch");
        assert_eq!(route.path, module.path);
        assert_eq!(route.success, module.success);
    }
}

#[test]
fn every_typed_success_has_a_response_root() {
    let roots: BTreeMap<&str, _> = corpus_roots().iter().copied().collect();
    let fixture = corpus();
    let fixture = fixture.as_object().unwrap();
    let mut typed_routes = 0;
    for route in ROUTES.iter() {
        let expected = typed(route.success);
        let name = root(route.operation_id);
        assert_eq!(
            roots.contains_key(name.as_str()),
            expected,
            "operation {} has success {:?}",
            route.operation_id,
            route.success
        );
        if !expected {
            continue;
        }
        typed_routes += 1;
        assert!(fixture.contains_key(&name), "corpus is missing {name}");
    }
    assert_eq!(typed_routes, 118, "expected 118 typed responses, found {typed_routes}");
}

#[test]
fn every_typed_response_round_trips() {
    let fixture = corpus();
    let roots: BTreeMap<&str, _> = corpus_roots().iter().copied().collect();
    for route in ROUTES.iter().filter(|route| typed(route.success)) {
        let name = root(route.operation_id);
        let round_trip = roots[name.as_str()];
        let entry = &fixture[&name];
        for sample in entry["valid"].as_array().unwrap() {
            let input = sample["input"].as_str().unwrap();
            let expected = sample["expected"].as_str().unwrap();
            let actual = round_trip(input)
                .unwrap_or_else(|error| panic!("{} input {input} failed to decode: {error}", route.operation_id));
            assert_eq!(actual, expected, "{} input {input}", route.operation_id);
        }
        for input in entry["invalid"].as_array().unwrap() {
            let input = input.as_str().unwrap();
            let result = round_trip(input);
            assert!(result.is_err(), "{} accepted invalid input {input} as {result:?}", route.operation_id);
        }
    }
}
