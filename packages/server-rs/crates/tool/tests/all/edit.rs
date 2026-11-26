use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, Authorizer, Target};
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FileWriter, FormatHook, NoFormat};
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::edit::EditDeps;
use opencode_tool::{SnapshotOptions, ToolCall, ToolContext, ToolOutput, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{Fixture, RecordingAuthorizer, file_tree, materialize, replace_placeholders, unique_directory};

#[derive(Deserialize)]
struct EditCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    call: Value,
    #[serde(default)]
    permission: PermissionSpec,
    expected: Value,
}

#[derive(Default, Deserialize)]
struct PermissionSpec {
    #[serde(default)]
    allow: Option<bool>,
    #[serde(default, rename = "deniedResources")]
    denied_resources: Vec<String>,
    #[serde(default, rename = "deniedActions")]
    denied_actions: Vec<String>,
    #[serde(default, rename = "readAllowed")]
    read_allowed: Option<bool>,
}

#[tokio::test]
async fn edit_corpus_matches() {
    let cases: Vec<EditCase> =
        serde_json::from_str(include_str!("../corpus/edit.json")).expect("the edit corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("edit-{index}"));
        let outside = unique_directory(&format!("edit-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);
        let root_text = root.to_string_lossy().into_owned();

        let mut recording =
            RecordingAuthorizer::new(case.permission.allow.unwrap_or(true), case.permission.denied_resources.clone());
        recording.deny_actions(case.permission.denied_actions.clone());
        recording.read_allowed = case.permission.read_allowed.unwrap_or(true);
        let recording = Arc::new(recording);
        let authorizer: Arc<dyn Authorizer> = recording.clone();
        let mutation = Mutation::new();
        let writer = FileWriter::new(
            Access::new(root_text.clone(), root_text, std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())),
            mutation.clone(),
            authorizer,
            Arc::new(NoFormat),
        );
        let deps =
            BuiltinDeps { full_codemode: false, edit: Some(EditDeps { writer, mutation }), ..BuiltinDeps::default() };
        let registry = ToolRegistry::new();
        let _registration = builtin::activate(&registry, &deps).await;
        let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
        let context = ToolContext {
            session_id: "ses_edit_tool_test".to_owned(),
            agent: "build".to_owned(),
            message_id: "msg_tool_test".to_owned(),
            call_id: format!("call-{index}"),
            authorizer: recording.clone(),
            ..ToolContext::default()
        };
        let result = snapshot
            .execute(
                ToolCall {
                    id: format!("call-{index}"),
                    name: "edit".to_owned(),
                    input: replace_placeholders(case.call.clone(), &root, &outside),
                },
                &context,
            )
            .await;
        let actual = result_value(result, recording.as_ref(), &root, &outside);
        assert_eq!(actual, expected, "case {index} ({})", case.name);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}

#[tokio::test]
async fn formatted_content_is_reported_in_the_diff() {
    let root = unique_directory("edit-format");
    std::fs::write(root.join("formatted.txt"), "before\n").expect("the fixture file is written");
    let (snapshot, context, _authorizer, _registration) = build(&root, Arc::new(ReplaceAfter)).await;
    let result = snapshot
        .execute(
            ToolCall {
                id: "call-format".to_owned(),
                name: "edit".to_owned(),
                input: json!({ "path": "formatted.txt", "oldString": "before", "newString": "after" }),
            },
            &context,
        )
        .await
        .expect("the edit completes");
    assert!(
        result.metadata.expect("the metadata is present")["files"][0]["patch"]
            .as_str()
            .unwrap()
            .contains("-before\n+AFTER")
    );
    assert_eq!(std::fs::read_to_string(root.join("formatted.txt")).unwrap(), "AFTER\n");
    std::fs::remove_dir_all(&root).expect("the test root directory is removed");
}

#[tokio::test]
async fn preserves_bom_and_crlf_line_endings() {
    let root = unique_directory("edit-bom");
    std::fs::write(root.join("windows.txt"), "\u{feff}before\r\nrest\r\n").expect("the fixture file is written");
    let (snapshot, context, _authorizer, _registration) = build(&root, Arc::new(StripBom)).await;
    let result = snapshot
        .execute(
            ToolCall {
                id: "call-bom".to_owned(),
                name: "edit".to_owned(),
                input: json!({ "path": "windows.txt", "oldString": "before\nrest", "newString": "after\nrest" }),
            },
            &context,
        )
        .await;
    assert!(result.is_ok(), "the edit completes");
    assert_eq!(std::fs::read_to_string(root.join("windows.txt")).unwrap(), "\u{feff}after\r\nrest\r\n");
    std::fs::remove_dir_all(&root).expect("the test root directory is removed");
}

#[tokio::test]
async fn serializes_concurrent_edit_transactions() {
    let root = unique_directory("edit-concurrent");
    std::fs::write(root.join("concurrent.txt"), "one\ntwo\n").expect("the fixture file is written");
    let (snapshot, context, _authorizer, _registration) = build(&root, Arc::new(NoFormat)).await;
    let first = ToolContext { call_id: "call-edit-one".to_owned(), ..context.clone() };
    let second = ToolContext { call_id: "call-edit-two".to_owned(), ..context.clone() };
    let (one, two) = tokio::join!(
        snapshot.execute(
            ToolCall {
                id: "call-edit-one".to_owned(),
                name: "edit".to_owned(),
                input: json!({ "path": "concurrent.txt", "oldString": "one", "newString": "ONE" }),
            },
            &first,
        ),
        snapshot.execute(
            ToolCall {
                id: "call-edit-two".to_owned(),
                name: "edit".to_owned(),
                input: json!({ "path": "concurrent.txt", "oldString": "two", "newString": "TWO" }),
            },
            &second,
        )
    );
    assert!(one.is_ok(), "the first edit completes");
    assert!(two.is_ok(), "the second edit completes");
    assert_eq!(std::fs::read_to_string(root.join("concurrent.txt")).unwrap(), "ONE\nTWO\n");
    std::fs::remove_dir_all(&root).expect("the test root directory is removed");
}

async fn build(
    root: &std::path::Path,
    format: Arc<dyn FormatHook>,
) -> (opencode_tool::Snapshot, ToolContext, Arc<RecordingAuthorizer>, opencode_location::state::Registration) {
    let recording = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let authorizer: Arc<dyn Authorizer> = recording.clone();
    let mutation = Mutation::new();
    let root_text = root.to_string_lossy().into_owned();
    let writer = FileWriter::new(
        Access::new(root_text.clone(), root_text, std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())),
        mutation.clone(),
        authorizer,
        format,
    );
    let deps =
        BuiltinDeps { full_codemode: false, edit: Some(EditDeps { writer, mutation }), ..BuiltinDeps::default() };
    let registry = ToolRegistry::new();
    let registration = builtin::activate(&registry, &deps).await;
    let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
    let context = ToolContext {
        session_id: "ses_edit_tool_test".to_owned(),
        agent: "build".to_owned(),
        message_id: "msg_tool_test".to_owned(),
        call_id: "call-edit".to_owned(),
        authorizer: recording.clone(),
        ..ToolContext::default()
    };
    (snapshot, context, recording, registration)
}

fn result_value(
    result: Result<ToolOutput, opencode_tool::ToolError>,
    authorizer: &RecordingAuthorizer,
    root: &std::path::Path,
    outside: &std::path::Path,
) -> Value {
    let mut actual = json!({
        "assertions": authorizer.assertions(),
        "tree": file_tree(root, outside),
    });
    match result {
        Ok(ToolOutput { output, content, metadata }) => {
            actual["status"] = json!("completed");
            actual["output"] = output;
            actual["content"] = json!(content.iter().map(opencode_tool::Content::to_value).collect::<Vec<_>>());
            if let Some(metadata) = metadata {
                actual["metadata"] = metadata;
            }
        }
        Err(error) => {
            actual["status"] = json!("error");
            actual["error"] = json!(error.message);
        }
    }
    actual
}

struct ReplaceAfter;

impl FormatHook for ReplaceAfter {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let text = std::fs::read_to_string(&target.absolute).expect("the formatted file is readable");
            std::fs::write(&target.absolute, text.replace("after", "AFTER")).expect("the file is formatted");
            true
        })
    }
}

struct StripBom;

impl FormatHook for StripBom {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let text = std::fs::read_to_string(&target.absolute).expect("the formatted file is readable");
            std::fs::write(&target.absolute, text.trim_start_matches('\u{feff}')).expect("the file is formatted");
            true
        })
    }
}
