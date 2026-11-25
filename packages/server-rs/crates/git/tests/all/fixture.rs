use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_git::{Git, RunOptions};
use tokio::io::AsyncWriteExt;

use crate::text;

const REPOSITORY_CONFIG: &str =
    "\n[core]\n\tfsmonitor = false\n[commit]\n\tgpgsign = false\n[user]\n\temail = test@opencode.test\n\tname = Test\n";

pub fn handle() -> Arc<Git> {
    Arc::new(Git::new(Arc::new(|name: &str| std::env::var_os(name))))
}

pub async fn git(git: &Git, directory: &Path, args: &[&str]) -> String {
    let options = RunOptions {
        cwd: Some(directory.to_string_lossy().into_owned()),
        env: vec![("GIT_ALLOW_PROTOCOL".to_string(), "file".to_string())],
        ..Default::default()
    };
    let output = git.execute(args, options).await.expect("git starts");
    assert_eq!(output.status, 0, "git {} failed: {}", args.join(" "), output.stderr_text());
    output.text().trim().to_string()
}

pub async fn init_git(git: &Git, directory: &Path) {
    tokio::fs::create_dir_all(directory).await.expect("creates the repository directory");
    crate::fixture::git(git, directory, &["init"]).await;
    let mut config = tokio::fs::OpenOptions::new()
        .append(true)
        .open(directory.join(".git").join("config"))
        .await
        .expect("opens the repository config");
    config.write_all(REPOSITORY_CONFIG.as_bytes()).await.expect("appends the repository config");
}

pub async fn commit_allow_empty(git: &Git, directory: &Path, message: &str) {
    crate::fixture::git(git, directory, &["commit", "--allow-empty", "-m", message]).await;
}

pub async fn init_repo(git: &Git, directory: &Path) {
    init_git(git, directory).await;
    commit_allow_empty(git, directory, "root").await;
}

pub struct RemoteFixture {
    pub source: PathBuf,
    pub remote: String,
}

pub async fn git_remote(git: &Git, root: &Path) -> RemoteFixture {
    let origin = root.join("origin.git");
    let source = root.join("source");
    crate::fixture::git(git, root, &["init", "--bare", text(&origin)]).await;
    init_git(git, &source).await;
    write(&source.join("README.md"), "one\n").await;
    crate::fixture::git(git, &source, &["add", "README.md"]).await;
    crate::fixture::git(git, &source, &["commit", "-m", "initial"]).await;
    crate::fixture::git(git, &source, &["branch", "-M", "main"]).await;
    let remote = file_url(&origin);
    crate::fixture::git(git, &source, &["remote", "add", "origin", &remote]).await;
    crate::fixture::git(git, &source, &["push", "-u", "origin", "main"]).await;
    crate::fixture::git(git, root, &["--git-dir", text(&origin), "symbolic-ref", "HEAD", "refs/heads/main"]).await;
    RemoteFixture { source, remote }
}

pub async fn commit(git: &Git, source: &Path, content: &str, message: &str) {
    write(&source.join("README.md"), content).await;
    crate::fixture::git(git, source, &["add", "README.md"]).await;
    crate::fixture::git(git, source, &["commit", "-m", message]).await;
    crate::fixture::git(git, source, &["push"]).await;
}

pub async fn branch(git: &Git, source: &Path, name: &str, content: &str) {
    crate::fixture::git(git, source, &["checkout", "-b", name]).await;
    write(&source.join("README.md"), content).await;
    crate::fixture::git(git, source, &["add", "README.md"]).await;
    crate::fixture::git(git, source, &["commit", "-m", name]).await;
    crate::fixture::git(git, source, &["push", "-u", "origin", name]).await;
}

pub async fn read(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.expect("reads the file").replace("\r\n", "\n")
}

async fn write(path: &Path, content: &str) {
    tokio::fs::write(path, content).await.expect("writes the file");
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') { format!("file://{text}") } else { format!("file:///{text}") }
}
