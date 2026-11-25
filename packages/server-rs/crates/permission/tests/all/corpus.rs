use std::fs;

use opencode_contract::codec::Decode;
use opencode_contract::generated::PermissionRule;
use opencode_permission::rule::{Platform, evaluate_on};

#[test]
fn evaluate_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/evaluate.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert_eq!(cases.len(), 34, "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        let platform = match case["platform"].as_str() {
            Some("macos") => Platform::Macos,
            Some("other") => Platform::Other,
            other => panic!("case {name}: unknown platform {other:?}"),
        };
        let action = case["action"].as_str().expect("case action is a string");
        let resource = case["resource"].as_str().expect("case resource is a string");
        let rulesets = Vec::<Vec<PermissionRule>>::decode(&case["rules"], &mut opencode_contract::codec::Path::new())
            .unwrap_or_else(|error| panic!("case {name}: rules decode: {error}"));
        let expected = PermissionRule::decode(&case["rule"], &mut opencode_contract::codec::Path::new())
            .unwrap_or_else(|error| panic!("case {name}: rule decode: {error}"));
        let slices: Vec<&[PermissionRule]> = rulesets.iter().map(|rules| rules.as_slice()).collect();
        let actual = evaluate_on(platform, action, resource, &slices);
        assert_eq!(actual, expected, "case {name}");
    }
}
