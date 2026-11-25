use crate::run::RunOutput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Clone,
    Fetch,
    Checkout,
    Reset,
    Create,
    Refresh,
    WriteTree,
    ListFiles,
    Diff,
    Restore,
    Pack,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Operation::Clone => "clone",
            Operation::Fetch => "fetch",
            Operation::Checkout => "checkout",
            Operation::Reset => "reset",
            Operation::Create => "create",
            Operation::Refresh => "refresh",
            Operation::WriteTree => "write_tree",
            Operation::ListFiles => "list_files",
            Operation::Diff => "diff",
            Operation::Restore => "restore",
            Operation::Pack => "pack",
        }
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Git {operation} failed: {message}")]
pub struct OperationError {
    pub operation: Operation,
    pub message: String,
    pub directory: Option<String>,
    pub cause: Option<String>,
}

#[derive(Debug)]
pub struct CommandError {
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub cause: Option<String>,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Git {} failed", self.args.join(" "))?;
        match self.exit_code {
            Some(code) => write!(formatter, " with exit code {code}"),
            None => formatter.write_str(" to start"),
        }
    }
}

impl std::error::Error for CommandError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeOperation {
    Create,
    Remove,
    List,
}

impl WorktreeOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            WorktreeOperation::Create => "create",
            WorktreeOperation::Remove => "remove",
            WorktreeOperation::List => "list",
        }
    }
}

impl std::fmt::Display for WorktreeOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Git worktree {operation} failed: {message}")]
pub struct WorktreeError {
    pub operation: WorktreeOperation,
    pub message: String,
    pub directory: Option<String>,
    pub force_required: bool,
    pub cause: Option<String>,
}

impl WorktreeError {
    pub(crate) fn failed(operation: WorktreeOperation, directory: Option<String>, output: &RunOutput) -> WorktreeError {
        let stderr = output.stderr_text();
        let stdout = output.text();
        let message = if stderr.trim().is_empty() {
            if stdout.trim().is_empty() { "Git failed".to_string() } else { stdout.trim().to_string() }
        } else {
            stderr.trim().to_string()
        };
        let force_required = operation == WorktreeOperation::Remove && dirty(&message);
        WorktreeError { operation, message, directory, force_required, cause: None }
    }
}

fn dirty(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("contains modified or untracked files") || message.contains("is dirty")
}
