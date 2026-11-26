use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("opencode-fs-{}-{id}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("creates the temporary directory");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn cases(area: &str) -> Vec<Value> {
    let path = format!("{}/tests/corpus/{area}.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let cases: Vec<Value> = serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {path}: {error}"));
    assert!(!cases.is_empty(), "{area} corpus is empty");
    cases
}

pub fn field<'a>(case: &'a Value, key: &str) -> &'a str {
    case[key].as_str().unwrap_or_else(|| panic!("case {}: {key} is a string", case["name"]))
}

pub fn materialize(directory: &Path, fixture: &Value) {
    for dir in fixture["dirs"].as_array().into_iter().flatten() {
        fs::create_dir_all(directory.join(dir.as_str().expect("dir is a string"))).expect("creates a fixture dir");
    }
    for file in fixture["files"].as_array().into_iter().flatten() {
        let path = directory.join(file["path"].as_str().expect("file path is a string"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("creates a fixture file directory");
        }
        let bytes = match file["base64"].as_str() {
            Some(encoded) => STANDARD.decode(encoded).expect("decodes fixture bytes"),
            None => file["text"].as_str().unwrap_or_default().as_bytes().to_vec(),
        };
        fs::write(&path, bytes).expect("writes a fixture file");
    }
    for link in fixture["symlinks"].as_array().into_iter().flatten() {
        let path = directory.join(link["path"].as_str().expect("symlink path is a string"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("creates a symlink directory");
        }
        symlink(link["target"].as_str().expect("symlink target is a string"), &path);
    }
}

pub fn template(value: &str, root: &str) -> String {
    value.replace("{{root}}", root)
}

pub fn template_json(value: &Value, root: &str) -> Value {
    match value {
        Value::String(text) => Value::String(template(text, root)),
        Value::Array(items) => Value::Array(items.iter().map(|item| template_json(item, root)).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(key, item)| (key.clone(), template_json(item, root))).collect())
        }
        other => other.clone(),
    }
}

#[cfg(unix)]
fn symlink(target: &str, path: &Path) {
    std::os::unix::fs::symlink(target, path).expect("creates a fixture symlink");
}

#[cfg(windows)]
fn symlink(target: &str, path: &Path) {
    let resolved = path.parent().unwrap_or(Path::new(".")).join(target);
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, path).expect("creates a fixture directory link");
    } else {
        std::os::windows::fs::symlink_file(target, path).expect("creates a fixture file link");
    }
}
