use std::path::Path;
use std::process::Stdio;

use opencode_contract::generated::requests::VcsMode;
use opencode_contract::generated::{
    FileDiffInfo, FileDiffInfoStatus, VcsBranch, VcsFileStatus, VcsFileStatusStatus, VcsInfo,
};
use opencode_git::{
    MAX_PATCH_BYTES, MAX_TOTAL_PATCH_BYTES, Patch, add_patch, chunks_by_file, count_patch, delete_patch, empty_patch,
};
use opencode_util::path::Flavor;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::provider::{
    BranchesInput, DiffError, ProviderDiffInput, ProviderError, ProviderFuture, VcsProvider, VcsScope,
};

pub struct HgProvider {
    worktree: String,
    scope: String,
}

impl HgProvider {
    pub fn new(directory: impl Into<String>, worktree: impl Into<String>) -> Self {
        let directory = directory.into();
        let worktree = worktree.into();
        let relative = Flavor::NATIVE.relative(&worktree, &directory);
        let scope = if relative.is_empty() { ".".to_owned() } else { relative };
        HgProvider { worktree, scope }
    }

    fn hg(&self) -> Hg {
        Hg { worktree: self.worktree.clone() }
    }
}

impl VcsProvider for HgProvider {
    fn id(&self) -> &str {
        "hg"
    }

    fn name(&self) -> &str {
        "Mercurial"
    }

    fn init(&self, scope: VcsScope) -> Option<ProviderFuture<()>> {
        let hg = Hg { worktree: scope.worktree };
        Some(Box::pin(async move {
            if hg.run(&["init"], None).await.status != 0 {
                return Err(ProviderError::message("Mercurial initialization failed"));
            }
            Ok(())
        }))
    }

    fn info(&self, _scope: VcsScope) -> ProviderFuture<VcsInfo> {
        let hg = self.hg();
        Box::pin(async move {
            let branch = hg.branch().await;
            Ok(VcsInfo { provider: None, branch: VcsBranch { current: branch, default: Some("default".to_owned()) } })
        })
    }

    fn branches(&self, _input: BranchesInput) -> ProviderFuture<Vec<String>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn status(&self, _scope: VcsScope) -> ProviderFuture<Vec<VcsFileStatus>> {
        let hg = self.hg();
        let scope = self.scope.clone();
        Box::pin(async move {
            let rows = hg.diff_against(&scope, None, 0).await;
            Ok(rows
                .into_iter()
                .map(|row| VcsFileStatus {
                    file: row.file,
                    additions: row.additions,
                    deletions: row.deletions,
                    status: vcs_status(row.status),
                })
                .collect())
        })
    }

    fn diff(&self, input: ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>> {
        let hg = self.hg();
        let scope = self.scope.clone();
        Box::pin(async move {
            if input.mode == VcsMode::Working {
                return Ok(hg.diff_against(&scope, None, input.context).await);
            }
            if input.mode == VcsMode::Committed || input.base.is_some() {
                return Err(DiffError::new(
                    "The Mercurial provider does not support committed reviews or explicit bases",
                )
                .into());
            }
            let branch = hg.branch().await;
            if branch.as_deref().is_none_or(|branch| branch == "default") {
                return Ok(Vec::new());
            }
            let Some(ancestor) = hg.ancestor().await else {
                return Ok(Vec::new());
            };
            Ok(hg.diff_against(&scope, Some(&ancestor), input.context).await)
        })
    }
}

struct Hg {
    worktree: String,
}

impl Hg {
    async fn run(&self, args: &[&str], max_output_bytes: Option<usize>) -> Run {
        command(&self.worktree, args, max_output_bytes).await
    }

    async fn status(&self, rev: Option<&str>, scope: &str) -> Vec<Item> {
        let mut args = vec!["status", "-0"];
        if let Some(rev) = rev {
            args.push("--rev");
            args.push(rev);
        }
        args.push(scope);
        let result = self.run(&args, None).await;
        if result.status != 0 {
            return Vec::new();
        }
        result
            .text
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| {
                let file = entry.get(2..)?;
                if file.is_empty() {
                    return None;
                }
                let code = entry.get(..1)?;
                Some(Item { file: file.to_owned(), code: code.to_owned(), status: kind(code) })
            })
            .collect()
    }

    async fn diff(&self, rev: Option<&str>, scope: &str, context: i32) -> Patch {
        let unified = context.to_string();
        let mut args: Vec<&str> = vec!["diff", "--git", "--unified", unified.as_str()];
        if let Some(rev) = rev {
            args.push("-r");
            args.push(rev);
        }
        args.push(scope);
        let result = self.run(&args, Some(MAX_TOTAL_PATCH_BYTES)).await;
        if result.status != 0 {
            return Patch { text: String::new(), truncated: false };
        }
        Patch { text: result.text, truncated: result.truncated }
    }

    async fn branch(&self) -> Option<String> {
        let result = self.run(&["branch"], None).await;
        if result.status != 0 {
            return None;
        }
        let branch = result.text.trim();
        if branch.is_empty() { None } else { Some(branch.to_owned()) }
    }

    async fn ancestor(&self) -> Option<String> {
        let result = self.run(&["log", "-r", "ancestor(., default)", "-T", "{node}"], None).await;
        if result.status != 0 {
            return None;
        }
        let ancestor = result.text.trim();
        if ancestor.is_empty() { None } else { Some(ancestor.to_owned()) }
    }

    async fn cat(&self, rev: &str, file: &str) -> Option<String> {
        let result = self.run(&["cat", "-r", rev, "--", file], Some(MAX_PATCH_BYTES)).await;
        if result.status != 0 || result.truncated {
            return None;
        }
        Some(result.text)
    }

    async fn diff_against(&self, scope: &str, rev: Option<&str>, context: i32) -> Vec<FileDiffInfo> {
        let (mut items, batch) = tokio::join!(self.status(rev, scope), self.diff(rev, scope, context));
        let chunks = chunks_by_file(&batch, |_| None);
        items.sort_by(|a, b| a.file.cmp(&b.file));
        let mut list = Vec::with_capacity(items.len());
        for item in items {
            let patch = self.patch_for(&item, rev, chunks.get(&item.file)).await;
            let counts = count_patch(&patch);
            list.push(FileDiffInfo {
                file: item.file,
                patch,
                additions: counts.additions as f64,
                deletions: counts.deletions as f64,
                status: file_status(item.status),
                more: None,
            });
        }
        list
    }

    async fn patch_for(&self, item: &Item, rev: Option<&str>, chunk: Option<&String>) -> String {
        if let Some(chunk) = chunk {
            return chunk.clone();
        }
        if item.code == "?" {
            let Ok(content) = tokio::fs::read(Path::new(&self.worktree).join(&item.file)).await else {
                return empty_patch(&item.file);
            };
            if content.len() > MAX_PATCH_BYTES {
                return empty_patch(&item.file);
            }
            return add_patch(&item.file, &String::from_utf8_lossy(&content));
        }
        if item.code == "!" {
            let Some(content) = self.cat(rev.unwrap_or("."), &item.file).await else {
                return empty_patch(&item.file);
            };
            return delete_patch(&item.file, &content);
        }
        empty_patch(&item.file)
    }
}

struct Run {
    status: i32,
    text: String,
    truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Added,
    Deleted,
    Modified,
}

#[derive(Clone, Debug)]
struct Item {
    file: String,
    code: String,
    status: Kind,
}

async fn command(worktree: &str, args: &[&str], max_output_bytes: Option<usize>) -> Run {
    let mut command = Command::new("hg");
    command.args(args);
    command.current_dir(worktree);
    command.env("HGPLAIN", "1");
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return Run { status: 1, text: String::new(), truncated: false };
    };
    let ((stdout, truncated), _) = tokio::join!(
        collect_output(child.stdout.take(), max_output_bytes),
        collect_output(child.stderr.take(), max_output_bytes),
    );
    let status = child.wait().await.ok().and_then(|status| status.code()).unwrap_or(1);
    Run { status, text: String::from_utf8_lossy(&stdout).into_owned(), truncated }
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

fn kind(code: &str) -> Kind {
    if code == "A" || code == "?" {
        return Kind::Added;
    }
    if code == "R" || code == "!" {
        return Kind::Deleted;
    }
    Kind::Modified
}

fn file_status(kind: Kind) -> FileDiffInfoStatus {
    match kind {
        Kind::Added => FileDiffInfoStatus::Added,
        Kind::Deleted => FileDiffInfoStatus::Deleted,
        Kind::Modified => FileDiffInfoStatus::Modified,
    }
}

fn vcs_status(status: FileDiffInfoStatus) -> VcsFileStatusStatus {
    match status {
        FileDiffInfoStatus::Added => VcsFileStatusStatus::Added,
        FileDiffInfoStatus::Deleted => VcsFileStatusStatus::Deleted,
        FileDiffInfoStatus::Modified => VcsFileStatusStatus::Modified,
    }
}
