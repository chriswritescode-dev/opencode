use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use opencode_git::{CaptureInput, FileDiffStatus, Git, Repository, RunOptions};

use crate::fixture;
use crate::{TempDir, text};

struct Setup {
    project: PathBuf,
    source: Repository,
    store: Repository,
}

async fn setup(git: &Git, temp: &TempDir, files: &[(&str, &str)]) -> Setup {
    let project = temp.path().join("project");
    fixture::init_repo(git, &project).await;
    for (name, content) in files {
        write(project.join(name), content).await;
    }
    fixture::git(git, &project, &["add", "."]).await;
    fixture::git(git, &project, &["commit", "-q", "-m", "initial"]).await;
    let source = git.discover(text(&project)).await.expect("discovers the project");
    let storage = temp.path().join("storage");
    let store =
        git.create_repository(&source.worktree, text(&storage), Some(&source)).await.expect("creates the store");
    Setup { project, source, store }
}

fn capture_input(setup: &Setup, maximum_untracked_file_bytes: Option<u64>) -> CaptureInput {
    CaptureInput {
        repository: setup.store.clone(),
        scopes: vec![".".to_string()],
        ignores: Some(setup.source.clone()),
        maximum_untracked_file_bytes,
    }
}

async fn write(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.expect("creates the parent directory");
    }
    tokio::fs::write(path, content).await.expect("writes the file");
}

fn observe(git: &Git) -> Arc<Mutex<Vec<Vec<String>>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    git.set_observer(Some(Arc::new(move |args: &[String]| {
        recorded.lock().unwrap_or_else(|error| error.into_inner()).push(args.to_vec());
    })));
    seen
}

fn invocations(seen: &Mutex<Vec<Vec<String>>>) -> Vec<Vec<String>> {
    seen.lock().unwrap_or_else(|error| error.into_inner()).clone()
}

fn count_command(seen: &Mutex<Vec<Vec<String>>>, command: &str) -> usize {
    invocations(seen).iter().filter(|args| args.iter().any(|arg| arg == command)).count()
}

#[tokio::test]
async fn capture_applies_store_and_source_ignore_rules_without_splitting_the_index() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("tracked.bin", "small\n")]).await;
    write(setup.project.join(".git/info/exclude"), "source-ignored/\n").await;
    let store_directory = Path::new(&setup.store.git_directory);
    write(store_directory.join("info/exclude"), "store-ignored/\n").await;
    fixture::git(
        &git,
        temp.path(),
        &["--git-dir", setup.store.git_directory.as_str(), "config", "core.splitIndex", "true"],
    )
    .await;

    let before = git.tree_capture(capture_input(&setup, None)).await.expect("captures the initial tree");
    write(setup.project.join("tracked.bin"), "changed\n").await;
    write(setup.project.join("store-ignored/a.txt"), "a\n").await;
    write(setup.project.join("source-ignored/b.txt"), "b\n").await;
    write(setup.project.join("kept.txt"), "kept\n").await;

    let after = git.tree_capture(capture_input(&setup, None)).await.expect("captures the changed tree");
    let files = git.tree_files(&setup.store, &before, &after).await.expect("lists the changed files");
    assert_eq!(files, ["kept.txt", "tracked.bin"]);
    assert_eq!(git.tree_capture(capture_input(&setup, None)).await.expect("captures an unchanged tree"), after);
    assert!(fixture::read(&store_directory.join("info/exclude")).await.starts_with("store-ignored/\n"));
}

#[tokio::test]
async fn queued_captures_share_one_capture_that_still_sees_changes_made_before_the_call() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
    let before = git.tree_capture(capture_input(&setup, None)).await.expect("captures the initial tree");
    write(setup.project.join("a.txt"), "changed\n").await;
    let seen = observe(&git);

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let git = git.clone();
        let input = capture_input(&setup, None);
        tasks.spawn(async move { git.tree_capture(input).await });
    }
    let mut trees = Vec::new();
    while let Some(result) = tasks.join_next().await {
        trees.push(result.expect("joins the capture").expect("captures the tree"));
    }
    assert_eq!(trees.len(), 4);
    assert_eq!(trees.iter().collect::<HashSet<_>>().len(), 1);
    let files = git.tree_files(&setup.store, &before, &trees[0]).await.expect("lists the changed files");
    assert_eq!(files, ["a.txt"]);
    assert!(count_command(&seen, "ls-files") <= 2);
}

#[tokio::test]
async fn skips_repeat_ignore_checks_for_oversized_and_info_exclude_only_untracked_files() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
    write(setup.project.join(".git/info/exclude"), "excluded.txt\n").await;
    write(setup.project.join("excluded.txt"), "x\n").await;
    write(setup.project.join("large.txt"), "too large\n").await;

    let first = git.tree_capture(capture_input(&setup, Some(4))).await.expect("captures the initial tree");
    let seen = observe(&git);
    let second = git.tree_capture(capture_input(&setup, Some(4))).await.expect("captures the unchanged tree");
    assert_eq!(second, first);

    let calls = invocations(&seen);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().any(|args| args.iter().any(|arg| arg == "diff-files")));
    assert!(calls.iter().any(|args| args.iter().any(|arg| arg == "ls-files")));

    let entries = fixture::git(
        &git,
        temp.path(),
        &["--git-dir", setup.store.git_directory.as_str(), "ls-tree", "--name-only", first.as_str()],
    )
    .await;
    assert_eq!(entries.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>(), ["a.txt", "b.txt"]);
}

#[tokio::test]
async fn treats_a_zero_maximum_untracked_file_bytes_as_no_limit() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n")]).await;
    write(setup.project.join("large.txt"), "too large\n").await;

    let tree = git.tree_capture(capture_input(&setup, Some(0))).await.expect("captures the tree");
    let entries = fixture::git(
        &git,
        temp.path(),
        &["--git-dir", setup.store.git_directory.as_str(), "ls-tree", "--name-only", tree.as_str()],
    )
    .await;
    assert_eq!(entries.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>(), ["a.txt", "large.txt"]);
}

#[tokio::test]
async fn seeds_and_rebuilds_the_store_from_a_source_with_a_split_index() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "one\n")]).await;
    fixture::git(&git, &setup.project, &["update-index", "--split-index"]).await;

    let before = git.tree_capture(capture_input(&setup, None)).await.expect("captures the initial tree");
    write(setup.project.join("a.txt"), "two\n").await;
    let after = git.tree_capture(capture_input(&setup, None)).await.expect("captures the changed tree");
    let files = git.tree_files(&setup.store, &before, &after).await.expect("lists the changed files");
    assert_eq!(files, ["a.txt"]);

    Box::pin(git.tree_restore(&setup.store, &[("a.txt".to_string(), before.clone())]))
        .await
        .expect("restores the tracked file");
    assert_eq!(fixture::read(&setup.project.join("a.txt")).await, "one\n");
    let store_directory = Path::new(&setup.store.git_directory);
    write(store_directory.join("index"), "not an index").await;
    assert_eq!(
        git.tree_capture(capture_input(&setup, None)).await.expect("rebuilds the store from the source"),
        before
    );
    let shared = std::fs::read_dir(store_directory)
        .expect("reads the store")
        .filter_map(|entry| {
            let name = entry.expect("reads a store entry").file_name().to_string_lossy().into_owned();
            name.starts_with("sharedindex.").then_some(name)
        })
        .collect::<Vec<_>>();
    assert!(shared.is_empty(), "{shared:?}");
}

#[tokio::test]
async fn sweeps_abandoned_temporary_indexes_by_their_creation_time_not_their_mtime() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
    let store_directory = Path::new(&setup.store.git_directory);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("reads the clock").as_millis();
    let live = store_directory.join(format!("index.opencode-{now}-1-live"));
    let abandoned = store_directory.join(format!("index.opencode-{}-1-abandoned", now - 7_200_000));
    std::fs::write(&live, "").expect("creates the live temporary index");
    std::fs::write(&abandoned, "").expect("creates the abandoned temporary index");
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7_200);
    let file = std::fs::OpenOptions::new().write(true).open(&live).expect("opens the live temporary index");
    file.set_times(std::fs::FileTimes::new().set_accessed(two_hours_ago).set_modified(two_hours_ago))
        .expect("ages the live temporary index");

    git.tree_capture(capture_input(&setup, None)).await.expect("captures the tree");

    assert!(live.exists());
    assert!(!abandoned.exists());
}

#[tokio::test]
async fn records_a_tracked_file_replaced_by_an_embedded_repository_as_a_gitlink() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
    git.tree_capture(capture_input(&setup, None)).await.expect("captures the initial tree");
    std::fs::remove_file(setup.project.join("a.txt")).expect("removes the tracked file");
    write(setup.project.join("a.txt/inner.txt"), "inner\n").await;
    let embedded = setup.project.join("a.txt");
    fixture::init_git(&git, &embedded).await;
    fixture::git(&git, &embedded, &["add", "."]).await;
    fixture::git(&git, &embedded, &["commit", "-q", "-m", "inner"]).await;

    let tree = git.tree_capture(capture_input(&setup, None)).await.expect("captures the gitlink");
    let entry = fixture::git(
        &git,
        temp.path(),
        &["--git-dir", setup.store.git_directory.as_str(), "ls-tree", tree.as_str(), "a.txt"],
    )
    .await;
    assert!(entry.starts_with("160000 commit "), "{entry}");
}

#[tokio::test]
async fn keeps_the_index_when_git_cannot_even_start() {
    let temp = TempDir::new();
    let git = fixture::handle();
    let setup = setup(&git, &temp, &[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
    let before = git.tree_capture(capture_input(&setup, None)).await.expect("captures the initial tree");
    let moved = temp.path().join("moved");
    std::fs::rename(&setup.project, &moved).expect("moves the project");
    assert!(git.tree_capture(capture_input(&setup, None)).await.is_err());
    std::fs::rename(&moved, &setup.project).expect("restores the project");
    assert!(Path::new(&setup.store.git_directory).join("index").exists());
    assert_eq!(git.tree_capture(capture_input(&setup, None)).await.expect("captures the restored tree"), before);
}

#[tokio::test]
async fn refresh_handles_check_ignore_exit_codes() {
    for exit_code in [0, 1, 128] {
        let temp = TempDir::new();
        let git = fixture::handle();
        let setup = setup(&git, &temp, &[("scope/allowed.txt", "one\n"), ("scope/ignored name.txt", "one\n")]).await;
        let before = git.tree_write(&setup.store, None).await.expect("writes the initial tree");
        write(setup.project.join("scope/allowed.txt"), "two\n").await;
        write(setup.project.join("scope/ignored name.txt"), "two\n").await;
        let exclude = if exit_code == 0 { "scope/ignored name.txt\n" } else { "" };
        write(setup.project.join(".git/info/exclude"), exclude).await;
        if exit_code == 128 {
            write(setup.project.join(".git/config"), "[broken\n").await;
        }

        let output = git
            .execute(
                &[
                    "--git-dir",
                    setup.source.git_directory.as_str(),
                    "--work-tree",
                    setup.source.worktree.as_str(),
                    "check-ignore",
                    "--no-index",
                    "--stdin",
                    "-z",
                ],
                RunOptions {
                    cwd: Some(setup.source.worktree.clone()),
                    stdin: Some("scope/allowed.txt\0scope/ignored name.txt\0".as_bytes().to_vec()),
                    ..Default::default()
                },
            )
            .await
            .expect("git starts");
        assert_eq!(output.status, exit_code, "check-ignore exit {exit_code}");

        let paths = vec!["scope/allowed.txt".to_string(), "scope/ignored name.txt".to_string()];
        if exit_code == 128 {
            let error = git.index_refresh(&setup.store, "scope", Some(&setup.source), None).await.unwrap_err();
            assert!(error.message.contains("bad config line"), "{}", error.message);
            assert_eq!(git.tree_write(&setup.store, None).await.expect("writes the unchanged tree"), before);
            assert!(
                git.index_refresh(&setup.store, "missing", Some(&setup.source), None)
                    .await
                    .expect("refreshes a missing scope")
                    .is_empty()
            );
        } else {
            let expected =
                if exit_code == 0 { HashSet::from(["scope/ignored name.txt".to_string()]) } else { HashSet::new() };
            assert_eq!(git.index_ignored(&setup.source, &paths).await.expect("reads the ignored paths"), expected);
            assert!(
                git.index_refresh(&setup.store, "scope", Some(&setup.source), None)
                    .await
                    .expect("refreshes the scope")
                    .is_empty()
            );
            let after = git.tree_write(&setup.store, None).await.expect("writes the refreshed tree");
            let files = git.tree_files(&setup.store, &before, &after).await.expect("lists the changed files");
            assert_eq!(files, ["scope/allowed.txt", "scope/ignored name.txt"]);
            let statuses = git
                .tree_diff(&setup.store, &before, &after, None, None)
                .await
                .expect("diffs the trees")
                .iter()
                .map(|diff| diff.status)
                .collect::<Vec<_>>();
            assert_eq!(
                statuses,
                if exit_code == 0 {
                    [FileDiffStatus::Modified, FileDiffStatus::Deleted]
                } else {
                    [FileDiffStatus::Modified, FileDiffStatus::Modified]
                }
            );
        }
    }
}
