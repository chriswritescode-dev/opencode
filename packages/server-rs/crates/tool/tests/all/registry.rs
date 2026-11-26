use opencode_tool::{ToolOptions, ToolRegistration, ToolRegistry, ToolSpec, definition};

use crate::{echo, text_schema};

fn tool(name: &str, text: &str) -> ToolRegistration {
    ToolRegistration {
        spec: ToolSpec {
            name: name.to_string(),
            description: Some("Return text".to_string()),
            input_schema: Some(text_schema()),
            ..ToolSpec::default()
        },
        options: ToolOptions { codemode: serde_json::Value::Bool(false), ..ToolOptions::default() },
        runtime: echo(text),
    }
}

#[test]
fn flattens_and_normalizes_effective_names() {
    assert_eq!(definition::normalized_name("echo.tool"), "echo_tool");
    assert_eq!(definition::effective_name(None, "echo.tool"), "echo_tool");
    assert_eq!(definition::effective_name(Some("acme"), "echo"), "acme_echo");
    assert_eq!(definition::effective_name(Some("opencode.session"), "move"), "opencode_session_move");
}

#[test]
fn rejects_invalid_names_and_namespaces() {
    assert!(definition::namespace_error("slack..admin").is_some());
    assert!(definition::namespace_error("123._private.-tools").is_none());
    assert!(definition::namespace_error("").is_some());

    let spec = ToolSpec { name: String::new(), description: Some("empty".to_string()), ..ToolSpec::default() };
    assert_eq!(definition::register(&spec, &ToolOptions::default()).unwrap_err().message, "Invalid tool name: ");
}

#[tokio::test]
async fn keeps_the_latest_valid_registration_of_a_key() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(tool("before", "before"));
            editor.add(tool("", "invalid"));
            editor.add(tool("echo.tool", "first"));
            editor.add(tool("echo_tool", "last"));
            editor.add(tool("execute", "reserved"));
        })
        .await;

    let snapshot = registry.snapshot(&[], &Default::default()).await.expect("the snapshot builds");
    let names: Vec<&str> = snapshot.definitions.iter().map(|definition| definition.name.as_str()).collect();
    assert_eq!(names, ["before", "echo_tool"]);

    let result = snapshot
        .execute(
            opencode_tool::ToolCall {
                id: "call".to_string(),
                name: "echo_tool".to_string(),
                input: serde_json::json!({ "text": "last" }),
            },
            &Default::default(),
        )
        .await
        .expect("the latest registration executes");
    assert_eq!(result.output, serde_json::json!({ "text": "last" }));
}

#[tokio::test]
async fn updates_keep_the_name_and_namespace_and_ignore_invalid_edits() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(tool("echo", "original"));
            editor.update("echo", |registration| {
                registration.spec.description = Some("Updated".to_string());
                registration.spec.name = "renamed".to_string();
                registration.options.namespace = Some("acme".to_string());
            });
        })
        .await;

    let registered = registry.get("echo").await.expect("the registry reads").expect("the tool exists");
    assert_eq!(registered.name, "echo");
    assert_eq!(registered.namespace, None);
    assert_eq!(registered.definition.name, "echo");
    assert_eq!(registered.definition.description, "Updated");

    let _update = registry
        .transform(|editor| {
            editor.update("echo", |registration| {
                registration.spec.description = None;
            });
        })
        .await;
    let registered = registry.get("echo").await.expect("the registry reads").expect("the tool exists");
    assert_eq!(registered.definition.description, "Updated");
}

#[tokio::test]
async fn missing_input_becomes_an_empty_schema_and_removal_clears_the_key() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor.add(ToolRegistration {
                spec: ToolSpec {
                    name: "direct".to_string(),
                    description: Some("Direct".to_string()),
                    ..ToolSpec::default()
                },
                options: ToolOptions { codemode: serde_json::Value::Bool(false), ..ToolOptions::default() },
                runtime: echo("direct"),
            });
        })
        .await;
    let registered = registry.get("direct").await.expect("the registry reads").expect("the tool exists");
    assert_eq!(registered.definition.input_schema, serde_json::json!({}));

    let _removal = registry.transform(|editor| editor.remove("direct")).await;
    assert!(registry.get("direct").await.expect("the registry reads").is_none());
    assert!(registry.list().await.expect("the registry lists").is_empty());
}

#[tokio::test]
async fn records_invalid_registrations_without_dropping_healthy_tools() {
    let registry = ToolRegistry::new();
    let _registration = registry
        .transform(|editor| {
            editor
                .namespace(opencode_tool::ToolNamespace { name: "invalid..namespace".to_string(), description: None });
            editor.add(tool("first", "first"));
            editor.add(ToolRegistration {
                options: ToolOptions {
                    namespace: Some("invalid..namespace".to_string()),
                    codemode: serde_json::Value::Bool(false),
                    ..ToolOptions::default()
                },
                ..tool("second", "second")
            });
        })
        .await;

    let ids: Vec<String> =
        registry.list().await.expect("the registry lists").into_iter().map(|tool| tool.key).collect();
    assert_eq!(ids, ["first"]);
    let errors = registry.errors().await.expect("the registry reads errors");
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].kind, "namespace");
    assert_eq!(errors[1].kind, "tool");
}
