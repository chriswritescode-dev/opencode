use opencode_git::{WorktreeKind, WorktreeOperation};

use crate::fixture;
use crate::{TempDir, text};

#[tokio::test]
async fn creates_lists_and_removes_linked_worktrees() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let main = root.join("main");
    let linked = root.join("linked");
    let git = fixture::handle();
    fixture::init_git(&git, &main).await;
    fixture::commit_allow_empty(&git, &main, "root").await;

    let repository = git.discover(text(&main)).await.expect("discovers the repository");
    git.worktree_create(&repository, text(&linked), None).await.expect("creates the worktree");

    let entries = git.worktree_list(&repository).await.expect("lists the worktrees");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, WorktreeKind::Main);
    assert_eq!(entries[1].kind, WorktreeKind::Linked);
    assert!(entries[1].directory.ends_with("linked"));

    let checkout = git.discover(text(&linked)).await.expect("discovers the worktree");
    assert_eq!(checkout.worktree, text(&linked));
    assert_eq!(checkout.common_directory, repository.common_directory);
    assert_ne!(checkout.git_directory, repository.git_directory);

    git.worktree_remove(&checkout, text(&linked), false).await.expect("removes the worktree");

    let remaining = git.worktree_list(&repository).await.expect("lists the worktrees");
    assert_eq!(remaining.len(), 1);
    assert!(!remaining.iter().any(|entry| entry.directory.ends_with("linked")));
}

#[tokio::test]
async fn removing_a_dirty_worktree_requires_force() {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("canonicalizes the temporary directory");
    let main = root.join("main");
    let linked = root.join("linked");
    let git = fixture::handle();
    fixture::init_git(&git, &main).await;
    fixture::commit_allow_empty(&git, &main, "root").await;

    let repository = git.discover(text(&main)).await.expect("discovers the repository");
    git.worktree_create(&repository, text(&linked), None).await.expect("creates the worktree");
    tokio::fs::write(linked.join("dirty.txt"), "dirty\n").await.expect("writes the dirty file");

    let error = git.worktree_remove(&repository, text(&linked), false).await.expect_err("rejects a dirty worktree");
    assert!(error.force_required);
    assert_eq!(error.operation, WorktreeOperation::Remove);

    git.worktree_remove(&repository, text(&linked), true).await.expect("forces the removal");
    assert_eq!(git.worktree_list(&repository).await.expect("lists the worktrees").len(), 1);
}
