use crate::fixture;
use crate::{TempDir, text};

#[tokio::test]
async fn clones_a_remote_and_reads_checkout_metadata() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");

    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");

    assert_eq!(git.remote_get(&repository).await.expect("reads the remote"), Some(remote.remote.clone()));
    assert!(git.head(&repository).await.is_some());
    assert_eq!(git.branch(&repository).await, Some("main".to_string()));
    assert_eq!(git.default_remote_branch(&repository).await, Some("main".to_string()));
    assert_eq!(repository.worktree, text(&target));
    assert_eq!(repository.git_directory, text(&target.join(".git")));
    assert_eq!(repository.common_directory, repository.git_directory);
    assert_eq!(fixture::read(&target.join("README.md")).await, "one\n");
}

#[tokio::test]
async fn fetches_checks_out_and_resets_remote_changes() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");

    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");

    fixture::commit(&git, &remote.source, "two\n", "second").await;
    git.fetch_remotes(&repository, true).await.expect("fetches the remotes");
    git.reset_hard(&repository, "origin/main").await.expect("resets the checkout");
    assert_eq!(fixture::read(&target.join("README.md")).await, "two\n");

    fixture::branch(&git, &remote.source, "feature/docs", "feature\n").await;
    git.fetch_branch(&repository, "origin", "feature/docs", true).await.expect("fetches the branch");
    git.checkout_remote_branch(&repository, "origin", "feature/docs", true).await.expect("checks out the branch");
    git.reset_hard(&repository, "origin/feature/docs").await.expect("resets the branch");

    assert_eq!(git.branch(&repository).await, Some("feature/docs".to_string()));
    assert_eq!(fixture::read(&target.join("README.md")).await, "feature\n");
}
