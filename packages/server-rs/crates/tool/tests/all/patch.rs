use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_fs::FsError;
use opencode_fs::access::{Access, Authorizer, Target};
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FormatHook, NoFormat};
use opencode_permission::service::{AssertInput, PermissionError};
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::patch::{EntryKind, HostFiles, PatchDeps, PatchFiles};
use opencode_tool::{Snapshot, SnapshotOptions, ToolCall, ToolContext, ToolOutput, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    Fixture, PermissionSpec, RecordingAuthorizer, materialize, replace_placeholders, result_value, substitute,
    unique_directory,
};

#[derive(Deserialize)]
struct PatchCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    call: Value,
    #[serde(default)]
    permission: PermissionSpec,
    #[serde(default)]
    location: Option<LocationSpec>,
    expected: Value,
}

#[derive(Deserialize)]
struct LocationSpec {
    #[serde(default)]
    directory: Option<String>,
    #[serde(default, rename = "projectDirectory")]
    project_directory: Option<String>,
}

#[tokio::test]
async fn patch_corpus_matches() {
    let cases: Vec<PatchCase> =
        serde_json::from_str(include_str!("../corpus/patch.json")).expect("the patch corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("patch-{index}"));
        let outside = unique_directory(&format!("patch-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);
        let directory = case
            .location
            .as_ref()
            .and_then(|location| location.directory.as_deref())
            .map_or_else(|| root.clone(), |directory| PathBuf::from(substitute(directory, &root, &outside)));
        let project = case
            .location
            .as_ref()
            .and_then(|location| location.project_directory.as_deref())
            .map_or_else(|| directory.clone(), |project| PathBuf::from(substitute(project, &root, &outside)));

        let mut recording =
            RecordingAuthorizer::new(case.permission.allow.unwrap_or(true), case.permission.denied_resources.clone());
        recording.deny_actions(case.permission.denied_actions.clone());
        let recording = Arc::new(recording);
        let authorizer: Arc<dyn Authorizer> = recording.clone();
        let access = Access::new(
            directory.to_string_lossy().into_owned(),
            project.to_string_lossy().into_owned(),
            std::env::var("HOME").unwrap_or_else(|_| "/".to_owned()),
        );
        let deps = PatchDeps::new(access, Mutation::new(), authorizer, Arc::new(NoFormat));
        let (registry, _registration, snapshot) = activate(deps).await;
        let context = context_for(index, recording.clone());
        let result = snapshot
            .execute(
                ToolCall {
                    id: format!("call-{index}"),
                    name: "patch".to_owned(),
                    input: replace_placeholders(case.call.clone(), &root, &outside),
                },
                &context,
            )
            .await;
        let actual = result_value(result, recording.as_ref(), &root, &outside);
        assert_eq!(actual, expected, "case {index} ({})", case.name);
        drop(registry);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}

#[tokio::test]
async fn formatter_changes_the_final_file_and_diff() {
    let root = unique_directory("patch-format");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = deps_for(&root, authorizer.clone(), Arc::new(ReplaceCreated));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    let result = execute(&snapshot, &context, "*** Begin Patch\n*** Add File: formatted.txt\n+created\n*** End Patch")
        .await
        .expect("the patch completes");
    let files = result.metadata.as_ref().and_then(|metadata| metadata.get("files")).and_then(Value::as_array);
    let patch = files.and_then(|files| files.first()).and_then(|file| file.get("patch")).and_then(Value::as_str);
    assert!(patch.is_some_and(|patch| patch.contains("+FORMATTED")), "{patch:?}");
    assert_eq!(std::fs::read_to_string(root.join("formatted.txt")).unwrap(), "FORMATTED\n");
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn formatting_does_not_invent_a_bom_diff_line() {
    let root = unique_directory("patch-bom");
    std::fs::write(root.join("example.cs"), "\u{feff}using System;\n\nclass Test {}\n")
        .expect("the fixture is written");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = deps_for(&root, authorizer.clone(), Arc::new(StripBom));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    let result = execute(
        &snapshot,
        &context,
        "*** Begin Patch\n*** Update File: example.cs\n@@\n class Test {}\n+class Next {}\n*** End Patch",
    )
    .await
    .expect("the patch completes");
    let files = result.metadata.as_ref().and_then(|metadata| metadata.get("files")).and_then(Value::as_array);
    let patch = files
        .and_then(|files| files.first())
        .and_then(|file| file.get("patch"))
        .and_then(Value::as_str)
        .expect("the diff is present");
    assert!(patch.contains("+class Next {}"));
    assert!(!patch.contains('\u{feff}'));
    assert!(!patch.contains("-using System;"));
    assert!(!patch.contains("+using System;"));
    assert_eq!(
        std::fs::read_to_string(root.join("example.cs")).unwrap(),
        "\u{feff}using System;\n\nclass Test {}\nclass Next {}\n"
    );
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn serializes_concurrent_patch_transactions() {
    let root = unique_directory("patch-concurrent");
    std::fs::write(root.join("concurrent.txt"), "one\ntwo\n").expect("the fixture is written");
    let deps = deps_for(&root, Arc::new(SlowAuthorizer), Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, Arc::new(SlowAuthorizer));
    let first = snapshot.execute(
        ToolCall {
            id: "call-patch-one".to_owned(),
            name: "patch".to_owned(),
            input: json!({ "patchText": "*** Begin Patch\n*** Update File: concurrent.txt\n@@\n-one\n+ONE\n*** End Patch" }),
        },
        &context,
    );
    let second = snapshot.execute(
        ToolCall {
            id: "call-patch-two".to_owned(),
            name: "patch".to_owned(),
            input: json!({ "patchText": "*** Begin Patch\n*** Update File: concurrent.txt\n@@\n-two\n+TWO\n*** End Patch" }),
        },
        &context,
    );
    let (first, second) = futures_util::future::join(first, second).await;
    first.expect("the first patch completes");
    second.expect("the second patch completes");
    assert_eq!(std::fs::read_to_string(root.join("concurrent.txt")).unwrap(), "ONE\nTWO\n");
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn serializes_a_relative_and_an_absolute_patch_transaction() {
    let root = unique_directory("patch-concurrent-paths");
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("the fixture is written");
    let deps = deps_for(&root, Arc::new(SlowAuthorizer), Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, Arc::new(SlowAuthorizer));
    let absolute = root.join("a.txt").to_string_lossy().into_owned();
    let first = snapshot.execute(
        ToolCall {
            id: "call-patch-relative".to_owned(),
            name: "patch".to_owned(),
            input: json!({ "patchText": "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n*** End Patch" }),
        },
        &context,
    );
    let second = snapshot.execute(
        ToolCall {
            id: "call-patch-absolute".to_owned(),
            name: "patch".to_owned(),
            input: json!({
                "patchText": format!("*** Begin Patch\n*** Update File: {absolute}\n@@\n-two\n+TWO\n*** End Patch")
            }),
        },
        &context,
    );
    let (first, second) = futures_util::future::join(first, second).await;
    first.expect("the relative patch completes");
    second.expect("the absolute patch completes");
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "ONE\nTWO\n");
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn overwrites_an_add_target_that_appears_during_approval() {
    let root = unique_directory("patch-appears");
    let authorizer = Arc::new(AppearingAuthorizer {
        inner: RecordingAuthorizer::new(true, Vec::new()),
        path: root.join("appeared.txt"),
    });
    let deps = deps_for(&root, authorizer.clone(), Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    execute(&snapshot, &context, "*** Begin Patch\n*** Add File: appeared.txt\n+replacement\n*** End Patch")
        .await
        .expect("the patch completes");
    assert_eq!(std::fs::read_to_string(root.join("appeared.txt")).unwrap(), "replacement\n");
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn reports_a_write_failure_with_the_completed_prefix() {
    let root = unique_directory("patch-fail-write");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = faulty(&root, authorizer.clone(), Some("second.txt"), None, Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    let error = execute(
        &snapshot,
        &context,
        "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
    )
    .await
    .expect_err("the second write fails");
    assert_eq!(error.message, "Failed to write second.txt: forced write failure. Completed before failure: first.txt");
    assert_eq!(std::fs::read_to_string(root.join("first.txt")).unwrap(), "first\n");
    assert!(!root.join("second.txt").exists());
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn reports_the_failing_move_destination() {
    let root = unique_directory("patch-fail-move");
    std::fs::write(root.join("old.txt"), "before\n").expect("the fixture is written");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = faulty(&root, authorizer.clone(), Some("new.txt"), None, Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    let error = execute(
        &snapshot,
        &context,
        "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-before\n+after\n*** End Patch",
    )
    .await
    .expect_err("the move write fails");
    assert_eq!(error.message, "Failed to write new.txt: forced write failure");
    assert_eq!(std::fs::read_to_string(root.join("old.txt")).unwrap(), "before\n");
    assert!(!root.join("new.txt").exists());
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn reports_a_destination_written_before_move_removal_fails() {
    let root = unique_directory("patch-fail-remove");
    std::fs::write(root.join("old.txt"), "before\n").expect("the fixture is written");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = faulty(&root, authorizer.clone(), None, Some("old.txt"), Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    let error = execute(
        &snapshot,
        &context,
        "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-before\n+after\n*** End Patch",
    )
    .await
    .expect_err("the removal fails");
    assert_eq!(error.message, "Wrote new.txt but failed to remove old.txt: forced remove failure");
    assert_eq!(std::fs::read_to_string(root.join("old.txt")).unwrap(), "before\n");
    assert_eq!(std::fs::read_to_string(root.join("new.txt")).unwrap(), "after\n");
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

#[tokio::test]
async fn stops_after_an_earlier_delete_when_a_later_delete_fails() {
    let root = unique_directory("patch-delete-failure");
    std::fs::write(root.join("first.txt"), "first").expect("the first fixture is written");
    std::fs::write(root.join("second.txt"), "second").expect("the second fixture is written");
    let authorizer = Arc::new(RecordingAuthorizer::new(true, Vec::new()));
    let deps = faulty(&root, authorizer.clone(), None, Some("second.txt"), Arc::new(NoFormat));
    let (_registry, _registration, snapshot) = activate(deps).await;
    let context = context_for(0, authorizer);
    execute(
        &snapshot,
        &context,
        "*** Begin Patch\n*** Delete File: first.txt\n*** Delete File: second.txt\n*** End Patch",
    )
    .await
    .expect_err("the second delete fails");
    assert!(!root.join("first.txt").exists());
    assert!(root.join("second.txt").exists());
    std::fs::remove_dir_all(&root).expect("the test directory is removed");
}

async fn activate(deps: PatchDeps) -> (ToolRegistry, opencode_location::state::Registration, Snapshot) {
    let registry = ToolRegistry::new();
    let registration = builtin::activate(&registry, &BuiltinDeps { patch: Some(deps), ..BuiltinDeps::default() }).await;
    let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
    (registry, registration, snapshot)
}

fn deps_for(root: &Path, authorizer: Arc<dyn Authorizer>, format: Arc<dyn FormatHook>) -> PatchDeps {
    let directory = root.to_string_lossy().into_owned();
    PatchDeps::new(
        Access::new(directory.clone(), directory, std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())),
        Mutation::new(),
        authorizer,
        format,
    )
}

fn faulty(
    root: &Path,
    authorizer: Arc<dyn Authorizer>,
    fail_write: Option<&str>,
    fail_remove: Option<&str>,
    format: Arc<dyn FormatHook>,
) -> PatchDeps {
    let mut deps = deps_for(root, authorizer, format);
    deps.files = Arc::new(FaultyFiles {
        inner: HostFiles::new(deps.mutation.clone()),
        fail_write: fail_write.map(str::to_owned),
        fail_remove: fail_remove.map(str::to_owned),
    });
    deps
}

fn context_for(index: usize, authorizer: Arc<dyn Authorizer>) -> ToolContext {
    ToolContext {
        session_id: "ses_patch_tool_test".to_owned(),
        agent: "build".to_owned(),
        message_id: "msg_tool_test".to_owned(),
        call_id: format!("call-{index}"),
        authorizer,
        ..ToolContext::default()
    }
}

async fn execute(
    snapshot: &Snapshot,
    context: &ToolContext,
    patch_text: &str,
) -> Result<ToolOutput, opencode_tool::ToolError> {
    snapshot
        .execute(
            ToolCall {
                id: context.call_id.clone(),
                name: "patch".to_owned(),
                input: json!({ "patchText": patch_text }),
            },
            context,
        )
        .await
}

struct ReplaceCreated;

impl FormatHook for ReplaceCreated {
    fn file<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let text = std::fs::read_to_string(&target.absolute).expect("the formatted file is readable");
            std::fs::write(&target.absolute, text.replace("created", "FORMATTED")).expect("the file is formatted");
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

struct SlowAuthorizer;

impl Authorizer for SlowAuthorizer {
    fn assert<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

struct AppearingAuthorizer {
    inner: RecordingAuthorizer,
    path: PathBuf,
}

impl Authorizer for AppearingAuthorizer {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move {
            if input.action == "edit" {
                std::fs::write(&self.path, "winner\n").expect("the appearing target is written");
            }
            self.inner.assert(input).await
        })
    }

    fn allows<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        self.inner.allows(input)
    }
}

struct FaultyFiles {
    inner: HostFiles,
    fail_write: Option<String>,
    fail_remove: Option<String>,
}

impl PatchFiles for FaultyFiles {
    fn write<'a>(&'a self, target: &'a Target, content: &'a [u8]) -> BoxFuture<'a, Result<(), FsError>> {
        if self.fail_write.as_deref().is_some_and(|name| basename(&target.absolute) == name) {
            let path = target.absolute.clone();
            return Box::pin(async move { Err(FsError::Failed { path, cause: "forced write failure".to_owned() }) });
        }
        self.inner.write(target, content)
    }

    fn remove<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Result<(), FsError>> {
        if self.fail_remove.as_deref().is_some_and(|name| basename(absolute) == name) {
            let path = absolute.to_owned();
            return Box::pin(async move { Err(FsError::Failed { path, cause: "forced remove failure".to_owned() }) });
        }
        self.inner.remove(absolute)
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), FsError>> {
        self.inner.rename(from, to)
    }

    fn kind<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Option<EntryKind>> {
        self.inner.kind(absolute)
    }
}

fn basename(path: &str) -> String {
    Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}
