use std::path::Path;
use std::sync::Arc;

use opencode_git::{Git, RunOptions};
use tokio::io::AsyncWriteExt;

const REPOSITORY_CONFIG: &str =
    "\n[core]\n\tfsmonitor = false\n[commit]\n\tgpgsign = false\n[user]\n\temail = test@opencode.test\n\tname = Test\n";

pub fn handle() -> Arc<Git> {
    Arc::new(Git::new(Arc::new(|name: &str| std::env::var_os(name))))
}

pub async fn git(git: &Git, directory: &Path, args: &[&str]) -> String {
    let options = RunOptions { cwd: Some(directory.to_string_lossy().into_owned()), ..Default::default() };
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
