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

#[tokio::test]
async fn fetch_treats_an_option_like_remote_as_a_repository_argument() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");
    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");
    let marker = root.join("upload-pack-ran");

    let error = git
        .fetch_branch(&repository, &format!("--upload-pack=touch {}", text(&marker)), "main", false)
        .await
        .expect_err("fetches fail for the option-like remote");

    assert!(!marker.exists(), "the remote is never executed as an upload-pack command");
    assert!(!error.message.contains("unknown option"), "the remote is treated as a repository: {}", error.message);
}

#[tokio::test]
async fn checkout_refuses_option_like_branches_and_remotes() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");
    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");

    let branch = git.checkout_remote_branch(&repository, "origin", "-f", false).await.expect_err("rejects the branch");
    assert_eq!(branch.operation, opencode_git::Operation::Checkout);
    let remote = git.checkout_remote_branch(&repository, "-f", "main", true).await.expect_err("rejects the remote");
    assert_eq!(remote.operation, opencode_git::Operation::Checkout);
    assert_eq!(git.branch(&repository).await, Some("main".to_string()));
}

#[tokio::test]
async fn checkout_fails_for_a_name_that_is_only_a_path() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");
    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");
    tokio::fs::write(target.join("README.md"), "local\n").await.expect("changes the tracked file");

    let error =
        git.checkout_remote_branch(&repository, "origin", "README.md", false).await.expect_err("rejects the path");

    assert_eq!(error.operation, opencode_git::Operation::Checkout);
    assert_eq!(fixture::read(&target.join("README.md")).await, "local\n");
    assert_eq!(git.branch(&repository).await, Some("main".to_string()));
}

#[tokio::test]
async fn reset_refuses_option_like_revisions() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let git = fixture::handle();
    let remote = fixture::git_remote(&git, &root).await;
    let target = root.join("checkout");
    let repository = git.clone_repository(&remote.remote, text(&target), None, None).await.expect("clones the remote");
    tokio::fs::write(target.join("README.md"), "local\n").await.expect("changes the tracked file");

    let error = git.reset_hard(&repository, "-q").await.expect_err("rejects the revision");

    assert_eq!(error.operation, opencode_git::Operation::Reset);
    assert_eq!(fixture::read(&target.join("README.md")).await, "local\n");
}
