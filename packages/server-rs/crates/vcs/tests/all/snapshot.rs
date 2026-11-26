use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opencode_git::{Git, RunOptions};
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_location::project::Vcs;
use opencode_util::path::realpath;
use opencode_util::temp::TempDir;
use opencode_vcs::snapshot::{DiffInput, RepositoryFuture, RestoreInput, Snapshot, SnapshotOperation};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

const REPOSITORY_CONFIG: &str = "\n[core]\n\tfsmonitor = false\n[commit]\n\tgpgsign = false\n[gc]\n\tauto = 0\n[maintenance]\n\tauto = false\n[user]\n\temail = test@opencode.test\n\tname = Test\n";

#[tokio::test]
async fn keeps_lazy_repository_discovery_after_the_first_caller_is_interrupted() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("tracked.txt"), "one\n").await;
    init_git(&git, &project, false).await;

    let discoveries = Arc::new(AtomicUsize::new(0));
    let provider_discoveries = discoveries.clone();
    let (started, mut started_rx) = watch::channel(false);
    let (release, release_rx) = watch::channel(false);
    let snapshot = Arc::new(Snapshot::new_with_repository(
        git.clone(),
        &location(&project, &project, "project", true),
        tmp.path(),
        move |inner| {
            let discoveries = provider_discoveries.clone();
            let started = started.clone();
            let release = release_rx.clone();
            Arc::new(move || -> RepositoryFuture {
                let inner = inner.clone();
                let discoveries = discoveries.clone();
                let started = started.clone();
                let mut release = release.clone();
                Box::pin(async move {
                    discoveries.fetch_add(1, Ordering::SeqCst);
                    started.send_replace(true);
                    release.wait_for(|value| *value).await.expect("the release sender is alive");
                    inner().await
                })
            })
        },
    ));

    let first = tokio::spawn({
        let snapshot = snapshot.clone();
        async move { snapshot.capture().await }
    });
    started_rx.wait_for(|value| *value).await.expect("the provider reports starting");
    assert_eq!(discoveries.load(Ordering::SeqCst), 1);
    first.abort();
    assert!(first.await.is_err());

    let second = tokio::spawn({
        let snapshot = snapshot.clone();
        async move { snapshot.capture().await }
    });
    release.send_replace(true);
    assert!(second.await.expect("the capture task joins").is_some());
    assert_eq!(discoveries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_panicking_repository_provider_resolves_waiters_with_an_error() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    let snapshot =
        Snapshot::new_with_repository(git, &location(&project, &project, "project", true), tmp.path(), |_| {
            Arc::new(|| -> RepositoryFuture { Box::pin(async { panic!("repository discovery panicked") }) })
        });

    let result = tokio::time::timeout(Duration::from_secs(5), snapshot.files("one", "two"))
        .await
        .expect("waiters resolve instead of hanging");
    let error = result.expect_err("the panicking provider fails the caller");
    assert_eq!(error.operation, SnapshotOperation::Files);
}

#[tokio::test]
async fn captures_and_restores_location_scoped_changes() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let scope = project.join("scope");
    let git = handle();
    write(&scope.join("tracked.txt"), "one\n").await;
    write(&project.join("outside.txt"), "outside\n").await;
    init_git(&git, &project, false).await;

    let snapshot = build(tmp.path(), &scope, &project, true);
    let before = snapshot.capture().await.expect("captures the first tree");

    write(&scope.join("tracked.txt"), "two\n").await;
    write(&scope.join("added.txt"), "added\n").await;
    write(&project.join("outside.txt"), "changed outside\n").await;
    let after = snapshot.capture().await.expect("captures the second tree");

    assert_eq!(
        snapshot.files(&before, &after).await.expect("lists the changed files"),
        ["scope/added.txt", "scope/tracked.txt"]
    );

    let files = vec![("scope/tracked.txt".to_owned(), before)];
    snapshot.restore(RestoreInput { files: &files }).await.expect("restores the selection");
    assert_eq!(read(&scope.join("tracked.txt")).await, "one\n");
    assert_eq!(read(&scope.join("added.txt")).await, "added\n");
    assert_eq!(read(&project.join("outside.txt")).await, "changed outside\n");
}

#[tokio::test]
async fn recovers_from_a_corrupt_index_and_ignores_a_stale_index_lock() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("tracked.txt"), "one\n").await;
    init_git(&git, &project, true).await;

    let snapshot = build(tmp.path(), &project, &project, true);
    let before = snapshot.capture().await.expect("captures the first tree");
    let store = snapshot_directory(tmp.path());

    tokio::fs::write(store.join("index"), vec![0u8; 512]).await.expect("corrupts the index");
    tokio::fs::write(store.join("index.lock"), "").await.expect("leaves a stale index lock");
    write(&project.join("tracked.txt"), "two\n").await;

    let after = snapshot.capture().await.expect("captures the second tree");
    assert_eq!(snapshot.files(&before, &after).await.expect("lists the changed files"), ["tracked.txt"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_concurrently_from_independent_processes() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    for index in 0..200 {
        let content = format!("{index}\n");
        write(&project.join(format!("f{index}.txt")), &content).await;
    }
    init_git(&git, &project, true).await;

    let writers = (0..3).map(|id| tokio::spawn(writer(tmp.path().to_owned(), project.clone(), id))).collect::<Vec<_>>();
    for writer in writers {
        writer.await.expect("the writer task joins");
    }
}

#[tokio::test]
async fn captures_a_location_in_a_directory_whose_name_starts_with_two_dots() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let scope = project.join("..scope");
    let git = handle();
    write(&scope.join("tracked.txt"), "one\n").await;
    init_git(&git, &project, false).await;

    let snapshot = build(tmp.path(), &scope, &project, true);
    let before = snapshot.capture().await.expect("captures the first tree");
    write(&scope.join("tracked.txt"), "two\n").await;
    let after = snapshot.capture().await.expect("captures the second tree");

    assert_eq!(snapshot.files(&before, &after).await.expect("lists the changed files"), ["..scope/tracked.txt"]);
}

#[tokio::test]
async fn captures_and_restores_a_location_opened_through_a_symlinked_path() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let link = tmp.path().join("link");
    let git = handle();
    write(&project.join("scope/tracked.txt"), "one\n").await;
    init_git(&git, &project, false).await;
    link_directory(&project, &link);

    let snapshot = build(tmp.path(), &link.join("scope"), &project, true);
    let before = snapshot.capture().await.expect("captures the first tree");
    write(&link.join("scope/tracked.txt"), "two\n").await;
    let after = snapshot.capture().await.expect("captures the second tree");

    assert_eq!(snapshot.files(&before, &after).await.expect("lists the changed files"), ["scope/tracked.txt"]);

    let files = vec![("scope/tracked.txt".to_owned(), before)];
    snapshot.restore(RestoreInput { files: &files }).await.expect("restores the selection");
    assert_eq!(read(&project.join("scope/tracked.txt")).await, "one\n");
}

#[cfg(windows)]
#[tokio::test]
async fn captures_a_location_opened_with_different_letter_case() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("scope/tracked.txt"), "one\n").await;
    init_git(&git, &project, false).await;

    let snapshot = build(tmp.path(), &project.join("SCOPE"), &project, true);
    let before = snapshot.capture().await.expect("captures the first tree");
    write(&project.join("scope/tracked.txt"), "two\n").await;
    let after = snapshot.capture().await.expect("captures the second tree");

    assert_eq!(snapshot.files(&before, &after).await.expect("lists the changed files"), ["scope/tracked.txt"]);
}

#[tokio::test]
async fn restores_many_files_from_several_trees_and_removes_paths_absent_from_them() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("a.txt"), "a1\n").await;
    write(&project.join("b[1].txt"), "b1\n").await;
    init_git(&git, &project, true).await;

    let snapshot = build(tmp.path(), &project, &project, true);
    let first = snapshot.capture().await.expect("captures the first tree");
    write(&project.join("a.txt"), "a2\n").await;
    write(&project.join("c.txt"), "c2\n").await;
    let second = snapshot.capture().await.expect("captures the second tree");
    write(&project.join("a.txt"), "a3\n").await;
    write(&project.join("b[1].txt"), "b3\n").await;
    write(&project.join("c.txt"), "c3\n").await;
    write(&project.join("d.txt"), "d3\n").await;

    let files = vec![
        ("a.txt".to_owned(), second.clone()),
        ("b[1].txt".to_owned(), first.clone()),
        ("c.txt".to_owned(), first),
        ("d.txt".to_owned(), second),
    ];
    snapshot.restore(RestoreInput { files: &files }).await.expect("restores the selection");
    assert_eq!(read(&project.join("a.txt")).await, "a2\n");
    assert_eq!(read(&project.join("b[1].txt")).await, "b1\n");
    assert!(!exists(&project.join("c.txt")).await);
    assert!(!exists(&project.join("d.txt")).await);
}

#[tokio::test]
async fn restores_and_diffs_a_selection_too_long_for_one_command_line() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let names = (0..300).map(|index| format!("deep/{}-{index}.txt", "n".repeat(90))).collect::<Vec<_>>();
    let git = handle();
    for name in &names {
        write(&project.join(name), "one\n").await;
    }
    init_git(&git, &project, true).await;

    let snapshot = build(tmp.path(), &project, &project, true);
    let first = snapshot.capture().await.expect("captures the first tree");
    for name in &names {
        write(&project.join(name), "two\n").await;
    }
    let second = snapshot.capture().await.expect("captures the second tree");

    let diffs = snapshot
        .diff(DiffInput { from: &first, to: &second, context: None, paths: Some(&names) })
        .await
        .expect("diffs the selection");
    let mut changed = diffs.iter().map(|diff| diff.file.clone()).collect::<Vec<_>>();
    changed.sort();
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(changed, expected);
    assert!(diffs.iter().all(|diff| diff.additions == 1 && diff.deletions == 1 && diff.patch.contains("+two")));

    let files = names.iter().map(|name| (name.clone(), first.clone())).collect::<Vec<_>>();
    snapshot.restore(RestoreInput { files: &files }).await.expect("restores the selection");
    for name in &names {
        assert_eq!(read(&project.join(name)).await, "one\n");
    }
}

#[tokio::test]
async fn restores_the_other_files_when_removing_one_path_fails() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("c.txt"), "old\n").await;
    init_git(&git, &project, true).await;

    let snapshot = build(tmp.path(), &project, &project, true);
    let first = snapshot.capture().await.expect("captures the first tree");
    write(&project.join("c.txt"), "changed\n").await;
    write(&project.join("a"), "file\n").await;

    let files = vec![("a/b".to_owned(), first.clone()), ("c.txt".to_owned(), first)];
    assert!(snapshot.restore(RestoreInput { files: &files }).await.is_err());
    assert_eq!(read(&project.join("c.txt")).await, "old\n");
}

#[tokio::test]
async fn applies_availability_transforms() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let git = handle();
    write(&project.join("tracked.txt"), "one\n").await;
    init_git(&git, &project, false).await;

    let snapshot = build(tmp.path(), &project, &project, true);
    let registration = snapshot.transform(|editor| editor.configure(false)).await;
    assert!(snapshot.capture().await.is_none());

    registration.dispose().await;
    assert!(snapshot.capture().await.is_some());
}

#[tokio::test]
async fn treats_capture_outside_git_as_unavailable() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let snapshot = build(tmp.path(), tmp.path(), tmp.path(), false);
    assert!(snapshot.capture().await.is_none());
}

#[tokio::test]
async fn isolates_snapshot_indexes_by_canonical_git_worktree() {
    let tmp = TempDir::new().expect("creates a temporary directory");
    let project = tmp.path().join("project");
    let linked = tmp.path().join("linked");
    let git = handle();
    write(&project.join("tracked.txt"), "main\n").await;
    init_git(&git, &project, true).await;
    self::git(&git, &project, &["-c", "core.fsmonitor=false", "worktree", "add", "--detach", text(&linked), "HEAD"])
        .await;

    assert!(build(tmp.path(), &project, &project, true).capture().await.is_some());
    assert!(build(tmp.path(), &linked, &linked, true).capture().await.is_some());

    let store = tmp.path().join("snapshot").join("project");
    let project_path = realpath(&project).expect("resolves the project");
    let linked_path = realpath(&linked).expect("resolves the linked worktree");
    assert!(store.join(sha1_hex(text(&project_path))).is_dir());
    assert!(store.join(sha1_hex(text(&linked_path))).is_dir());
}

async fn writer(data: PathBuf, project: PathBuf, id: usize) {
    let snapshot = build(&data, &project, &project, true);
    for index in 0..8 {
        let content = format!("{index}\n");
        write(&project.join(format!("writer-{id}.txt")), &content).await;
        assert!(snapshot.capture().await.is_some());
    }
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path is UTF-8")
}

fn handle() -> Arc<Git> {
    Arc::new(Git::new(Arc::new(|name: &str| std::env::var_os(name))))
}

async fn git(git: &Git, directory: &Path, args: &[&str]) {
    let options =
        RunOptions { cwd: Some(text(directory).to_owned()), env: crate::support::identity_env(), ..Default::default() };
    let output = git.execute(args, options).await.expect("git starts");
    assert_eq!(output.status, 0, "git {} failed: {}", args.join(" "), output.stderr_text());
}

async fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.expect("creates the parent directory");
    }
    tokio::fs::write(path, content).await.expect("writes the file");
}

async fn read(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.expect("reads the file").replace("\r\n", "\n")
}

async fn exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

async fn init_git(git: &Git, directory: &Path, commit: bool) {
    tokio::fs::create_dir_all(directory).await.expect("creates the project directory");
    self::git(git, directory, &["init"]).await;
    let mut config = tokio::fs::OpenOptions::new()
        .append(true)
        .open(directory.join(".git").join("config"))
        .await
        .expect("opens the repository config");
    config.write_all(REPOSITORY_CONFIG.as_bytes()).await.expect("appends the repository config");
    self::git(git, directory, &["-c", "core.fsmonitor=false", "add", "."]).await;
    if !commit {
        return;
    }
    self::git(
        git,
        directory,
        &["-c", "user.email=test@opencode.test", "-c", "user.name=Test", "commit", "--no-gpg-sign", "-m", "initial"],
    )
    .await;
}

fn location(directory: &Path, project: &Path, id: &str, git: bool) -> LocationInfo {
    LocationInfo {
        directory: text(directory).to_owned(),
        workspace_id: None,
        project: LocationProject {
            id: id.to_owned(),
            directory: text(project).to_owned(),
            canonical: text(project).to_owned(),
        },
        vcs: git.then(|| Vcs::Git { store: text(&project.join(".git")).to_owned() }),
    }
}

fn build(data: &Path, directory: &Path, project: &Path, git: bool) -> Arc<Snapshot> {
    Arc::new(Snapshot::new(handle(), &location(directory, project, "project", git), data))
}

fn snapshot_directory(data: &Path) -> PathBuf {
    let project = std::fs::read_dir(data.join("snapshot"))
        .expect("lists the snapshot projects")
        .next()
        .expect("has a snapshot project")
        .expect("reads the snapshot project")
        .path();
    std::fs::read_dir(&project)
        .expect("lists the snapshot repositories")
        .next()
        .expect("has a snapshot repository")
        .expect("reads the snapshot repository")
        .path()
}

fn sha1_hex(value: &str) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, value.as_bytes());
    hash.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(unix)]
fn link_directory(source: &Path, link: &Path) {
    std::os::unix::fs::symlink(source, link).expect("creates the directory symlink");
}

#[cfg(windows)]
fn link_directory(source: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(source, link).expect("creates the directory symlink");
}
