use std::path::Path;

use opencode_git::{CaptureInput, FileDiffStatus, Git, MAX_TOTAL_PATCH_BYTES, Repository, empty_patch};

use crate::fixture;
use crate::{TempDir, text};

async fn write(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.expect("creates the parent directory");
    }
    tokio::fs::write(path, content).await.expect("writes the file");
}

async fn capture(git: &Git, repository: &Repository) -> String {
    Box::pin(git.tree_capture(CaptureInput {
        repository: repository.clone(),
        scopes: vec![".".to_string()],
        ignores: None,
        maximum_untracked_file_bytes: None,
    }))
    .await
    .expect("captures the tree")
}

#[tokio::test]
async fn lists_both_sides_of_a_rename_as_separate_file_changes() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_repo(&git, temp.path()).await;
    fixture::git(&git, temp.path(), &["config", "diff.renames", "true"]).await;
    write(temp.path().join("old name.txt"), "Preserve this content.\n").await;

    let repository = git.discover(text(temp.path())).await.expect("discovers the repository");
    let before = capture(&git, &repository).await;
    std::fs::rename(temp.path().join("old name.txt"), temp.path().join("new name.txt")).expect("renames the file");
    let after = capture(&git, &repository).await;

    assert_eq!(
        git.tree_files(&repository, &before, &after).await.expect("lists the changed files"),
        ["new name.txt", "old name.txt"]
    );
    assert_eq!(
        git.tree_diff(&repository, &before, &after, None, None)
            .await
            .expect("diffs the trees")
            .iter()
            .map(|diff| (diff.file.as_str(), diff.status))
            .collect::<Vec<_>>(),
        [("new name.txt", FileDiffStatus::Added), ("old name.txt", FileDiffStatus::Deleted)]
    );
}

#[tokio::test]
async fn caps_batched_tree_patches_keeps_per_file_stats_past_the_cap_and_matches_non_ascii_names() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_repo(&git, temp.path()).await;

    let repository = git.discover(text(temp.path())).await.expect("discovers the repository");
    let before = capture(&git, &repository).await;
    let lines = MAX_TOTAL_PATCH_BYTES.div_ceil(80) + 1;
    write(temp.path().join("a-small.txt"), "small\n").await;
    let large = format!("{}\n", "x".repeat(79)).repeat(lines);
    write(temp.path().join("b-large.txt"), &large).await;
    std::fs::write(temp.path().join("c-binary.bin"), [0u8, 1, 2, 3]).expect("writes the binary file");
    write(temp.path().join("a-café.txt"), "café\n").await;
    let after = capture(&git, &repository).await;

    let diffs = git.tree_diff(&repository, &before, &after, Some(0), None).await.expect("diffs the trees");
    assert_eq!(
        diffs.iter().map(|diff| (diff.file.as_str(), diff.status, diff.additions, diff.deletions)).collect::<Vec<_>>(),
        [
            ("a-café.txt", FileDiffStatus::Added, 1, 0),
            ("a-small.txt", FileDiffStatus::Added, 1, 0),
            ("b-large.txt", FileDiffStatus::Added, lines, 0),
            ("c-binary.bin", FileDiffStatus::Added, 0, 0),
        ]
    );
    assert!(diffs[0].patch.contains("+café\n"));
    assert!(diffs[1].patch.contains("+small\n"));
    assert_eq!(diffs[2].patch, empty_patch("b-large.txt"));
    assert_eq!(diffs[3].patch, "");
    assert!(git.tree_diff(&repository, &before, &after, None, Some(&[])).await.expect("diffs nothing").is_empty());
}

#[tokio::test]
async fn diffs_a_path_selection_longer_than_any_platform_command_line() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_repo(&git, temp.path()).await;
    write(temp.path().join("changed.txt"), "before\n").await;
    write(temp.path().join("removed.txt"), "removed\n").await;
    write(temp.path().join("dir/nested/file.txt"), "before\n").await;

    let repository = git.discover(text(temp.path())).await.expect("discovers the repository");
    let before = capture(&git, &repository).await;
    write(temp.path().join("changed.txt"), "after\n").await;
    std::fs::remove_file(temp.path().join("removed.txt")).expect("removes the file");
    write(temp.path().join("dir/nested/file.txt"), "after\n").await;
    write(temp.path().join("first.txt"), "first\n").await;
    write(temp.path().join("last.txt"), "last\n").await;
    write(temp.path().join("unselected.txt"), "unselected\n").await;
    let after = capture(&git, &repository).await;

    let missing = (0..30_000).map(|index| format!("missing/{index:06}/{}.txt", "x".repeat(80))).collect::<Vec<_>>();
    let mut paths =
        vec!["first.txt".to_string(), "changed.txt".to_string(), "removed.txt".to_string(), "dir".to_string()];
    paths.extend(missing.iter().cloned());
    paths.push("last.txt".to_string());

    let diffs =
        Box::pin(git.tree_diff(&repository, &before, &after, None, Some(&paths))).await.expect("diffs the trees");
    assert_eq!(
        diffs.iter().map(|diff| (diff.file.as_str(), diff.status, diff.additions, diff.deletions)).collect::<Vec<_>>(),
        [
            ("changed.txt", FileDiffStatus::Modified, 1, 1),
            ("dir/nested/file.txt", FileDiffStatus::Modified, 1, 1),
            ("first.txt", FileDiffStatus::Added, 1, 0),
            ("last.txt", FileDiffStatus::Added, 1, 0),
            ("removed.txt", FileDiffStatus::Deleted, 0, 1),
        ]
    );
    assert!(diffs[0].patch.contains("-before\n+after\n"));
    assert!(diffs[2].patch.contains("+first\n"));
    assert!(diffs[4].patch.contains("-removed\n"));
    assert!(git.tree_diff(&repository, &before, &after, None, Some(&missing)).await.expect("diffs nothing").is_empty());
    assert_eq!(git.tree_write(&repository, None).await.expect("writes the tree"), after);
}

#[tokio::test]
async fn captures_compares_previews_and_restores_scoped_trees() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_repo(&git, temp.path()).await;
    write(temp.path().join("scope/tracked.txt"), "one\n").await;
    write(temp.path().join("outside.txt"), "outside\n").await;
    fixture::git(&git, temp.path(), &["add", "."]).await;
    fixture::git(&git, temp.path(), &["commit", "-q", "-m", "initial"]).await;

    let source = git.discover(text(temp.path())).await.expect("discovers the source");
    let storage = temp.path().join("storage");
    let store =
        git.create_repository(&source.worktree, text(&storage), Some(&source)).await.expect("creates the store");
    fixture::git(
        &git,
        temp.path(),
        &["--git-dir", store.git_directory.as_str(), "config", "--add", "include.path", "first.gitconfig"],
    )
    .await;
    fixture::git(
        &git,
        temp.path(),
        &["--git-dir", store.git_directory.as_str(), "config", "--add", "include.path", "second.gitconfig"],
    )
    .await;
    fixture::git(&git, temp.path(), &["--git-dir", store.git_directory.as_str(), "config", "core.autocrlf", "true"])
        .await;
    git.create_repository(&source.worktree, text(&storage), Some(&source)).await.expect("reopens the store");

    let autocrlf = fixture::git(
        &git,
        temp.path(),
        &["--git-dir", store.git_directory.as_str(), "config", "--local", "--includes", "core.autocrlf"],
    )
    .await;
    assert_eq!(autocrlf, "false");
    let config = fixture::read(&Path::new(&store.git_directory).join("config")).await;
    assert_eq!(config.matches("opencode.gitconfig").count(), 1);
    let includes = fixture::git(
        &git,
        temp.path(),
        &["--git-dir", store.git_directory.as_str(), "config", "--local", "--get-all", "include.path"],
    )
    .await;
    assert_eq!(includes, "opencode.gitconfig\nfirst.gitconfig\nsecond.gitconfig");

    git.index_refresh(&store, "scope", None, None).await.expect("refreshes the scope");
    let before = git.tree_write(&store, None).await.expect("writes the tree");
    write(temp.path().join("scope/tracked.txt"), "two\n").await;
    write(temp.path().join("scope/added.txt"), "added\n").await;
    write(temp.path().join("outside.txt"), "changed outside\n").await;
    git.index_refresh(&store, "scope", None, None).await.expect("refreshes the scope");
    let after = git.tree_write(&store, None).await.expect("writes the tree");

    assert_eq!(
        git.tree_files(&store, &before, &after).await.expect("lists the changed files"),
        ["scope/added.txt", "scope/tracked.txt"]
    );
    assert_eq!(
        git.tree_diff(&store, &before, &after, Some(1), None)
            .await
            .expect("diffs the trees")
            .iter()
            .map(|diff| (diff.file.as_str(), diff.status))
            .collect::<Vec<_>>(),
        [("scope/added.txt", FileDiffStatus::Added), ("scope/tracked.txt", FileDiffStatus::Modified)]
    );

    git.tree_restore(&store, &[("scope/tracked.txt".to_string(), before.clone())])
        .await
        .expect("restores the tracked file");
    assert_eq!(fixture::read(&temp.path().join("scope/tracked.txt")).await, "one\n");
    assert_eq!(fixture::read(&temp.path().join("scope/added.txt")).await, "added\n");
    assert_eq!(fixture::read(&temp.path().join("outside.txt")).await, "changed outside\n");
}

#[tokio::test]
async fn restores_paths_in_the_given_order() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_repo(&git, temp.path()).await;
    write(temp.path().join("a"), "file a\n").await;

    let repository = git.discover(text(temp.path())).await.expect("discovers the repository");
    let file_tree = capture(&git, &repository).await;
    std::fs::remove_file(temp.path().join("a")).expect("removes the file");
    write(temp.path().join("a/b"), "nested b\n").await;
    let nested_tree = capture(&git, &repository).await;

    git.tree_restore(&repository, &[("a/b".to_string(), file_tree.clone()), ("a".to_string(), file_tree.clone())])
        .await
        .expect("restores the nested path before the file");
    assert_eq!(fixture::read(&temp.path().join("a")).await, "file a\n");

    git.tree_restore(&repository, &[("a/b".to_string(), nested_tree)]).await.expect("restores the nested path");
    git.tree_restore(&repository, &[("a".to_string(), file_tree.clone()), ("a/b".to_string(), file_tree.clone())])
        .await
        .expect_err("fails to remove the nested path after checking out the file");
    assert_eq!(fixture::read(&temp.path().join("a")).await, "file a\n");
}
