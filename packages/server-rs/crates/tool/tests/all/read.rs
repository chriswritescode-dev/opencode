use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::future::BoxFuture;
use indexmap::IndexMap;
use opencode_contract::generated::{AbsolutePath, SkillID, SkillInfo, SkillName};
use opencode_fs::access::Access;
use opencode_registry::events::RegistryEvents;
use opencode_registry::skill::SkillService;
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::read::to_model_content;
use opencode_tool::tools::read::{NearbyInstructions, ReadDeps};
use opencode_tool::tools::read_filesystem::{self, LocalFiles, PageInput, ReadError, ReadFileError, ReadOutput};
use opencode_tool::{SnapshotOptions, ToolCall, ToolContext, ToolOutput, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    Fixture, RecordingAuthorizer, file_tree, materialize, replace_placeholders, substitute, unique_directory,
};

#[derive(Deserialize)]
struct FilesystemCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    path: String,
    resource: String,
    #[serde(default)]
    page: Page,
    expected: Value,
}

#[derive(Default, Deserialize)]
struct Page {
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[tokio::test]
async fn read_filesystem_corpus_matches() {
    let cases: Vec<FilesystemCase> = serde_json::from_str(include_str!("../corpus/read_filesystem.json"))
        .expect("the read_filesystem corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("read-fs-{index}"));
        let outside = unique_directory(&format!("read-fs-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);
        let target = substitute(&case.path, &root, &outside);
        let page = PageInput { offset: case.page.offset, limit: case.page.limit };
        let actual = match read_filesystem::read(&LocalFiles, &target, &case.resource, &page).await {
            Ok(output) => {
                let model: Vec<Value> = to_model_content(&case.resource, page.offset, &output)
                    .iter()
                    .map(opencode_tool::Content::to_value)
                    .collect();
                let output = match &output {
                    ReadOutput::Media(media) => {
                        let mut value = output.to_value();
                        value["bytesBase64"] = json!(BASE64.encode(&media.bytes));
                        value
                    }
                    _ => output.to_value(),
                };
                json!({ "ok": true, "output": output, "model": model })
            }
            Err(error) => json!({ "ok": false, "kind": error_kind(&error), "message": error_message(&error) }),
        };
        assert_eq!(actual, expected, "case {} ({})", index, case.name);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}

fn error_kind(error: &ReadError) -> &'static str {
    match error {
        ReadError::Binary { .. } => "ReadTool.BinaryFileError",
        ReadError::MediaIngestLimit { .. } => "ReadTool.MediaIngestLimitError",
        ReadError::PathKind { .. } => "ReadTool.PathKindError",
        ReadError::DirectoryOffset { .. } => "ReadTool.DirectoryOffsetError",
        ReadError::File(ReadFileError::NotFound { .. }) => "Environment.NotFound",
        ReadError::File(_) => "Environment.Failed",
    }
}

fn error_message(error: &ReadError) -> String {
    match error {
        ReadError::File(ReadFileError::NotFound { .. }) => String::new(),
        error => error.to_string(),
    }
}

#[derive(Deserialize)]
struct ReadCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    call: ReadCall,
    #[serde(default)]
    permission: PermissionSpec,
    #[serde(default)]
    skills: Vec<SkillFixture>,
    expected: Value,
}

#[derive(Deserialize)]
struct ReadCall {
    path: String,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Default, Deserialize)]
struct PermissionSpec {
    #[serde(default)]
    allow: Option<bool>,
    #[serde(default, rename = "deniedResources")]
    denied_resources: Vec<String>,
}

#[derive(Deserialize)]
struct SkillFixture {
    id: String,
    name: String,
    path: String,
    content: String,
    #[serde(default)]
    files: Option<BTreeMap<String, String>>,
}

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _kind: &'static str) {}
}

struct RecordingNearby {
    loaded: Mutex<Vec<Vec<String>>>,
}

impl NearbyInstructions for RecordingNearby {
    fn load<'a>(&'a self, _session_id: &'a str, paths: Vec<String>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.loaded.lock().expect("the loaded log is not poisoned").push(paths);
        })
    }
}

#[tokio::test]
async fn read_corpus_matches() {
    let cases: Vec<ReadCase> =
        serde_json::from_str(include_str!("../corpus/read.json")).expect("the read corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("read-{index}"));
        let outside = unique_directory(&format!("read-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);
        let root_text = root.to_string_lossy().into_owned();

        let skills = SkillService::new(Arc::new(NoEvents));
        let mut registrations = Vec::new();
        for fixture in &case.skills {
            let info = SkillInfo {
                id: SkillID(fixture.id.clone()),
                name: SkillName(fixture.name.clone()),
                description: Some(fixture.name.clone()),
                autoinvoke: None,
                path: AbsolutePath(fixture.path.clone()),
                content: fixture.content.clone(),
            };
            let files: Option<IndexMap<String, String>> = fixture
                .files
                .as_ref()
                .map(|files| files.iter().map(|(key, value)| (key.clone(), value.clone())).collect());
            registrations.push(skills.transform(move |editor| editor.add(info.clone(), files.as_ref())).await);
        }
        let authorizer = Arc::new(RecordingAuthorizer::new(
            case.permission.allow.unwrap_or(true),
            case.permission.denied_resources.clone(),
        ));
        let nearby = Arc::new(RecordingNearby { loaded: Mutex::new(Vec::new()) });
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
        let deps = BuiltinDeps {
            full_codemode: false,
            read: Some(ReadDeps {
                files: Arc::new(LocalFiles),
                access: Access::new(root_text.clone(), root_text.clone(), home),
                skills: Some(skills.clone()),
                instructions: nearby.clone(),
                claude: true,
            }),
            write: None,
            edit: None,
            patch: None,
        };
        let registry = ToolRegistry::new();
        registrations.push(builtin::activate(&registry, &deps).await);
        let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
        let context = ToolContext {
            session_id: "ses_read_tool_test".to_owned(),
            agent: "build".to_owned(),
            message_id: "msg_tool_test".to_owned(),
            call_id: format!("call-{index}"),
            authorizer: authorizer.clone(),
            ..ToolContext::default()
        };
        let mut input = json!({ "path": substitute(&case.call.path, &root, &outside) });
        if let Some(offset) = case.call.offset {
            input["offset"] = json!(offset);
        }
        if let Some(limit) = case.call.limit {
            input["limit"] = json!(limit);
        }
        let result =
            snapshot.execute(ToolCall { id: format!("call-{index}"), name: "read".to_owned(), input }, &context).await;
        let mut actual = json!({
            "assertions": authorizer.assertions(),
            "loaded": *nearby.loaded.lock().unwrap(),
            "tree": file_tree(&root, &outside),
        });
        match result {
            Ok(ToolOutput { output, content, metadata }) => {
                actual["status"] = json!("completed");
                actual["output"] = output;
                actual["content"] = json!(content.iter().map(opencode_tool::Content::to_value).collect::<Vec<_>>());
                actual["metadata"] = metadata.unwrap_or(Value::Null);
            }
            Err(error) => {
                actual["status"] = json!("error");
                actual["error"] = json!(error.message);
            }
        }
        assert_eq!(actual, expected, "case {index} ({})", case.name);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}
