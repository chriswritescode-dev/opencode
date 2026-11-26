use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text { text: String },
    Repeat { repeat: RepeatPart },
    Padded { padded: PaddedPart },
}

#[derive(Deserialize)]
pub struct RepeatPart {
    pub text: String,
    pub count: usize,
}

#[derive(Deserialize)]
pub struct PaddedPart {
    pub count: usize,
    pub width: usize,
    #[serde(default)]
    pub separator: Option<String>,
    #[serde(default, rename = "final")]
    pub final_newline: Option<bool>,
    #[serde(default, rename = "prefixAt")]
    pub prefix_at: Option<PrefixAt>,
}

#[derive(Deserialize)]
pub struct PrefixAt {
    pub index: usize,
    pub text: String,
}

#[derive(Deserialize)]
pub struct Fixture {
    pub path: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub base64: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, rename = "headerBase64")]
    pub header_base64: Option<String>,
}

pub fn part_text(parts: &[Part]) -> String {
    let mut result = String::new();
    for part in parts {
        match part {
            Part::Text { text } => result.push_str(text),
            Part::Repeat { repeat } => result.push_str(&repeat.text.repeat(repeat.count)),
            Part::Padded { padded } => {
                let separator = padded.separator.as_deref().unwrap_or("\n");
                for index in 0..padded.count {
                    if index > 0 {
                        result.push_str(separator);
                    }
                    if padded.prefix_at.as_ref().is_some_and(|prefix| prefix.index == index) {
                        result.push_str(&padded.prefix_at.as_ref().unwrap().text);
                    }
                    let number = (index + 1).to_string();
                    result.push_str(&number);
                    for _ in number.len()..padded.width {
                        result.push('x');
                    }
                }
                if padded.final_newline != Some(false) {
                    result.push_str(separator);
                }
            }
        }
    }
    result
}

pub fn substitute(value: &str, root: &Path, outside: &Path) -> String {
    let root = root.to_string_lossy();
    let outside = outside.to_string_lossy();
    let relative = opencode_util::path::Flavor::NATIVE.relative(&root, &outside);
    value.replace("{{root}}", &root).replace("{{outside}}", &outside).replace("{{outside_relative}}", &relative)
}

pub fn replace_placeholders(value: serde_json::Value, root: &Path, outside: &Path) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(substitute(&text, root, outside)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(|item| replace_placeholders(item, root, outside)).collect())
        }
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object.into_iter().map(|(key, item)| (key, replace_placeholders(item, root, outside))).collect(),
        ),
        value => value,
    }
}

pub fn materialize(root: &Path, outside: &Path, fixtures: &[Fixture]) {
    for fixture in fixtures {
        let target = PathBuf::from(substitute(&fixture.path, root, outside));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("the fixture parent directory is created");
        }
        match fixture.kind.as_deref() {
            Some("directory") => std::fs::create_dir_all(&target).expect("the fixture directory is created"),
            Some("binary") => {
                let bytes =
                    BASE64.decode(fixture.base64.as_deref().unwrap_or_default()).expect("the fixture bytes are base64");
                std::fs::write(&target, bytes).expect("the fixture file is written");
            }
            Some("symlink") => {
                let link = substitute(fixture.target.as_deref().unwrap_or_default(), root, outside);
                std::os::unix::fs::symlink(link, &target).expect("the fixture symlink is created");
            }
            Some("truncate") => {
                let header = fixture.header_base64.as_deref().unwrap_or_default();
                std::fs::write(&target, BASE64.decode(header).expect("the fixture header is base64"))
                    .expect("the fixture file is written");
                let file = std::fs::OpenOptions::new().write(true).open(&target).expect("the fixture file opens");
                file.set_len(fixture.size.unwrap_or_default()).expect("the fixture file is truncated");
            }
            _ => std::fs::write(&target, part_text(&fixture.parts)).expect("the fixture file is written"),
        }
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn unique_directory(label: &str) -> PathBuf {
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "opencode-tool-test-{}-{counter}-{}",
        std::process::id(),
        label.replace(|character: char| !character.is_ascii_alphanumeric(), "-")
    ));
    std::fs::create_dir_all(&path).expect("the test directory is created");
    std::fs::canonicalize(&path).expect("the test directory resolves")
}

#[derive(Clone, serde::Serialize)]
pub struct RecordedAssertion {
    pub action: String,
    pub resources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

pub struct RecordingAuthorizer {
    pub allow: bool,
    pub read_allowed: bool,
    pub denied: Vec<String>,
    pub denied_actions: Vec<String>,
    pub assertions: std::sync::Mutex<Vec<RecordedAssertion>>,
}

impl RecordingAuthorizer {
    pub fn new(allow: bool, denied: Vec<String>) -> Self {
        RecordingAuthorizer {
            allow,
            read_allowed: true,
            denied,
            denied_actions: Vec::new(),
            assertions: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn deny_actions(&mut self, denied: Vec<String>) {
        self.denied_actions = denied;
    }

    pub fn assertions(&self) -> Vec<RecordedAssertion> {
        self.assertions.lock().expect("the assertion log is not poisoned").clone()
    }

    fn denies(&self, action: &str, resources: &[String]) -> bool {
        !self.allow
            || self.denied_actions.iter().any(|denied| denied == action)
            || resources.iter().any(|resource| self.denied.contains(resource))
    }
}

impl opencode_fs::access::Authorizer for RecordingAuthorizer {
    fn assert<'a>(
        &'a self,
        input: opencode_permission::service::AssertInput,
    ) -> futures_util::future::BoxFuture<'a, Result<(), opencode_permission::service::PermissionError>> {
        Box::pin(async move {
            self.assertions.lock().expect("the assertion log is not poisoned").push(RecordedAssertion {
                action: input.action.clone(),
                resources: input.resources.clone(),
                save: input.save.clone(),
                metadata: input.metadata.map(|metadata| serde_json::Value::Object(metadata.0.into_iter().collect())),
            });
            if self.denies(&input.action, &input.resources) {
                return Err(opencode_permission::service::PermissionError::Blocked {
                    rules: Vec::new(),
                    permission: input.action,
                    resources: input.resources,
                    reason: None,
                });
            }
            Ok(())
        })
    }

    fn allows<'a>(
        &'a self,
        input: opencode_permission::service::AssertInput,
    ) -> futures_util::future::BoxFuture<'a, Result<bool, opencode_permission::service::PermissionError>> {
        Box::pin(async move { Ok(self.read_allowed && !self.denies(&input.action, &input.resources)) })
    }
}

pub fn file_tree(root: &Path, outside: &Path) -> serde_json::Value {
    let mut tree = serde_json::Map::new();
    collect_tree(root, "{{root}}", &mut tree);
    collect_tree(outside, "{{outside}}", &mut tree);
    serde_json::Value::Object(tree)
}

fn collect_tree(directory: &Path, prefix: &str, tree: &mut serde_json::Map<String, serde_json::Value>) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let key = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
        if metadata.is_symlink() {
            let target = std::fs::read_link(&path).unwrap_or_default();
            tree.insert(key, serde_json::json!(format!("symlink:{}", target.to_string_lossy())));
        } else if metadata.is_dir() {
            tree.insert(key.clone(), serde_json::Value::Null);
            collect_tree(&path, &key, tree);
        } else {
            let bytes = std::fs::read(&path).unwrap_or_default();
            tree.insert(key, serde_json::json!(BASE64.encode(bytes)));
        }
    }
}

#[derive(Default, Deserialize)]
pub struct PermissionSpec {
    #[serde(default)]
    pub allow: Option<bool>,
    #[serde(default, rename = "deniedResources")]
    pub denied_resources: Vec<String>,
    #[serde(default, rename = "deniedActions")]
    pub denied_actions: Vec<String>,
}

pub fn result_value(
    result: Result<opencode_tool::ToolOutput, opencode_tool::ToolError>,
    authorizer: &RecordingAuthorizer,
    root: &Path,
    outside: &Path,
) -> serde_json::Value {
    let mut actual = serde_json::json!({
        "assertions": authorizer.assertions(),
        "tree": file_tree(root, outside),
    });
    match result {
        Ok(opencode_tool::ToolOutput { output, content, metadata }) => {
            actual["status"] = serde_json::json!("completed");
            actual["output"] = output;
            actual["content"] =
                serde_json::json!(content.iter().map(opencode_tool::Content::to_value).collect::<Vec<_>>());
            if let Some(metadata) = metadata {
                actual["metadata"] = metadata;
            }
        }
        Err(error) => {
            actual["status"] = serde_json::json!("error");
            actual["error"] = serde_json::json!(error.message);
        }
    }
    actual
}
