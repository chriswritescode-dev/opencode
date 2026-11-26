use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, Authorizer, Target};
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FileWriter, FormatHook, NoFormat};
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::write::WriteDeps;
use opencode_tool::{SnapshotOptions, ToolCall, ToolContext, ToolOutput, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{Fixture, RecordingAuthorizer, file_tree, materialize, replace_placeholders, unique_directory};

#[derive(Deserialize)]
struct WriteCase {
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
}

#[tokio::test]
async fn write_corpus_matches() {
    let cases: Vec<WriteCase> =
        serde_json::from_str(include_str!("../corpus/write.json")).expect("the write corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("write-{index}"));
        let outside = unique_directory(&format!("write-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);
        let root_text = root.to_string_lossy().into_owned();

        let mut recording =
            RecordingAuthorizer::new(case.permission.allow.unwrap_or(true), case.permission.denied_resources.clone());
        recording.deny_actions(case.permission.denied_actions.clone());
        let recording = Arc::new(recording);
        let authorizer: Arc<dyn Authorizer> = recording.clone();
        let writer = FileWriter::new(
            Access::new(root_text.clone(), root_text, std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())),
            Mutation::new(),
            authorizer,
            Arc::new(NoFormat),
        );
        let deps = BuiltinDeps { full_codemode: false, write: Some(WriteDeps { writer }), ..BuiltinDeps::default() };
        let registry = ToolRegistry::new();
        let _registration = builtin::activate(&registry, &deps).await;
        let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
        let context = ToolContext {
            session_id: "ses_write_tool_test".to_owned(),
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
                    name: "write".to_owned(),
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
async fn preserves_exactly_one_bom_when_overwriting_existing_files() {
    let root = unique_directory("write-bom");
    std::fs::write(root.join("preserved.txt"), "\u{feff}before").expect("the preserved fixture is written");
    std::fs::write(root.join("deduplicated.txt"), "\u{feff}before").expect("the deduplicated fixture is written");
    let recording = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let authorizer: Arc<dyn Authorizer> = recording.clone();
    let root_text = root.to_string_lossy().into_owned();
    let writer = FileWriter::new(
        Access::new(root_text.clone(), root_text, std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())),
        Mutation::new(),
        authorizer,
        Arc::new(TripleBom),
    );
    let deps = BuiltinDeps { full_codemode: false, write: Some(WriteDeps { writer }), ..BuiltinDeps::default() };
    let registry = ToolRegistry::new();
    let _registration = builtin::activate(&registry, &deps).await;
    let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
    let context = ToolContext {
        session_id: "ses_write_tool_test".to_owned(),
        agent: "build".to_owned(),
        message_id: "msg_tool_test".to_owned(),
        call_id: "call-write".to_owned(),
        authorizer: recording.clone(),
        ..ToolContext::default()
    };
    for (path, content) in [("preserved.txt", "after"), ("deduplicated.txt", "\u{feff}after")] {
        snapshot
            .execute(
                ToolCall {
                    id: format!("call-{path}"),
                    name: "write".to_owned(),
                    input: json!({ "path": path, "content": content }),
                },
                &context,
            )
            .await
            .expect("the write completes");
    }
    assert_eq!(std::fs::read_to_string(root.join("preserved.txt")).unwrap(), "\u{feff}after");
    assert_eq!(std::fs::read_to_string(root.join("deduplicated.txt")).unwrap(), "\u{feff}after");
    std::fs::remove_dir_all(&root).expect("the test root directory is removed");
}

struct TripleBom;

impl FormatHook for TripleBom {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let text = std::fs::read_to_string(&target.absolute).expect("the formatted file is readable");
            std::fs::write(
                &target.absolute,
                format!("\u{feff}\u{feff}\u{feff}{}", text.trim_start_matches('\u{feff}')),
            )
            .expect("the file is formatted");
            true
        })
    }
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
