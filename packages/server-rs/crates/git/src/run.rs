use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};

use crate::Git;
use crate::error::{CommandError, Operation, OperationError};
use crate::repo::Repository;

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub cwd: Option<String>,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    pub index: Option<String>,
    pub max_output_bytes: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct RunOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

impl RunOutput {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct SpawnError {
    pub message: String,
}

impl Git {
    pub async fn execute(&self, args: &[&str], options: RunOptions) -> Result<RunOutput, SpawnError> {
        let RunOptions { cwd, stdin, env, index, max_output_bytes } = options;
        let mut command = Command::new(self.executable());
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.envs(env);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        self.count_execution();
        self.notify_observer(args);
        let mut child = command.spawn().map_err(spawn_error)?;
        let (stdout, stderr, ()) = tokio::join!(
            collect_output(child.stdout.take(), max_output_bytes),
            collect_output(child.stderr.take(), max_output_bytes),
            feed_stdin(child.stdin.take(), stdin),
        );
        let status = child.wait().await.map_err(spawn_error)?;
        Ok(RunOutput { status: status.code().unwrap_or(1), truncated: stdout.1, stdout: stdout.0, stderr: stderr.0 })
    }

    pub(crate) async fn command(&self, directory: &str, args: &[&str]) -> Result<RunOutput, CommandError> {
        self.execute(args, RunOptions { cwd: Some(directory.to_string()), ..Default::default() }).await.map_err(
            |error| CommandError {
                args: args.iter().map(|arg| arg.to_string()).collect(),
                exit_code: None,
                cause: Some(error.message),
            },
        )
    }

    pub(crate) async fn run(&self, directory: &str, args: &[&str]) -> RunOutput {
        self.command(directory, args).await.unwrap_or(RunOutput {
            status: 1,
            stdout: Vec::new(),
            stderr: Vec::new(),
            truncated: false,
        })
    }

    pub async fn repository_run(
        &self,
        operation: Operation,
        repository: &Repository,
        args: &[&str],
        options: RunOptions,
    ) -> Result<RunOutput, OperationError> {
        let mut full = Vec::with_capacity(args.len() + 4);
        full.extend(["--git-dir", repository.git_directory.as_str(), "--work-tree", repository.worktree.as_str()]);
        full.extend_from_slice(args);
        let options = RunOptions { cwd: Some(repository.worktree.clone()), ..options };
        self.execute(&full, options).await.map_err(|cause| OperationError {
            operation,
            message: cause.message.clone(),
            directory: Some(repository.worktree.clone()),
            cause: Some(cause.message),
        })
    }

    pub(crate) async fn repository_operation(
        &self,
        operation: Operation,
        repository: &Repository,
        args: &[&str],
        options: RunOptions,
    ) -> Result<RunOutput, OperationError> {
        let output = self.repository_run(operation, repository, args, options).await?;
        if output.status == 0 {
            return Ok(output);
        }
        let stderr = output.stderr_text();
        let stdout = output.text();
        let message = if stderr.trim().is_empty() {
            if stdout.trim().is_empty() { format!("Git {operation} failed") } else { stdout.trim().to_string() }
        } else {
            stderr.trim().to_string()
        };
        Err(OperationError { operation, message, directory: Some(repository.worktree.clone()), cause: None })
    }

    pub(crate) async fn repository_operation_retry(
        &self,
        operation: Operation,
        repository: &Repository,
        args: &[&str],
        options: RunOptions,
    ) -> Result<RunOutput, OperationError> {
        let attempts = if cfg!(windows) { 3 } else { 1 };
        let mut attempt = 1;
        loop {
            match self.repository_operation(operation, repository, args, options.clone()).await {
                Ok(output) => return Ok(output),
                Err(error) if cfg!(windows) && error.cause.is_none() && attempt < attempts => {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn spawn_error(error: std::io::Error) -> SpawnError {
    SpawnError { message: error.to_string() }
}

async fn collect_output(stream: Option<impl AsyncRead + Unpin>, max_output_bytes: Option<usize>) -> (Vec<u8>, bool) {
    let Some(mut stream) = stream else {
        return (Vec::new(), false);
    };
    let limit = max_output_bytes.unwrap_or(usize::MAX);
    let mut output = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        let keep = limit.saturating_sub(total).min(read);
        output.extend_from_slice(&chunk[..keep]);
        total = total.saturating_add(read);
        truncated = truncated || total > limit;
    }
    (output, truncated)
}

async fn feed_stdin(mut stdin: Option<ChildStdin>, input: Option<Vec<u8>>) {
    let Some(stdin) = stdin.as_mut() else {
        return;
    };
    if let Some(input) = input {
        let _ = stdin.write_all(&input).await;
    }
}
