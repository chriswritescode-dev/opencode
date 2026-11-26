use opencode_contract::codec::{Decode, Path};
use opencode_contract::generated::PermissionRule;
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::{
    SnapshotOptions, ToolCall, ToolContext, ToolNamespace, ToolOptions, ToolRegistration, ToolRegistry, ToolSpec,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{echo, text_schema};

#[derive(Deserialize)]
struct DefinitionsCase {
    name: String,
    full_codemode: bool,
    permissions: Vec<Value>,
    hidden: Vec<String>,
    definitions: Vec<Value>,
}

#[derive(Deserialize)]
struct SnapshotCase {
    name: String,
    namespaces: Vec<NamespaceSpec>,
    adds: Vec<AddSpec>,
    removes: Vec<String>,
    permissions: Vec<Value>,
    hidden: Vec<String>,
    calls: Vec<CallSpec>,
    expected: Expected,
}

#[derive(Deserialize)]
struct NamespaceSpec {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct AddSpec {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    codemode: Option<bool>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct CallSpec {
    name: String,
    input: Value,
}

#[derive(Deserialize)]
struct Expected {
    definitions: Vec<Value>,
    ids: Vec<String>,
    calls: Vec<ExpectedCall>,
}

#[derive(Deserialize)]
struct ExpectedCall {
    name: String,
    status: String,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    message: Option<String>,
}

#[tokio::test]
async fn builtin_definitions_match_the_pinned_typescript() {
    let cases: Vec<DefinitionsCase> =
        serde_json::from_str(include_str!("../corpus/definitions.json")).expect("the definitions corpus is valid JSON");
    for case in cases {
        let registry = ToolRegistry::new();
        let _builtins =
            builtin::activate(&registry, &BuiltinDeps { full_codemode: case.full_codemode, ..BuiltinDeps::default() })
                .await;
        let permissions = decode_rules(&case.permissions);
        let options = SnapshotOptions { hidden: case.hidden.into_iter().collect(), ..SnapshotOptions::default() };
        let snapshot = registry.snapshot(&permissions, &options).await.expect("the snapshot builds");
        assert_eq!(
            serde_json::to_string(&snapshot.definitions).expect("definitions serialise"),
            serde_json::to_string(&case.definitions).expect("expected definitions serialise"),
            "case {}",
            case.name
        );
    }
}

#[tokio::test]
async fn snapshot_cases_match_the_pinned_typescript() {
    let cases: Vec<SnapshotCase> =
        serde_json::from_str(include_str!("../corpus/snapshot.json")).expect("the snapshot corpus is valid JSON");
    for case in cases {
        let registry = ToolRegistry::new();
        let namespaces: Vec<ToolNamespace> = case
            .namespaces
            .iter()
            .map(|namespace| ToolNamespace { name: namespace.name.clone(), description: namespace.description.clone() })
            .collect();
        let adds: Vec<ToolRegistration> = case.adds.iter().map(registration).collect();
        let mut _registrations = vec![
            registry
                .transform(move |editor| {
                    for namespace in &namespaces {
                        editor.namespace(namespace.clone());
                    }
                    for add in &adds {
                        editor.add(add.clone());
                    }
                })
                .await,
        ];
        if !case.removes.is_empty() {
            let removes = case.removes.clone();
            _registrations.push(
                registry
                    .transform(move |editor| {
                        for id in &removes {
                            editor.remove(id);
                        }
                    })
                    .await,
            );
        }
        let permissions = decode_rules(&case.permissions);
        let options = SnapshotOptions { hidden: case.hidden.into_iter().collect(), ..SnapshotOptions::default() };
        let snapshot = registry.snapshot(&permissions, &options).await.expect("the snapshot builds");

        assert_eq!(
            serde_json::to_string(&snapshot.definitions).expect("definitions serialise"),
            serde_json::to_string(&case.expected.definitions).expect("expected definitions serialise"),
            "definitions for {}",
            case.name
        );

        let ids: Vec<String> =
            registry.list().await.expect("the registry lists").into_iter().map(|tool| tool.key).collect();
        assert_eq!(ids, case.expected.ids, "ids for {}", case.name);

        assert_eq!(case.calls.len(), case.expected.calls.len(), "call count for {}", case.name);
        for (call, expected) in case.calls.iter().zip(&case.expected.calls) {
            assert_eq!(call.name, expected.name);
            let result = snapshot
                .execute(
                    ToolCall { id: format!("call-{}", call.name), name: call.name.clone(), input: call.input.clone() },
                    &ToolContext::default(),
                )
                .await;
            match (result, expected.status.as_str()) {
                (Ok(output), "completed") => {
                    assert_eq!(Some(&output.output), expected.output.as_ref(), "output for {}", case.name)
                }
                (Err(error), "error") => {
                    assert_eq!(Some(&error.message), expected.message.as_ref(), "message for {}", case.name)
                }
                (result, status) => panic!("case {} call {}: expected {status}, got {result:?}", case.name, call.name),
            }
        }
    }
}

fn registration(add: &AddSpec) -> ToolRegistration {
    ToolRegistration {
        spec: ToolSpec {
            name: add.name.clone(),
            description: Some(add.description.clone().unwrap_or_else(|| "Return text".to_string())),
            input_schema: match add.input.as_deref() {
                Some("missing") => None,
                _ => Some(text_schema()),
            },
            output_schema: match add.output.as_deref() {
                Some("missing") => None,
                _ => Some(text_schema()),
            },
            ..ToolSpec::default()
        },
        options: ToolOptions {
            namespace: add.namespace.clone(),
            permission: add.permission.clone(),
            codemode: Value::Bool(add.codemode.unwrap_or(false)),
        },
        runtime: echo(add.text.clone().unwrap_or_else(|| add.name.clone())),
    }
}

fn decode_rules(values: &[Value]) -> Vec<PermissionRule> {
    values
        .iter()
        .map(|value| PermissionRule::decode(value, &mut Path::new()).expect("the permission rule decodes"))
        .collect()
}
