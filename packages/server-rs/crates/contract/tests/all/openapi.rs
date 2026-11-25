use serde_json::Value;
use std::collections::BTreeSet;

const METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

fn document() -> Value {
    serde_json::from_str(opencode_contract::OPENAPI_JSON).unwrap()
}

#[test]
fn the_document_is_the_pinned_upstream_one() {
    let document = document();
    let paths = document["paths"].as_object().unwrap();
    let mut method_entries = 0;
    let mut tags = BTreeSet::new();
    for (_, item) in paths {
        for (method, operation) in item.as_object().unwrap() {
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            method_entries += 1;
            for tag in operation["tags"].as_array().unwrap() {
                tags.insert(tag.as_str().unwrap());
            }
        }
    }
    assert_eq!(method_entries, 188);
    assert_eq!(paths.len(), 152);
    assert_eq!(tags.len(), 32);
    assert_eq!(document["components"]["schemas"].as_object().unwrap().len(), 542);
}

fn operation_ids(document: &Value) -> BTreeSet<&str> {
    document["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|item| item.as_object().unwrap().values())
        .filter_map(|operation| operation["operationId"].as_str())
        .collect()
}

#[test]
fn the_served_document_matches_the_pinned_operations() {
    assert!(!opencode_contract::SERVED_OPENAPI_JSON.contains('\n'));
    let served: Value = serde_json::from_str(opencode_contract::SERVED_OPENAPI_JSON).unwrap();
    assert_eq!(served["info"]["title"].as_str(), Some("opencode HttpApi"));
    assert_eq!(operation_ids(&served), operation_ids(&document()));
}
