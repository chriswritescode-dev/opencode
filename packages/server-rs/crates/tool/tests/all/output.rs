use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use opencode_tool::ToolOutput;
use opencode_tool::context::Content;
use opencode_tool::output::{self, OutputStore};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
struct Case {
    name: String,
    limits: Limits,
    content: Vec<Value>,
    #[serde(default)]
    metadata: Option<Value>,
    expected: Expected,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limits {
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Deserialize)]
struct Expected {
    content: Vec<Value>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    saved: Option<String>,
}

#[tokio::test]
async fn truncate_corpus_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../corpus/truncate.json")).expect("the truncate corpus is valid JSON");
    for case in cases {
        let temp = TempDir::new(&case.name);
        let mut store = OutputStore::new(temp.path());
        store.configure(Some(case.limits.max_lines), Some(case.limits.max_bytes));
        if case.name == "save-failure" {
            tokio::fs::write(store.directory(), b"not a directory")
                .await
                .expect("the output directory path can be occupied by a file");
        }
        let result = store
            .truncate(ToolOutput {
                output: Value::Null,
                content: case
                    .content
                    .iter()
                    .map(|item| Content::from_value(item).expect("a known content shape"))
                    .collect(),
                metadata: case.metadata.clone(),
            })
            .await;
        let (content, metadata) = normalize(&result, &case.name);
        assert_eq!(content, case.expected.content, "content mismatch in {}", case.name);
        assert_eq!(metadata, case.expected.metadata, "metadata mismatch in {}", case.name);
        if let Some(saved) = &case.expected.saved {
            let path = result
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("outputPath"))
                .and_then(Value::as_str)
                .expect("a saved result names its output path");
            assert_eq!(tokio::fs::read_to_string(path).await.expect("the saved output can be read"), *saved);
        }
    }
}

fn normalize(result: &ToolOutput, name: &str) -> (Vec<Value>, Option<Value>) {
    let content: Vec<Value> = result.content.iter().map(Content::to_value).collect();
    let mut encoded = Value::Object(Map::from_iter([
        ("content".to_owned(), Value::Array(content)),
        ("metadata".to_owned(), result.metadata.clone().unwrap_or(Value::Null)),
    ]));
    if let Some(path) = result.metadata.as_ref().and_then(|metadata| metadata.get("outputPath")).and_then(Value::as_str)
    {
        encoded = replace(encoded, path, "{{output}}");
    }
    if name == "save-failure" {
        encoded = replace_error(encoded);
    }
    let Value::Object(mut object) = encoded else { unreachable!("the normalized result is an object") };
    let content = match object.shift_remove("content") {
        Some(Value::Array(content)) => content,
        _ => Vec::new(),
    };
    let metadata = match object.shift_remove("metadata") {
        Some(Value::Null) | None => None,
        metadata => metadata,
    };
    (content, metadata)
}

fn replace(value: Value, from: &str, to: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(from, to)),
        Value::Array(items) => Value::Array(items.into_iter().map(|item| replace(item, from, to)).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(key, item)| (key, replace(item, from, to))).collect())
        }
        other => other,
    }
}

fn replace_error(value: Value) -> Value {
    let Value::Object(mut map) = value else { return value };
    if let Some(Value::Array(items)) = map.get_mut("content") {
        for item in items {
            let Value::Object(text) = item else { continue };
            let Some(Value::String(message)) = text.get_mut("text") else { continue };
            let Some(start) = message.find("could not be saved: ") else { continue };
            let begin = start + "could not be saved: ".len();
            let Some(end) = message[begin..].find(']').map(|end| begin + end) else { continue };
            message.replace_range(begin..end, "{{error}}");
        }
    }
    Value::Object(map)
}

#[tokio::test]
async fn configures_limits_from_tool_output() {
    let temp = TempDir::new("tool-output-config");
    let mut store = OutputStore::new(temp.path());
    let info: opencode_contract::generated::ConfigEntryDocumentInfo =
        opencode_contract::codec::decode_json(r#"{"tool_output":{"max_lines":2,"max_bytes":1000}}"#)
            .expect("the tool output config decodes");
    store.observe(&[opencode_config::entry::Entry::Document(opencode_config::entry::Document {
        path: None,
        info,
        origin: opencode_config::entry::Origin::File,
    })]);
    let result = store
        .truncate(ToolOutput { output: Value::Null, content: vec![Content::text("one\ntwo\nthree")], metadata: None })
        .await;
    assert_eq!(result.metadata.as_ref().and_then(|metadata| metadata.get("truncated")), Some(&Value::Bool(true)));
    assert_eq!(result.content[0], Content::text("one\ntwo"));
}

#[tokio::test]
async fn cleanup_removes_outputs_older_than_retention() {
    let temp = TempDir::new("tool-output-cleanup");
    let store = OutputStore::new(temp.path());
    tokio::fs::create_dir_all(store.directory()).await.expect("the output directory is created");
    let old = store.directory().join(output::file_name(2_i64.pow(36) - 1));
    let recent = store.directory().join(output::file_name(2_i64.pow(36) + 1));
    tokio::fs::write(&old, b"old").await.expect("the old output is written");
    tokio::fs::write(&recent, b"recent").await.expect("the recent output is written");
    let file = std::fs::File::options().write(true).open(&old).expect("the old output opens");
    file.set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60))
        .expect("the old output keeps an old modification time");
    store.cleanup().await;
    assert!(!old.exists(), "the eight-day-old output is removed");
    assert!(recent.exists(), "the one-day-old output is kept");
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("opencode-tool-output-{}-{unique}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temporary directory is created");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
