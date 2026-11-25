use crate::CommandError;
use crate::Git;
use crate::repo::Repository;

impl Git {
    pub async fn remote_get(&self, repository: &Repository) -> Result<Option<String>, CommandError> {
        let args = ["remote", "get-url", "origin"];
        let output = self.command(&repository.worktree, &args).await?;
        if output.status == 2 {
            return Ok(None);
        }
        if output.status != 0 {
            return Err(failed(&args, output.status));
        }
        Ok(non_empty(output.text().trim()))
    }

    pub async fn root_commits(&self, repository: &Repository) -> Result<Vec<String>, CommandError> {
        let args = ["rev-list", "--max-parents=0", "HEAD"];
        let output = self.command(&repository.worktree, &args).await?;
        if output.status == 0 {
            let stdout = output.text();
            let mut commits: Vec<String> =
                stdout.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect();
            commits.sort();
            return Ok(commits);
        }
        let verify = self.command(&repository.worktree, &["rev-parse", "-q", "--verify", "HEAD"]).await?;
        if verify.status == 1 {
            return Ok(Vec::new());
        }
        Err(failed(&args, output.status))
    }

    pub async fn head(&self, repository: &Repository) -> Option<String> {
        running(self, &repository.worktree, &["rev-parse", "HEAD"]).await
    }

    pub async fn branch(&self, repository: &Repository) -> Option<String> {
        running(self, &repository.worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await
    }

    pub async fn default_remote_branch(&self, repository: &Repository) -> Option<String> {
        let branch = running(self, &repository.worktree, &["symbolic-ref", "refs/remotes/origin/HEAD"]).await?;
        non_empty(branch.strip_prefix("refs/remotes/origin/").unwrap_or(&branch))
    }
}

async fn running(git: &Git, directory: &str, args: &[&str]) -> Option<String> {
    let output = git.run(directory, args).await;
    if output.status != 0 {
        return None;
    }
    non_empty(output.text().trim())
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn failed(args: &[&str], status: i32) -> CommandError {
    CommandError { args: args.iter().map(|arg| arg.to_string()).collect(), exit_code: Some(status), cause: None }
}
