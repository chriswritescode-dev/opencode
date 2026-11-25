use std::fs;

use opencode_registry::markdown;
use serde_json::Value;

#[test]
fn corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/markdown.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert_eq!(cases.len(), 32, "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        let input = case["input"].as_str().expect("case input is a string");
        let output = &case["output"];
        let parsed = markdown::parse(input);

        if output.is_null() {
            assert!(
                parsed.is_none(),
                "case {name}: expected no frontmatter, got {:?}",
                parsed.map(|parsed| Value::Object(parsed.data))
            );
            continue;
        }

        let parsed = parsed.unwrap_or_else(|| panic!("case {name}: expected frontmatter, got none"));
        let data = Value::Object(parsed.data);
        let expected_data = &output["data"];
        assert!(
            equivalent(&data, expected_data),
            "case {name}: data mismatch\nexpected: {expected_data}\nactual: {data}"
        );
        let content = Value::String(parsed.content);
        let expected_content = &output["content"];
        assert!(
            equivalent(&content, expected_content),
            "case {name}: content mismatch\nexpected: {expected_content}\nactual: {content}"
        );
    }
}

fn equivalent(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected)) => actual.as_f64() == expected.as_f64(),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual.iter().zip(expected).all(|(actual, expected)| equivalent(actual, expected))
        }
        (Value::Object(actual), Value::Object(expected)) => {
            actual.len() == expected.len()
                && expected
                    .iter()
                    .all(|(key, expected)| actual.get(key).is_some_and(|actual| equivalent(actual, expected)))
        }
        (actual, expected) => actual == expected,
    }
}
