use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_storage::kv;
use opencode_util::time::now_ms;
use opencode_vcs::repository::{self, Reference, RemoteReference, Vars};
use opencode_vcs::repository_cache::{EnsureInput, Error, Materializer, Refresh, RepositoryCache, Status};
use serde_json::json;

use crate::support::Fixture;

const ALLOW_FILE: &[(&str, &str)] = &[("GIT_ALLOW_PROTOCOL", "file")];

#[tokio::test]
async fn persists_the_daily_throttle_across_cache_recreation_and_serializes_competing_refreshes() {
    let fixture = Fixture::new();
    let (source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);

    let initial = materializer(&fixture).ensure(daily(&reference)).await.expect("clones the cache");
    assert_eq!(initial.status, Status::Cloned);
    commit(&fixture, &source, "two\n", "advance main").await;

    let cache = materializer(&fixture);
    assert_eq!(cache.ensure(daily(&reference)).await.expect("reads the cache").status, Status::Cached);
    assert_eq!(read(&fixture, &initial.local_path).await, "one\n");
    set_attempt(&fixture, &initial.local_path, now_ms() - 86_400_000);

    let first = materializer(&fixture);
    let second = materializer(&fixture);
    let (left, right) = tokio::join!(first.ensure(daily(&reference)), second.ensure(daily(&reference)));
    let mut statuses = vec![left.expect("first refresh").status, right.expect("second refresh").status];
    statuses.sort_by_key(|status| match status {
        Status::Cached => 0,
        Status::Cloned => 1,
        Status::Refreshed => 2,
    });
    assert_eq!(statuses, vec![Status::Cached, Status::Refreshed]);
    assert_eq!(read(&fixture, &initial.local_path).await, "two\n");

    tokio::fs::remove_dir_all(&initial.local_path).await.expect("removes the checkout");
    let recreated = materializer(&fixture).ensure(daily(&reference)).await.expect("recreates the cache");
    assert_eq!(recreated.status, Status::Cloned);
    assert_eq!(read(&fixture, &initial.local_path).await, "two\n");
}

#[tokio::test]
async fn honors_legacy_attempt_records_while_allowing_forced_refreshes() {
    let fixture = Fixture::new();
    let (source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let cache = materializer(&fixture);

    let initial = cache.ensure(daily(&reference)).await.expect("clones the cache");
    commit(&fixture, &source, "two\n", "advance main").await;

    let value = json!({ "attemptedAt": now_ms(), "refreshedAt": 0 });
    fixture
        .database
        .read(|connection| kv::set(connection, &cache_key(&initial.local_path), &value))
        .expect("writes the legacy record");

    assert_eq!(cache.ensure(daily(&reference)).await.expect("throttles").status, Status::Cached);
    assert_eq!(read(&fixture, &initial.local_path).await, "one\n");

    let forced = EnsureInput { refresh: Some(Refresh::Force), ..request(&reference) };
    assert_eq!(cache.ensure(forced).await.expect("forces a refresh").status, Status::Refreshed);
    assert_eq!(read(&fixture, &initial.local_path).await, "two\n");
    assert_eq!(cache.ensure(daily(&reference)).await.expect("throttles again").status, Status::Cached);
}

#[tokio::test]
async fn throttles_failed_refresh_attempts_until_the_next_interval() {
    let fixture = Fixture::new();
    let (source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let cache = materializer(&fixture);

    let initial = cache.ensure(daily(&reference)).await.expect("clones the cache");
    set_attempt(&fixture, &initial.local_path, now_ms() - 86_400_000);
    tokio::fs::rename(fixture.root.join("origin.git"), fixture.root.join("offline.git"))
        .await
        .expect("takes the origin offline");

    let error = cache.ensure(daily(&reference)).await.expect_err("fetch fails");
    assert!(matches!(error, Error::FetchFailed { .. }), "{error:?}");
    assert_eq!(cache.ensure(daily(&reference)).await.expect("throttles the failure").status, Status::Cached);
    assert_eq!(read(&fixture, &initial.local_path).await, "one\n");

    tokio::fs::rename(fixture.root.join("offline.git"), fixture.root.join("origin.git"))
        .await
        .expect("restores the origin");
    commit(&fixture, &source, "two\n", "advance main").await;
    set_attempt(&fixture, &initial.local_path, now_ms() - 86_400_000);

    assert_eq!(cache.ensure(daily(&reference)).await.expect("refreshes").status, Status::Refreshed);
    assert_eq!(read(&fixture, &initial.local_path).await, "two\n");
}

#[tokio::test]
async fn refreshes_existing_untracked_checkouts_and_keeps_branch_freshness_independent() {
    let fixture = Fixture::new();
    let (source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    branch(&fixture, &source, "feature", "feature\n").await;
    let cache = materializer(&fixture);

    let main = cache.ensure(daily(&reference)).await.expect("clones the default branch");
    let featured = cache
        .ensure(EnsureInput { branch: Some("feature".to_owned()), ..daily(&reference) })
        .await
        .expect("clones the branch");
    fixture
        .database
        .read(|connection| kv::remove(connection, &cache_key(&featured.local_path)))
        .expect("removes the branch throttle");
    commit(&fixture, &source, "new feature\n", "advance feature").await;

    assert_eq!(cache.ensure(daily(&reference)).await.expect("throttles the default branch").status, Status::Cached);
    let refreshed = cache
        .ensure(EnsureInput { branch: Some("feature".to_owned()), ..daily(&reference) })
        .await
        .expect("refreshes the branch");
    assert_eq!(refreshed.status, Status::Refreshed);
    assert_eq!(read(&fixture, &main.local_path).await, "one\n");
    assert_eq!(read(&fixture, &featured.local_path).await, "new feature\n");
}

#[tokio::test]
async fn replaces_a_stale_cache_directory_before_cloning() {
    let fixture = Fixture::new();
    let (_source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let local_path = repository::cache_path(
        &fixture.root.join("repos").to_string_lossy(),
        &Reference::Remote(reference.clone()),
        None,
    );
    tokio::fs::create_dir_all(&local_path).await.expect("creates the stale directory");
    tokio::fs::write(local_path.clone() + "/stale.txt", "stale").await.expect("writes the stale file");

    let result = materializer(&fixture).ensure(request(&reference)).await.expect("replaces the cache");

    assert_eq!(result.status, Status::Cloned);
    assert!(tokio::fs::metadata(Path::new(&local_path).join("stale.txt")).await.is_err());
    assert_eq!(read(&fixture, &local_path).await, "one\n");
}

#[tokio::test]
async fn serializes_concurrent_materialization_for_the_same_checkout() {
    let fixture = Fixture::new();
    let (_source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);

    let first = materializer(&fixture);
    let second = materializer(&fixture);
    let (left, right) = tokio::join!(first.ensure(request(&reference)), second.ensure(request(&reference)));
    let left = left.expect("first materialization");
    let right = right.expect("second materialization");

    let mut statuses = vec![left.status, right.status];
    statuses.sort_by_key(|status| match status {
        Status::Cached => 0,
        Status::Cloned => 1,
        Status::Refreshed => 2,
    });
    assert_eq!(statuses, vec![Status::Cached, Status::Cloned]);
    assert_eq!(left.local_path, right.local_path);
}

#[tokio::test]
async fn replaces_an_existing_checkout_whose_origin_does_not_match() {
    let fixture = Fixture::new();
    let (_source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let cache = materializer(&fixture);

    let initial = cache.ensure(daily(&reference)).await.expect("clones the cache");
    let checkout = Path::new(&initial.local_path);
    fixture.git_env(checkout, &["config", "remote.origin.url", "https://github.com/other/repo.git"], ALLOW_FILE).await;
    fixture.write(&checkout.join("stale.txt"), "stale").await;

    let replaced = cache.ensure(daily(&reference)).await.expect("replaces the checkout");

    assert_eq!(replaced.status, Status::Cloned);
    assert!(tokio::fs::metadata(checkout.join("stale.txt")).await.is_err());
}

#[tokio::test]
async fn keeps_branch_checkouts_isolated_from_branchless_refreshes() {
    let fixture = Fixture::new();
    let (source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    branch(&fixture, &source, "feature", "two\n").await;
    let cache = materializer(&fixture);

    let featured = cache
        .ensure(EnsureInput { branch: Some("feature".to_owned()), ..request(&reference) })
        .await
        .expect("clones the branch");
    assert_eq!(featured.branch.as_deref(), Some("feature"));
    assert!(featured.local_path.ends_with("repo@feature"));
    assert_eq!(read(&fixture, &featured.local_path).await, "two\n");

    let refreshed = cache
        .ensure(EnsureInput { refresh: Some(Refresh::Force), ..request(&reference) })
        .await
        .expect("refreshes the default branch");
    assert_ne!(refreshed.local_path, featured.local_path);
    assert_eq!(read(&fixture, &refreshed.local_path).await, "one\n");

    let cached = cache
        .ensure(EnsureInput { branch: Some("feature".to_owned()), ..request(&reference) })
        .await
        .expect("reads the branch");
    assert_eq!(cached.status, Status::Cached);
    assert_eq!(read(&fixture, &cached.local_path).await, "two\n");
}

#[tokio::test]
async fn does_not_mistake_an_enclosing_repository_for_the_cache_checkout() {
    let fixture = Fixture::new();
    let (_source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let enclosing = fixture.root.join("repos");
    fixture.git_env(&fixture.root, &["clone", &remote, &text(&enclosing)], ALLOW_FILE).await;

    let result =
        materializer(&fixture).ensure(request(&reference)).await.expect("clones inside the enclosing repository");

    assert_eq!(result.status, Status::Cloned);
    assert_eq!(read(&fixture, &result.local_path).await, "one\n");
}

#[tokio::test]
async fn returns_typed_branch_validation_and_clone_failures() {
    let fixture = Fixture::new();
    let (_source, remote) = git_remote(&fixture).await;
    let reference = reference(&remote);
    let cache = materializer(&fixture);

    let invalid = cache
        .ensure(EnsureInput { branch: Some("../unsafe".to_owned()), ..request(&reference) })
        .await
        .expect_err("rejects the branch");
    assert!(matches!(invalid, Error::InvalidBranch { .. }), "{invalid:?}");

    let mut missing = reference.clone();
    missing.remote = file_url(&fixture.root.join("missing.git"));
    let clone_failure = cache.ensure(request(&missing)).await.expect_err("fails to clone");
    assert!(matches!(clone_failure, Error::CloneFailed { .. }), "{clone_failure:?}");
}

fn materializer(fixture: &Fixture) -> RepositoryCache {
    RepositoryCache::new(
        fixture.git.clone(),
        fixture.database.clone(),
        fixture.root.join("repos"),
        fixture.root.join("state").join("locks"),
        vars(),
    )
}

fn request(reference: &RemoteReference) -> EnsureInput {
    EnsureInput { reference: reference.clone(), refresh: None, branch: None }
}

fn daily(reference: &RemoteReference) -> EnsureInput {
    EnsureInput { refresh: Some(Refresh::Daily), ..request(reference) }
}

fn reference(remote: &str) -> RemoteReference {
    let mut reference = repository::parse_remote("owner/repo", &vars()).expect("parses the test reference");
    reference.remote = remote.to_owned();
    reference
}

fn vars() -> Vars {
    Arc::new(|name: &str| std::env::var_os(name))
}

fn cache_key(local_path: &str) -> String {
    format!("repository-cache:{local_path}")
}

fn set_attempt(fixture: &Fixture, local_path: &str, attempted_at: i64) {
    let value = json!({ "attemptedAt": attempted_at });
    fixture
        .database
        .read(|connection| kv::set(connection, &cache_key(local_path), &value))
        .expect("writes the throttle record");
}

async fn read(fixture: &Fixture, local_path: &str) -> String {
    fixture.read(&Path::new(local_path).join("README.md")).await
}

async fn git(fixture: &Fixture, directory: &Path, args: &[&str]) {
    fixture.git_env(directory, args, ALLOW_FILE).await;
}

async fn git_remote(fixture: &Fixture) -> (PathBuf, String) {
    let origin = fixture.root.join("origin.git");
    let source = fixture.root.join("source");
    git(fixture, &fixture.root, &["init", "--bare", &text(&origin)]).await;
    fixture.init_git(&source).await;
    fixture.write(&source.join("README.md"), "one\n").await;
    git(fixture, &source, &["add", "README.md"]).await;
    git(fixture, &source, &["commit", "-m", "initial"]).await;
    git(fixture, &source, &["branch", "-M", "main"]).await;
    let remote = file_url(&origin);
    git(fixture, &source, &["remote", "add", "origin", &remote]).await;
    git(fixture, &source, &["push", "-u", "origin", "main"]).await;
    git(fixture, &fixture.root, &["--git-dir", &text(&origin), "symbolic-ref", "HEAD", "refs/heads/main"]).await;
    (source, remote)
}

async fn commit(fixture: &Fixture, source: &Path, content: &str, message: &str) {
    fixture.write(&source.join("README.md"), content).await;
    git(fixture, source, &["add", "README.md"]).await;
    git(fixture, source, &["commit", "-m", message]).await;
    git(fixture, source, &["push"]).await;
}

async fn branch(fixture: &Fixture, source: &Path, name: &str, content: &str) {
    git(fixture, source, &["checkout", "-b", name]).await;
    fixture.write(&source.join("README.md"), content).await;
    git(fixture, source, &["add", "README.md"]).await;
    git(fixture, source, &["commit", "-m", name]).await;
    git(fixture, source, &["push", "-u", "origin", name]).await;
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') { format!("file://{text}") } else { format!("file:///{text}") }
}
