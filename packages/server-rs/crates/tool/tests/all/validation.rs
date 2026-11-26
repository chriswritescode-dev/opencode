use std::collections::HashMap;

use opencode_tool::generated::definitions;
use opencode_tool::input;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    tool: String,
    input: Value,
    message: Option<String>,
}

#[test]
fn validation_corpus_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../corpus/validation.json")).expect("the validation corpus is valid JSON");
    let schemas: HashMap<String, Value> = definitions(false)
        .iter()
        .map(|entry| (entry.definition.name.clone(), entry.definition.input_schema.clone()))
        .collect();
    for case in cases {
        let schema = schemas
            .get(&case.tool)
            .unwrap_or_else(|| panic!("the validation corpus names the known tool {}", case.tool));
        let outcome = input::decode(&case.tool, schema, &case.input);
        match case.message {
            Some(expected) => assert_eq!(outcome.expect_err("the input is invalid").message, expected),
            None => {
                outcome.expect("the input is valid");
            }
        }
    }
}

#[test]
fn strips_unknown_properties() {
    let schema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    });
    let decoded =
        input::decode("read", &schema, &json!({ "path": "a", "extra": true })).expect("excess properties are ignored");
    assert_eq!(decoded, json!({ "path": "a" }));
}

#[test]
fn accepts_integral_floats_as_integers() {
    let schema = json!({
        "type": "object",
        "properties": { "limit": { "type": "integer", "minimum": 0 } },
        "required": ["limit"],
        "additionalProperties": false
    });
    assert!(input::decode("read", &schema, &json!({ "limit": 2.0 })).is_ok());
    assert_eq!(
        input::decode("read", &schema, &json!({ "limit": 2.5 })).expect_err("a fraction is not an integer").message,
        "Invalid arguments for tool \"read\":\n- limit: Expected an integer\n\nArguments provided:\n{\n  \"limit\": 2.5\n}\n\nUpdate the arguments and call the tool again."
    );
}

#[tokio::test]
async fn execute_reports_invalid_arguments() {
    let registry = opencode_tool::ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(opencode_tool::ToolRegistration {
                spec: opencode_tool::ToolSpec {
                    name: "echo".to_string(),
                    description: Some("Return text".to_string()),
                    input_schema: Some(crate::text_schema()),
                    ..opencode_tool::ToolSpec::default()
                },
                options: opencode_tool::ToolOptions {
                    codemode: serde_json::Value::Bool(false),
                    ..opencode_tool::ToolOptions::default()
                },
                runtime: crate::echo("echo"),
            })
        })
        .await;
    let snapshot =
        registry.snapshot(&[], &opencode_tool::SnapshotOptions::default()).await.expect("the registry loads");
    let error = snapshot
        .execute(
            opencode_tool::ToolCall {
                id: "call_echo".to_string(),
                name: "echo".to_string(),
                input: json!({ "text": 1 }),
            },
            &opencode_tool::ToolContext::default(),
        )
        .await
        .expect_err("the input is invalid");
    assert_eq!(
        error.message,
        "Invalid arguments for tool \"echo\":\n- text: Expected string\n\nArguments provided:\n{\n  \"text\": 1\n}\n\nUpdate the arguments and call the tool again."
    );
}
