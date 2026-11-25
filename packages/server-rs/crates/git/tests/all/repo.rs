use std::path::Path;

use crate::TempDir;
use crate::fixture;

#[tokio::test]
async fn discovers_repository_metadata_without_a_work_tree() {
    let temp = TempDir::new();
    let main = temp.path().join("main");
    let git = fixture::handle();
    fixture::init_git(&git, &main).await;
    fixture::commit_allow_empty(&git, &main, "root").await;
    fixture::git(&git, &main, &["config", "core.bare", "true"]).await;

    let directory = std::fs::canonicalize(&main).expect("canonicalizes the repository");
    let repository = git.discover(text(&directory)).await.expect("discovers the repository");

    assert_eq!(repository.worktree, text(&directory));
    assert_eq!(repository.git_directory, text(&directory.join(".git")));
    assert_eq!(repository.common_directory, repository.git_directory);
}

#[tokio::test]
async fn reads_no_remote_and_no_root_commits_from_an_unborn_repository() {
    let temp = TempDir::new();
    let git = fixture::handle();
    fixture::init_git(&git, temp.path()).await;

    let repository = git.discover(text(temp.path())).await.expect("discovers the repository");

    assert_eq!(git.remote_get(&repository).await.expect("reads the remote"), None);
    assert!(git.root_commits(&repository).await.expect("reads root commits").is_empty());
}

#[tokio::test]
async fn fingerprint_changes_with_repository_metadata_including_from_a_linked_worktree() {
    let temp = TempDir::new();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    let git = fixture::handle();
    fixture::init_git(&git, &main).await;

    assert_eq!(git.fingerprint(&dot_git(temp.path())).await, None);
    let unborn = git.fingerprint(&dot_git(&main)).await.expect("fingerprints an unborn repository");
    assert_eq!(git.fingerprint(&dot_git(&main)).await, Some(unborn.clone()));

    fixture::commit_allow_empty(&git, &main, "root").await;
    let committed = git.fingerprint(&dot_git(&main)).await.expect("fingerprints a committed repository");
    assert_ne!(committed, unborn);

    fixture::git(&git, &main, &["worktree", "add", "-q", text(&linked)]).await;
    let before = git.fingerprint(&dot_git(&linked)).await.expect("fingerprints a linked worktree");

    fixture::git(&git, &main, &["remote", "add", "origin", "git@github.com:owner/repo.git"]).await;
    assert_ne!(git.fingerprint(&dot_git(&main)).await, Some(committed));
    assert_ne!(git.fingerprint(&dot_git(&linked)).await, Some(before));
}

fn dot_git(directory: &Path) -> String {
    text(&directory.join(".git")).to_string()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path is UTF-8")
}
