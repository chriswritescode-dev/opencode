use std::fs;
use std::sync::Arc;

use opencode_contract::codec::Decode;
use opencode_contract::generated::AgentInfo;
use opencode_registry::agent::{AgentService, Selection};
use opencode_registry::builtin;
use opencode_registry::events::RegistryEvents;
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

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

#[tokio::test]
async fn agents_corpus_matches_expected_output() {
    let path = format!("{}/tests/corpus/agents.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).expect("corpus file is readable");
    let home = crate::TempDir::new();
    let home_path = home.path().to_path_buf();
    let text = text.replace("{{home}}", &home_path.to_string_lossy());
    let cases: Vec<Value> = serde_json::from_str(&text).expect("corpus is a JSON array");
    assert!(!cases.is_empty(), "corpus case count");

    for case in &cases {
        let name = case["name"].as_str().expect("case name is a string");
        let service = AgentService::new(Arc::new(NoEvents));
        let _registration = service
            .transform({
                let home = home_path.clone();
                move |editor| {
                    builtin::agent::agent(editor);
                    builtin::agent::plan(editor, &home);
                }
            })
            .await;

        let list = decode_agents(&case["list"]);
        assert_eq!(service.list().await.expect("state reads"), list, "case {name}: list");
        let select = service.select(None).await.expect("state reads");
        assert_selection(&select, &case["select"], name);
        let resolve = service.resolve(None).await.expect("state reads");
        assert_eq!(resolve, Some(decode_agent(&case["resolve"])), "case {name}: resolve");
    }
}

fn decode_agents(value: &Value) -> Vec<AgentInfo> {
    value
        .as_array()
        .expect("agents are an array")
        .iter()
        .map(|item| AgentInfo::decode(item, &mut opencode_contract::codec::Path::new()).expect("agent decodes"))
        .collect()
}

fn decode_agent(value: &Value) -> AgentInfo {
    AgentInfo::decode(value, &mut opencode_contract::codec::Path::new()).expect("agent decodes")
}

fn assert_selection(actual: &Selection, expected: &Value, name: &str) {
    assert_eq!(actual.id, expected["id"].as_str().expect("selection id is a string"), "case {name}: select id");
    assert_eq!(actual.info, Some(decode_agent(&expected["info"])), "case {name}: select info");
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
