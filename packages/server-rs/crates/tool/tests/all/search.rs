use std::sync::Arc;
use std::time::Duration;

use opencode_config::paths::GlobalPaths;
use opencode_fs::access::Access;
use opencode_fs::ripgrep::Ripgrep;
use opencode_fs::ripgrep::binary::{Binary, releases};
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::glob::GlobDeps;
use opencode_tool::tools::grep::GrepDeps;
use opencode_tool::tools::search::{self, SearchError};
use opencode_tool::{SnapshotOptions, ToolCall, ToolContext, ToolOutput, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    Fixture, PermissionSpec, RecordingAuthorizer, file_tree, materialize, replace_placeholders, unique_directory,
};

#[derive(Deserialize)]
struct SearchCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    call: Value,
    #[serde(default)]
    permission: PermissionSpec,
    expected: Value,
}

fn ripgrep() -> Arc<Ripgrep> {
    Arc::new(Ripgrep::new(Binary::new(
        GlobalPaths::from_env().expect("global paths"),
        |name| std::env::var_os(name),
        releases(),
    )))
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())
}

async fn execute(
    tool: &str,
    registry: &ToolRegistry,
    index: usize,
    input: Value,
    authorizer: &Arc<RecordingAuthorizer>,
) -> Result<ToolOutput, opencode_tool::ToolError> {
    let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
    let context = ToolContext {
        session_id: format!("ses_{tool}_tool_test"),
        agent: "build".to_owned(),
        message_id: "msg_tool_test".to_owned(),
        call_id: format!("call-{index}"),
        authorizer: authorizer.clone(),
        ..ToolContext::default()
    };
    snapshot.execute(ToolCall { id: format!("call-{index}"), name: tool.to_owned(), input }, &context).await
}

fn record(result: Result<ToolOutput, opencode_tool::ToolError>, authorizer: &RecordingAuthorizer) -> Value {
    let mut actual = json!({ "assertions": authorizer.assertions() });
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

fn sort_glob_order(value: &mut Value) {
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        output.sort_by_key(|item| item.get("path").and_then(Value::as_str).unwrap_or_default().to_owned());
    }
    let Some(parts) = value.get_mut("content").and_then(Value::as_array_mut) else { return };
    for part in parts.iter_mut() {
        let Some(text) = part.get("text").and_then(Value::as_str).map(str::to_owned) else { continue };
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        let footer = lines.len().saturating_sub(2);
        let truncated =
            lines.len() >= 2 && lines[footer].is_empty() && lines[footer + 1].starts_with("(Results are truncated");
        let note = if truncated { lines.split_off(footer) } else { Vec::new() };
        lines.sort();
        lines.extend(note);
        part["text"] = Value::String(lines.join("\n"));
    }
}

async fn run_corpus(tool: &str, corpus: &str) {
    let cases: Vec<SearchCase> = serde_json::from_str(corpus).expect("the search corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("{tool}-{index}"));
        let outside = unique_directory(&format!("{tool}-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let mut expected = replace_placeholders(case.expected.clone(), &root, &outside);
        if tool == "glob" {
            sort_glob_order(&mut expected);
        }

        let authorizer = Arc::new(RecordingAuthorizer::new(
            case.permission.allow.unwrap_or(true),
            case.permission.denied_resources.clone(),
        ));
        let deps = BuiltinDeps {
            full_codemode: false,
            glob: (tool == "glob").then(|| GlobDeps { ripgrep: ripgrep(), access: access(&root) }),
            grep: (tool == "grep").then(|| GrepDeps { ripgrep: ripgrep(), access: access(&root) }),
            ..BuiltinDeps::default()
        };
        let registry = ToolRegistry::new();
        let _registration = builtin::activate(&registry, &deps).await;
        let input = replace_placeholders(case.call.clone(), &root, &outside);
        let result = execute(tool, &registry, index, input, &authorizer).await;
        let mut actual = record(result, &authorizer);
        actual["tree"] = file_tree(&root, &outside);
        if tool == "glob" {
            sort_glob_order(&mut actual);
        }
        assert_eq!(actual, expected, "case {index} ({})", case.name);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}

fn access(root: &std::path::Path) -> Access {
    let root = root.to_string_lossy().into_owned();
    Access::new(root.clone(), root, home())
}

#[tokio::test]
async fn glob_corpus_matches() {
    run_corpus("glob", include_str!("../corpus/glob.json")).await;
}

#[tokio::test]
async fn grep_corpus_matches() {
    run_corpus("grep", include_str!("../corpus/grep.json")).await;
}

#[tokio::test]
async fn glob_reports_truncation_without_ordering() {
    let root = unique_directory("glob-truncation");
    let outside = unique_directory("glob-truncation-outside");
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(root.join(name), "needle\n").expect("the fixture file is written");
    }
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = BuiltinDeps {
        full_codemode: false,
        glob: Some(GlobDeps { ripgrep: ripgrep(), access: access(&root) }),
        ..BuiltinDeps::default()
    };
    let registry = ToolRegistry::new();
    let _registration = builtin::activate(&registry, &deps).await;
    let output = execute("glob", &registry, 0, json!({ "pattern": "*.txt", "limit": 1 }), &authorizer)
        .await
        .expect("glob succeeds");
    assert_eq!(output.metadata, Some(json!({ "count": 1, "truncated": true })));
    assert_eq!(output.output.as_array().map(Vec::len), Some(1));
    let text = match &output.content[0] {
        opencode_tool::Content::Text { text } => text.clone(),
        other => panic!("unexpected content {other:?}"),
    };
    assert!(text.ends_with(
        "\n\n(Results are truncated: showing first 1 results. Consider using a more specific path or pattern.)"
    ));
    std::fs::remove_dir_all(&root).expect("the test root directory is removed");
    std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
}

#[tokio::test]
async fn bounded_requests_one_past_the_limit() {
    let bounded = search::bounded::<usize, (), _, _>(2, |limit| async move { Ok::<_, ()>((0..limit).collect()) })
        .await
        .ok()
        .expect("the search succeeds");
    assert_eq!(bounded.items, vec![0, 1]);
    assert!(bounded.truncated);
}

#[tokio::test(start_paused = true)]
async fn bounded_times_out_with_its_message() {
    let task = tokio::spawn(async {
        search::bounded::<usize, (), _, _>(5, |_| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(Vec::new())
        })
        .await
    });
    let result = task.await.expect("the task joins");
    assert!(matches!(result, Err(SearchError::Timeout)));
    assert_eq!(
        search::timeout_error().message,
        "Search timed out after 30 seconds. Consider using a more specific path or pattern."
    );
}
