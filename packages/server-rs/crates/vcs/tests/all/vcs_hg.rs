use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use opencode_bus::{Definition, PublishOptions};
use opencode_contract::generated::requests::VcsMode;
use opencode_contract::generated::{
    AbsolutePath, FileDiffInfoStatus, LocationRef, VcsBranch, VcsFileStatus, VcsFileStatusStatus, VcsInfo,
};
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_location::state::Registration;
use opencode_vcs::hg_provider::HgProvider;
use opencode_vcs::vcs::{DiffOptions, Vcs};
use serde_json::json;

use crate::support::Fixture;

async fn hg(directory: &Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("hg")
        .args(args)
        .current_dir(directory)
        .env("HGPLAIN", "1")
        .output()
        .await
        .expect("mercurial runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "hg {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

async fn hg_available() -> bool {
    tokio::process::Command::new("hg").arg("--version").output().await.is_ok_and(|output| output.status.success())
}

async fn hg_init(directory: &Path) {
    tokio::fs::create_dir_all(directory).await.expect("creates the repository directory");
    hg(directory, &["init"]).await;
}

async fn commit_all(directory: &Path, message: &str) {
    hg(directory, &["addremove", "-q"]).await;
    hg(directory, &["commit", "-q", "-m", message, "-u", "test"]).await;
}

fn hg_location(directory: &Path) -> LocationInfo {
    let text = directory.to_string_lossy().into_owned();
    LocationInfo {
        directory: text.clone(),
        workspace_id: None,
        project: LocationProject { id: "project".to_owned(), directory: text.clone(), canonical: text },
        vcs: Some(opencode_location::project::Vcs::Hg { store: directory.join(".hg").to_string_lossy().into_owned() }),
    }
}

async fn hg_service(fixture: &Fixture, directory: &Path) -> (Arc<Vcs>, Registration) {
    let info = hg_location(directory);
    let vcs = Vcs::new(fixture.bus.clone(), &info);
    let text = directory.to_string_lossy().into_owned();
    let registration =
        vcs.transform(move |editor| editor.add(Arc::new(HgProvider::new(text.clone(), text.clone())))).await;
    (vcs, registration)
}

fn location_ref(info: &LocationInfo) -> LocationRef {
    LocationRef { directory: AbsolutePath(info.directory.clone()), workspace_id: None }
}

fn publish_changed(fixture: &Fixture, location: &LocationRef, file: &Path) {
    fixture
        .bus
        .publish(
            &Definition::ephemeral("filesystem.changed"),
            json!({ "file": file.to_string_lossy(), "event": "change" }),
            PublishOptions { location: Some(location.clone()), ..Default::default() },
        )
        .expect("publishes a filesystem change");
}

fn vcs_info(current: &str, default: &str) -> VcsInfo {
    VcsInfo {
        provider: Some("hg".to_owned()),
        branch: VcsBranch { current: Some(current.to_owned()), default: Some(default.to_owned()) },
    }
}

#[tokio::test]
async fn mercurial_rejects_unsupported_review_modes_and_bases() {
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    tokio::fs::create_dir_all(&directory).await.expect("creates the project directory");
    let (vcs, _registration) = hg_service(&fixture, &directory).await;

    assert_eq!(vcs.base().await.expect("resolves no base"), None);
    let committed = vcs.diff(VcsMode::Committed, DiffOptions::default()).await.expect_err("rejects committed reviews");
    assert_eq!(committed.message, "The Mercurial provider does not support committed reviews or explicit bases");
    let explicit = vcs
        .diff(VcsMode::Branch, DiffOptions { base: Some("release".to_owned()), ..DiffOptions::default() })
        .await
        .expect_err("rejects explicit bases");
    assert_eq!(explicit.message, "The Mercurial provider does not support committed reviews or explicit bases");
}

#[tokio::test]
async fn reports_modified_missing_and_untracked_files() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    hg_init(&directory).await;
    fixture.write(&directory.join("keep.txt"), "one\ntwo\n").await;
    fixture.write(&directory.join("gone.txt"), "bye\n").await;
    commit_all(&directory, "initial").await;
    fixture.write(&directory.join("keep.txt"), "one\nthree\n").await;
    tokio::fs::remove_file(directory.join("gone.txt")).await.expect("removes the tracked file");
    fixture.write(&directory.join("new.txt"), "hello\nworld\n").await;
    let (vcs, _registration) = hg_service(&fixture, &directory).await;

    assert_eq!(
        vcs.status().await,
        [
            VcsFileStatus {
                file: "gone.txt".to_owned(),
                additions: 0.0,
                deletions: 1.0,
                status: VcsFileStatusStatus::Deleted,
            },
            VcsFileStatus {
                file: "keep.txt".to_owned(),
                additions: 1.0,
                deletions: 1.0,
                status: VcsFileStatusStatus::Modified,
            },
            VcsFileStatus {
                file: "new.txt".to_owned(),
                additions: 2.0,
                deletions: 0.0,
                status: VcsFileStatusStatus::Added,
            },
        ]
    );
}

#[tokio::test]
async fn diffs_the_working_copy_with_synthesized_untracked_and_missing_patches() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    hg_init(&directory).await;
    fixture.write(&directory.join("keep.txt"), "one\ntwo\n").await;
    fixture.write(&directory.join("gone.txt"), "bye\n").await;
    commit_all(&directory, "initial").await;
    fixture.write(&directory.join("keep.txt"), "one\nthree\n").await;
    tokio::fs::remove_file(directory.join("gone.txt")).await.expect("removes the tracked file");
    fixture.write(&directory.join("spaced name.txt"), "hello\n").await;
    let (vcs, _registration) = hg_service(&fixture, &directory).await;

    let diff = vcs.diff(VcsMode::Working, DiffOptions::default()).await.expect("diffs the working copy");
    let rows = diff.iter().map(|row| (row.file.as_str(), row.status)).collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("gone.txt", FileDiffInfoStatus::Deleted),
            ("keep.txt", FileDiffInfoStatus::Modified),
            ("spaced name.txt", FileDiffInfoStatus::Added),
        ]
    );
    assert!(diff[0].patch.contains("-bye"));
    assert!(diff[1].patch.contains("-two"));
    assert!(diff[1].patch.contains("+three"));
    assert_eq!(diff[1].additions, 1.0);
    assert_eq!(diff[1].deletions, 1.0);
    assert!(diff[2].patch.contains("+hello"));
    assert_eq!(diff[2].additions, 1.0);
}

#[tokio::test]
async fn caches_branch_info_and_publishes_branch_metadata_changes() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    hg_init(&directory).await;
    fixture.write(&directory.join("file.txt"), "one\n").await;
    commit_all(&directory, "initial").await;
    let (vcs, _registration) = hg_service(&fixture, &directory).await;
    assert_eq!(vcs.info().await, vcs_info("default", "default"));

    let info = hg_location(&directory);
    let location = location_ref(&info);
    let mut updates = fixture.bus.subscribe(&["vcs.branch.updated"], Some(location.clone()));

    hg(&directory, &["branch", "-q", "feature"]).await;
    assert_eq!(vcs.info().await, vcs_info("default", "default"));

    publish_changed(&fixture, &location, &directory.join(".hg").join("branch"));
    let event = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .expect("publishes a branch update")
        .expect("receives a branch update");
    assert_eq!(event.data()["branch"], json!("feature"));
    assert_eq!(vcs.info().await, vcs_info("feature", "default"));
}

#[tokio::test]
async fn respects_the_context_option() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    hg_init(&directory).await;
    let body = (0..20).map(|index| format!("line-{index}")).collect::<Vec<_>>().join("\n") + "\n";
    fixture.write(&directory.join("file.txt"), &body).await;
    commit_all(&directory, "initial").await;
    fixture.write(&directory.join("file.txt"), &body.replace("line-10", "changed")).await;
    let (vcs, _registration) = hg_service(&fixture, &directory).await;

    let full = vcs.diff(VcsMode::Working, DiffOptions::default()).await.expect("diffs the working copy");
    assert!(full[0].patch.contains("line-0"));
    assert!(full[0].patch.contains("line-19"));
    let tight = vcs
        .diff(VcsMode::Working, DiffOptions { context: Some(1), ..DiffOptions::default() })
        .await
        .expect("diffs the working copy with a narrow context");
    assert!(tight[0].patch.contains("line-9"));
    assert!(!tight[0].patch.contains("line-0"));
}

#[tokio::test]
async fn diffs_before_the_first_commit() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    let fixture = Fixture::new();
    let directory = fixture.path("project");
    hg_init(&directory).await;
    fixture.write(&directory.join("tracked.txt"), "a\nb\n").await;
    hg(&directory, &["add", "-q", "tracked.txt"]).await;
    fixture.write(&directory.join("loose.txt"), "hello\n").await;
    let (vcs, _registration) = hg_service(&fixture, &directory).await;

    assert_eq!(
        vcs.status().await,
        [
            VcsFileStatus {
                file: "loose.txt".to_owned(),
                additions: 1.0,
                deletions: 0.0,
                status: VcsFileStatusStatus::Added,
            },
            VcsFileStatus {
                file: "tracked.txt".to_owned(),
                additions: 2.0,
                deletions: 0.0,
                status: VcsFileStatusStatus::Added,
            },
        ]
    );
    let diff = vcs.diff(VcsMode::Working, DiffOptions::default()).await.expect("diffs the working copy");
    assert_eq!(diff.len(), 2);
    assert!(diff[0].patch.contains("+hello"));
    assert!(diff[1].patch.contains("+a"));
}

#[tokio::test]
async fn diffs_a_named_branch_against_the_default_branch() {
    if !hg_available().await {
        eprintln!("mercurial is not installed; skipping");
        return;
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let fixture = Fixture::new();
        let directory = fixture.path("project");
        hg_init(&directory).await;
        fixture.write(&directory.join("file.txt"), "one\n").await;
        commit_all(&directory, "initial").await;
        let (vcs, _registration) = hg_service(&fixture, &directory).await;
        assert!(vcs.diff(VcsMode::Branch, DiffOptions::default()).await.expect("diffs the default branch").is_empty());

        hg(&directory, &["branch", "-q", "feature"]).await;
        fixture.write(&directory.join("file.txt"), "one\ntwo\n").await;
        commit_all(&directory, "feature change").await;

        let diff = vcs.diff(VcsMode::Branch, DiffOptions::default()).await.expect("diffs the named branch");
        let rows = diff.iter().map(|row| (row.file.as_str(), row.status)).collect::<Vec<_>>();
        assert_eq!(rows, [("file.txt", FileDiffInfoStatus::Modified)]);
        assert!(diff[0].patch.contains("+two"));
    })
    .await
    .expect("diffs the named branch within the timeout");
}
