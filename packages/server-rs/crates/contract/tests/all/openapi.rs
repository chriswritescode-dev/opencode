use serde_json::{Value, json};
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
    assert_eq!(method_entries, 178);
    assert_eq!(paths.len(), 146);
    assert_eq!(tags.len(), 32);
    assert_eq!(document["components"]["schemas"].as_object().unwrap().len(), 527);
}

#[test]
fn the_document_keeps_what_the_newer_checkout_removed() {
    let document = document();
    let mcp_events = &document["components"]["schemas"]["Config.InfoEncoded"]["properties"]["experimental"]["properties"]
        ["mcp_events"];
    assert_eq!(mcp_events, &json!({ "type": "boolean" }));
}
