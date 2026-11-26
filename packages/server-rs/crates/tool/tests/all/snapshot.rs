use std::collections::{BTreeMap, BTreeSet};

use opencode_contract::generated::PermissionEffect;
use opencode_tool::policy::{ModelInfo, model_tools};
use opencode_tool::{
    SnapshotOptions, ToolCall, ToolContext, ToolDefinition, ToolOptions, ToolRegistration, ToolRegistry, ToolSpec,
    definition,
};
use serde_json::json;

use crate::{echo, text_schema};

fn model(id: &str) -> ModelInfo {
    ModelInfo { id: id.to_string(), model_id: id.to_string() }
}

fn hidden(model: Option<&ModelInfo>, websearch: bool, full_codemode: bool) -> Vec<String> {
    model_tools(model, websearch, full_codemode).hidden.into_iter().collect()
}

#[test]
fn hides_websearch_without_a_provider() {
    assert_eq!(hidden(None, false, false), ["websearch"]);
    assert!(hidden(None, true, false).is_empty());
}

#[test]
fn chooses_patch_or_edit_and_write_by_model() {
    for id in ["gpt-5", "gpt-6", "gpt-5.2"] {
        assert_eq!(hidden(Some(&model(id)), true, false), ["edit", "write"]);
    }
    for id in ["gpt-4.1", "openai/gpt-oss-120b", "gpt-4o", "custom-sonnet", "deepseek-reasoner"] {
        assert_eq!(hidden(Some(&model(id)), true, false), ["patch"]);
    }
}

#[test]
fn hides_search_tools_with_full_codemode() {
    assert_eq!(hidden(Some(&model("gpt-5")), true, true), ["edit", "glob", "grep", "webfetch", "write"]);
    assert_eq!(hidden(None, false, true), ["glob", "grep", "webfetch", "websearch"]);
}

#[test]
fn sees_the_model_id_as_well_as_the_id() {
    let model = ModelInfo { id: "azure-dep".to_string(), model_id: "gpt-6".to_string() };
    assert_eq!(hidden(Some(&model), true, false), ["edit", "write"]);
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> opencode_contract::generated::PermissionRule {
    opencode_contract::generated::PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}

#[tokio::test]
async fn filters_wholly_disabled_tools_by_permission_alias() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            for (name, permission) in
                [("question", None), ("bash", None), ("edit", Some("edit")), ("write", Some("edit"))]
            {
                editor.add(ToolRegistration {
                    spec: ToolSpec {
                        name: name.to_string(),
                        description: Some("Return text".to_string()),
                        input_schema: Some(text_schema()),
                        ..ToolSpec::default()
                    },
                    options: ToolOptions {
                        permission: permission.map(str::to_string),
                        codemode: json!(false),
                        ..ToolOptions::default()
                    },
                    runtime: echo(name),
                });
            }
        })
        .await;

    let names = |rules: Vec<opencode_contract::generated::PermissionRule>| {
        let registry = registry.clone();
        async move {
            let snapshot = registry.snapshot(&rules, &SnapshotOptions::default()).await.expect("the snapshot builds");
            snapshot.definitions.into_iter().map(|definition| definition.name).collect::<Vec<_>>()
        }
    };

    assert_eq!(names(vec![rule("question", "*", PermissionEffect::Deny)]).await, ["bash", "edit", "write"]);
    assert_eq!(
        names(vec![rule("*", "*", PermissionEffect::Deny), rule("question", "private", PermissionEffect::Allow)]).await,
        ["question"]
    );
    assert_eq!(
        names(vec![rule("question", "private", PermissionEffect::Allow), rule("*", "*", PermissionEffect::Deny)]).await,
        Vec::<String>::new()
    );
    assert_eq!(names(vec![rule("edit", "*", PermissionEffect::Deny)]).await, ["bash", "question"]);
}

#[tokio::test]
async fn hidden_names_remove_tools_from_the_snapshot() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(ToolRegistration {
                spec: ToolSpec {
                    name: "bash".to_string(),
                    description: Some("Bash".to_string()),
                    ..ToolSpec::default()
                },
                options: ToolOptions { codemode: json!(false), ..ToolOptions::default() },
                runtime: echo("bash"),
            })
        })
        .await;
    let options = SnapshotOptions { hidden: BTreeSet::from(["bash".to_string()]), ..SnapshotOptions::default() };
    let snapshot = registry.snapshot(&[], &options).await.expect("the snapshot builds");
    assert!(snapshot.definitions.is_empty());
    let error = snapshot
        .execute(
            ToolCall { id: "call".to_string(), name: "bash".to_string(), input: json!({}) },
            &ToolContext::default(),
        )
        .await
        .expect_err("a hidden tool is unavailable");
    assert_eq!(
        error.message,
        "No tool named \"bash\" is currently available. Please use a tool from the available tool list."
    );
}

#[tokio::test]
async fn rejects_tools_removed_from_the_request_definitions() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(ToolRegistration {
                spec: ToolSpec {
                    name: "bash".to_string(),
                    description: Some("Bash".to_string()),
                    ..ToolSpec::default()
                },
                options: ToolOptions { codemode: json!(false), ..ToolOptions::default() },
                runtime: echo("bash"),
            })
        })
        .await;
    let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
    let definitions = BTreeMap::<String, ToolDefinition>::new();
    let error = snapshot
        .execute_with(
            ToolCall { id: "call".to_string(), name: "bash".to_string(), input: json!({}) },
            &ToolContext::default(),
            Some(&definitions),
        )
        .await
        .expect_err("the request dropped the tool");
    assert_eq!(error.message, "Tool is not available for this request: bash");
}

#[test]
fn definition_registration_checks_names() {
    let options = ToolOptions::default();
    let spec = ToolSpec { name: "a b".to_string(), description: Some("Spaces".to_string()), ..ToolSpec::default() };
    let (key, definition) = definition::register(&spec, &options).expect("the name normalises");
    assert_eq!(key, "a_b");
    assert_eq!(definition.name, "a_b");
    assert_eq!(definition.input_schema, json!({}));
}
