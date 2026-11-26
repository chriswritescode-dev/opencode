use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_fs::FsError;
use opencode_fs::filesystem::{EntryKind, LocationFileSystem};
use serde_json::{Value, json};

use crate::fixture::{self, TempDir};

#[tokio::test]
async fn filesystem_corpus_matches_expected_output() {
    for case in fixture::cases("filesystem") {
        let name = fixture::field(&case, "name");
        let temp = TempDir::new(name);
        let directory = temp.path().join(name);
        std::fs::create_dir_all(&directory).expect("creates the case directory");
        fixture::materialize(&directory, &case["fixture"]);
        let location = directory.join(case["location"].as_str().unwrap_or("."));
        let filesystem = LocationFileSystem::new(location.to_string_lossy().into_owned())
            .await
            .unwrap_or_else(|error| panic!("case {name}: {error}"));
        let data = fixture::template_json(&case, &temp.text());
        match fixture::field(&data, "kind") {
            "read" => read_case(&filesystem, &data, &data["expected"], name).await,
            "list" => list_case(&filesystem, &data, &data["expected"], name).await,
            "write" => write_case(&filesystem, &data, &data["expected"], name).await,
            kind => panic!("case {name}: unknown kind {kind}"),
        }
    }
}

async fn read_case(filesystem: &LocationFileSystem, case: &Value, expected: &Value, name: &str) {
    let input = fixture::field(case, "input");
    let result = filesystem.read(input).await;
    if let Some(tag) = expected["error"].as_str() {
        let error = result.err().unwrap_or_else(|| panic!("case {name}: expected {tag}"));
        assert_eq!(error_tag(&error), tag, "case {name}");
        return;
    }
    let info = result.unwrap_or_else(|error| panic!("case {name}: {error}"));
    assert_eq!(Value::String(info.mime.clone()), expected["mime"], "case {name}: mime");
    assert_eq!(Value::from(info.size), expected["size"], "case {name}: size");
    assert_eq!(Value::Bool(info.mtime.is_some()), expected["mtime"], "case {name}: mtime");
    let offset = case["range"]["offset"].as_u64().unwrap_or(0);
    let length = case["range"]["bytesToRead"].as_u64();
    let bytes =
        filesystem.open_range(&info, offset, length).await.unwrap_or_else(|error| panic!("case {name}: {error}"));
    assert_eq!(Value::String(STANDARD.encode(bytes)), expected["bytes"], "case {name}: bytes");
}

async fn list_case(filesystem: &LocationFileSystem, case: &Value, expected: &Value, name: &str) {
    let input = case["input"].as_str();
    let result = filesystem.list(input).await;
    if let Some(tag) = expected["error"].as_str() {
        let error = result.err().unwrap_or_else(|| panic!("case {name}: expected {tag}"));
        assert_eq!(error_tag(&error), tag, "case {name}");
        return;
    }
    let entries = result.unwrap_or_else(|error| panic!("case {name}: {error}"));
    let actual = entries
        .iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "type": match entry.kind { EntryKind::File => "file", EntryKind::Directory => "directory" },
            })
        })
        .collect();
    assert_eq!(Value::Array(actual), *expected, "case {name}");
}

async fn write_case(filesystem: &LocationFileSystem, case: &Value, expected: &Value, name: &str) {
    let bytes = STANDARD.decode(fixture::field(case, "base64")).expect("decodes write bytes");
    let written = filesystem
        .write(fixture::field(case, "input"), &bytes)
        .await
        .unwrap_or_else(|error| panic!("case {name}: {error}"));
    assert_eq!(Value::String(written.to_string_lossy().into_owned()), expected["path"], "case {name}: path");
    let actual = std::fs::read(&written).unwrap_or_else(|error| panic!("case {name}: {error}"));
    assert_eq!(Value::String(STANDARD.encode(actual)), expected["bytes"], "case {name}: bytes");
}

#[tokio::test]
async fn missing_directory_is_directory_not_found() {
    let temp = TempDir::new("missing");
    let missing = temp.path().join("absent");
    let error = match LocationFileSystem::new(missing.to_string_lossy().into_owned()).await {
        Ok(_) => panic!("a missing directory must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, FsError::DirectoryNotFound { .. }), "{error}");
}

#[test]
fn dirname_strips_the_last_segment_without_resolving_the_path() {
    use opencode_fs::path;
    assert_eq!(path::dirname("/a/b"), "/a");
    assert_eq!(path::dirname("/a"), "/");
    assert_eq!(path::dirname("/"), "/");
    assert_eq!(path::dirname("a"), ".");
    assert_eq!(path::dirname("/a/b/.."), "/a/b");
}

fn error_tag(error: &FsError) -> &'static str {
    match error {
        FsError::NotFound { .. } => "FileSystem.NotFoundError",
        FsError::DirectoryNotFound { .. } => "FileSystem.DirectoryNotFoundError",
        FsError::DirectoryAccessDenied { .. } => "FileSystem.DirectoryAccessDeniedError",
        FsError::Failed { .. } => "FileSystem.Failed",
        FsError::InvalidUtf8 { .. } => "FileMutation.InvalidUtf8Error",
    }
}
