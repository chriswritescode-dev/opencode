use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use opencode_contract::generated::requests::{VcsMode, VcsUntracked};
use opencode_contract::generated::{
    FileDiffInfo, FileDiffInfoMore, FileDiffInfoStatus, VcsBase, VcsBaseSource, VcsBranch, VcsFileStatus,
    VcsFileStatusStatus, VcsInfo,
};
use opencode_git::{
    Git, MAX_PATCH_BYTES, MAX_TOTAL_PATCH_BYTES, PATCH_CONTEXT_LINES, Patch, RunOptions, chunks_by_file, empty_patch,
    split_git_patch,
};
use opencode_util::path::Flavor;
use opencode_util::temp::TempDir;

use crate::provider::{
    BranchesInput, DiffError, ProviderDiffInput, ProviderError, ProviderFuture, VcsProvider, VcsScope,
};

const CONFIG: [&str; 7] =
    ["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.longpaths=true", "-c", "core.quotepath=false"];

const MAX_FOLDER_ENTRIES: usize = 500;
const MAX_FOLDER_SCAN: usize = 5_000;
const MAX_FOLDER_SCAN_MS: u128 = 1_000;

pub struct GitProvider {
    git: Arc<Git>,
    directory: String,
    worktree: String,
}

impl GitProvider {
    pub fn new(git: Arc<Git>, directory: impl Into<String>, worktree: impl Into<String>) -> Self {
        GitProvider { git, directory: directory.into(), worktree: worktree.into() }
    }

    fn ctx(&self) -> Ctx {
        Ctx { git: self.git.clone(), directory: self.directory.clone(), worktree: self.worktree.clone() }
    }
}

impl VcsProvider for GitProvider {
    fn id(&self) -> &str {
        "git"
    }

    fn name(&self) -> &str {
        "Git"
    }

    fn init(&self, scope: VcsScope) -> Option<ProviderFuture<()>> {
        let git = self.git.clone();
        Some(Box::pin(async move {
            let result = git
                .execute(&["init"], RunOptions { cwd: Some(scope.worktree), ..RunOptions::default() })
                .await
                .map_err(|error| ProviderError::message(error.message))?;
            if result.status != 0 {
                return Err(ProviderError::message("Git initialization failed"));
            }
            Ok(())
        }))
    }

    fn info(&self, _scope: VcsScope) -> ProviderFuture<VcsInfo> {
        let ctx = self.ctx();
        Box::pin(async move {
            let (current, default) =
                tokio::join!(current_branch(&ctx.git, &ctx.directory), default_branch(&ctx.git, &ctx.directory));
            Ok(VcsInfo { provider: None, branch: VcsBranch { current, default: default.map(|root| root.name) } })
        })
    }

    fn base(&self, _scope: VcsScope) -> Option<ProviderFuture<Option<VcsBase>>> {
        let ctx = self.ctx();
        Some(Box::pin(async move { review_base(&ctx).await }))
    }

    fn branches(&self, input: BranchesInput) -> ProviderFuture<Vec<String>> {
        let ctx = self.ctx();
        Box::pin(async move { Ok(list_branches(&ctx.git, &ctx.directory, input.search.as_deref(), input.limit).await) })
    }

    fn status(&self, _scope: VcsScope) -> ProviderFuture<Vec<VcsFileStatus>> {
        let ctx = self.ctx();
        Box::pin(async move {
            let reference = has_head(&ctx.git, &ctx.directory).await.then_some("HEAD");
            let (list, stats) = tokio::join!(async { status_items(&ctx, VcsUntracked::Files).await.list }, async {
                match reference {
                    Some(reference) => diff_stats(&ctx.git, &ctx.directory, reference, None).await,
                    None => Vec::new(),
                }
            },);
            let map = nums(stats);
            let candidates = list
                .iter()
                .filter(|item| item.status == Kind::Added && !map.contains_key(&item.file))
                .map(|item| item.file.clone())
                .collect::<Vec<_>>();
            let untracked = untracked_batch(&ctx, &candidates, None).await;
            let mut sorted = list;
            sorted.sort_by(|a, b| a.file.cmp(&b.file));
            let mut rows = Vec::with_capacity(sorted.len());
            for item in sorted {
                let stat = stat_for(&ctx, &item, &map, &untracked.stats, true).await;
                rows.push(VcsFileStatus {
                    file: item.file,
                    additions: stat.as_ref().map_or(0.0, |stat| stat.additions as f64),
                    deletions: stat.as_ref().map_or(0.0, |stat| stat.deletions as f64),
                    status: vcs_status(item.status),
                });
            }
            Ok(rows)
        })
    }

    fn diff(&self, input: ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>> {
        let ctx = self.ctx();
        Box::pin(async move {
            let options = DiffOptions { context: Some(input.context), target: None, untracked: input.untracked };
            if input.mode == VcsMode::Working {
                let reference = has_head(&ctx.git, &ctx.directory).await.then_some("HEAD");
                return track(&ctx, reference, &options).await;
            }
            if !has_head(&ctx.git, &ctx.directory).await {
                if input.mode == VcsMode::Committed {
                    return Ok(Vec::new());
                }
                return track(&ctx, None, &options).await;
            }
            let base = match input.base {
                Some(base) => Some(base),
                None => default_branch(&ctx.git, &ctx.directory).await.map(|root| root.reference),
            };
            let reference = match &base {
                Some(base) => merge_base(&ctx.git, &ctx.directory, base).await,
                None => None,
            };
            let Some(reference) = reference else {
                return Err(DiffError::new(match &base {
                    Some(base) => format!("No merge base available for {base}"),
                    None => "No review base available".to_owned(),
                })
                .into());
            };
            let options =
                DiffOptions { target: (input.mode == VcsMode::Committed).then(|| "HEAD".to_owned()), ..options };
            diff_against_ref(&ctx, &reference, &options).await
        })
    }
}

#[derive(Clone)]
struct Ctx {
    git: Arc<Git>,
    directory: String,
    worktree: String,
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

#[derive(Clone, Debug)]
struct Stat {
    file: String,
    additions: usize,
    deletions: usize,
}

#[derive(Clone, Debug)]
struct GitRef {
    name: String,
    reference: String,
}

#[derive(Clone, Debug, Default)]
struct DiffOptions {
    context: Option<i32>,
    target: Option<String>,
    untracked: Option<VcsUntracked>,
}

#[derive(Clone, Debug, Default)]
struct PatchOptions {
    context: Option<i32>,
    target: Option<String>,
    max_output_bytes: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct PatchMode {
    drop_truncated: bool,
    no_index: bool,
}

#[derive(Clone, Debug)]
struct Batch {
    patches: BTreeMap<String, String>,
    capped: bool,
}

#[derive(Clone, Debug, Default)]
struct Untracked {
    stats: HashMap<String, Stat>,
    patches: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct StatusItems {
    list: Vec<Item>,
    more: Option<String>,
}

struct Run {
    status: i32,
    text: String,
    truncated: bool,
}

fn at(cwd: &str) -> RunOptions {
    RunOptions { cwd: Some(cwd.to_owned()), ..RunOptions::default() }
}

async fn run(git: &Git, args: &[&str], options: RunOptions) -> Run {
    let command = CONFIG.iter().copied().chain(args.iter().copied()).collect::<Vec<_>>();
    match git.execute(&command, options).await {
        Ok(output) => Run { status: output.status, text: output.text(), truncated: output.truncated },
        Err(_) => Run { status: 1, text: String::new(), truncated: false },
    }
}

async fn text(git: &Git, args: &[&str], cwd: &str) -> String {
    run(git, args, at(cwd)).await.text
}

async fn lines(git: &Git, args: &[&str], cwd: &str) -> Vec<String> {
    text(git, args, cwd).await.split('\n').map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect()
}

fn nuls(value: &str) -> Vec<&str> {
    value.split('\0').filter(|item| !item.is_empty()).collect()
}

fn count(value: &str) -> usize {
    if value == "-" {
        return 0;
    }
    value.parse::<usize>().unwrap_or(0)
}

fn numstat(value: &str) -> Vec<Stat> {
    nuls(value)
        .into_iter()
        .filter_map(|item| {
            let first = item.find('\t')?;
            let second = item[first + 1..].find('\t')? + first + 1;
            let file = item.get(second + 1..).filter(|file| !file.is_empty())?;
            Some(Stat {
                file: file.to_owned(),
                additions: count(&item[..first]),
                deletions: count(&item[first + 1..second]),
            })
        })
        .collect()
}

fn kind(code: &str) -> Kind {
    if code == "??" {
        return Kind::Added;
    }
    if code.contains('U') {
        return Kind::Modified;
    }
    if code.contains('A') && !code.contains('D') {
        return Kind::Added;
    }
    if code.contains('D') && !code.contains('A') {
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

fn vcs_status(kind: Kind) -> VcsFileStatusStatus {
    match kind {
        Kind::Added => VcsFileStatusStatus::Added,
        Kind::Deleted => VcsFileStatusStatus::Deleted,
        Kind::Modified => VcsFileStatusStatus::Modified,
    }
}

fn looks_like_commit(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn configured(git: &Git, cwd: &str, list: &[String]) -> Option<GitRef> {
    let name = run(git, &["config", "init.defaultBranch"], at(cwd)).await.text.trim().to_owned();
    if name.is_empty() || !list.contains(&name) {
        return None;
    }
    Some(GitRef { reference: name.clone(), name })
}

async fn primary(git: &Git, cwd: &str) -> Option<String> {
    let list = lines(git, &["remote"], cwd).await;
    if list.iter().any(|item| item == "origin") {
        return Some("origin".to_owned());
    }
    if list.len() == 1 {
        return list.first().cloned();
    }
    if list.iter().any(|item| item == "upstream") {
        return Some("upstream".to_owned());
    }
    list.first().cloned()
}

async fn current_branch(git: &Git, cwd: &str) -> Option<String> {
    let result = run(git, &["symbolic-ref", "--quiet", "--short", "HEAD"], at(cwd)).await;
    if result.status != 0 {
        return None;
    }
    let name = result.text.trim();
    if name.is_empty() { None } else { Some(name.to_owned()) }
}

async fn list_branches(git: &Git, cwd: &str, search: Option<&str>, limit: Option<usize>) -> Vec<String> {
    let search = search.map(|value| value.trim().to_lowercase()).filter(|value| !value.is_empty());
    let mut list = lines(
        git,
        &[
            "for-each-ref",
            "--sort=refname",
            "--sort=-committerdate",
            "--format=%(refname:lstrip=2)",
            "refs/heads",
            "refs/remotes",
        ],
        cwd,
    )
    .await;
    list.retain(|item| {
        !item.ends_with("/HEAD") && search.as_ref().is_none_or(|search| item.to_lowercase().contains(search))
    });
    if let Some(limit) = limit {
        list.truncate(limit);
    }
    list
}

async fn default_branch(git: &Git, cwd: &str) -> Option<GitRef> {
    if let Some(remote) = primary(git, cwd).await {
        let target = format!("refs/remotes/{remote}/HEAD");
        let head = run(git, &["symbolic-ref", &target], at(cwd)).await;
        if head.status == 0 {
            let value = head.text.trim();
            let reference = value.strip_prefix("refs/remotes/").unwrap_or(value);
            let prefix = format!("{remote}/");
            let name = reference.strip_prefix(&prefix).unwrap_or("");
            if !name.is_empty() {
                return Some(GitRef { name: name.to_owned(), reference: reference.to_owned() });
            }
        }
    }
    let list = lines(git, &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"], cwd).await;
    if let Some(next) = configured(git, cwd, &list).await {
        return Some(next);
    }
    if list.iter().any(|item| item == "main") {
        return Some(GitRef { name: "main".to_owned(), reference: "main".to_owned() });
    }
    if list.iter().any(|item| item == "master") {
        return Some(GitRef { name: "master".to_owned(), reference: "master".to_owned() });
    }
    None
}

async fn resolve(git: &Git, cwd: &str, reference: &str) -> Option<String> {
    let target = format!("{reference}^{{commit}}");
    let result = run(git, &["rev-parse", "--verify", "--end-of-options", &target], at(cwd)).await;
    if result.status != 0 {
        return None;
    }
    let value = result.text.trim();
    if value.is_empty() { None } else { Some(value.to_owned()) }
}

async fn ancestor(git: &Git, cwd: &str, commit: &str, reference: &str) -> bool {
    if !looks_like_commit(commit) {
        return false;
    }
    run(git, &["merge-base", "--is-ancestor", commit, reference], at(cwd)).await.status == 0
}

async fn named_ref(git: &Git, cwd: &str, input: &str) -> Option<GitRef> {
    if input == "HEAD"
        || input.ends_with("/HEAD")
        || input
            .chars()
            .any(|character| matches!(character, '~' | '^' | ':' | '@' | '{' | '}') || character.is_whitespace())
    {
        return None;
    }
    let reference = text(git, &["rev-parse", "--symbolic-full-name", "--verify", "--end-of-options", input], cwd)
        .await
        .trim()
        .to_owned();
    let valid = (reference.starts_with("refs/heads/") && reference.len() > "refs/heads/".len())
        || (reference.starts_with("refs/remotes/") && reference.len() > "refs/remotes/".len());
    if !valid || reference.ends_with("/HEAD") || resolve(git, cwd, &reference).await.is_none() {
        return None;
    }
    let name = match reference.strip_prefix("refs/heads/") {
        Some(name) => name.to_owned(),
        None => match reference.strip_prefix("refs/remotes/") {
            Some(rest) => rest.split_once('/').map(|(_, name)| name.to_owned()).unwrap_or_else(|| reference.clone()),
            None => reference.clone(),
        },
    };
    Some(GitRef { name, reference })
}

async fn review_base(ctx: &Ctx) -> Result<Option<VcsBase>, ProviderError> {
    let cwd = &ctx.directory;
    if !has_head(&ctx.git, cwd).await {
        return Ok(None);
    }
    let Some(current) = current_branch(&ctx.git, cwd).await else {
        return Err(DiffError::new("Choose a review base").into());
    };
    let target = format!("refs/heads/{current}");
    let history = lines(&ctx.git, &["reflog", "show", "--max-count=256", "--format=%H%x00%gs", &target], cwd).await;
    let history = history
        .iter()
        .filter_map(|line| {
            let (commit, message) = line.split_once('\0')?;
            if commit.is_empty()
                || message.is_empty()
                || !commit.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return None;
            }
            Some((commit.to_owned(), message.to_owned()))
        })
        .collect::<Vec<_>>();
    let renamed = history.iter().any(|(_, message)| message.starts_with("Branch: renamed "));
    let creation =
        if renamed { None } else { history.iter().find(|(_, message)| message.starts_with("branch: Created from ")) };
    if let Some((commit, message)) = creation {
        let origin = message.strip_prefix("branch: Created from ").unwrap_or("");
        if !origin.is_empty()
            && let Some(candidate) = named_ref(&ctx.git, cwd, origin).await
            && candidate.name != current
            && ancestor(&ctx.git, cwd, commit, "HEAD").await
            && ancestor(&ctx.git, cwd, commit, &candidate.reference).await
        {
            return Ok(Some(VcsBase {
                name: candidate.name,
                r#ref: candidate.reference,
                source: VcsBaseSource::Reflog,
            }));
        }
    }
    let Some(root) = default_branch(&ctx.git, cwd).await.filter(|root| root.name == current) else {
        return Err(DiffError::new("Choose a review base").into());
    };
    let Some(candidate) = named_ref(&ctx.git, cwd, &root.reference).await else {
        return Err(DiffError::new("The default review base is unavailable").into());
    };
    Ok(Some(VcsBase { name: root.name, r#ref: candidate.reference, source: VcsBaseSource::Default }))
}

async fn has_head(git: &Git, cwd: &str) -> bool {
    run(git, &["rev-parse", "--verify", "HEAD"], at(cwd)).await.status == 0
}

async fn merge_base(git: &Git, cwd: &str, base: &str) -> Option<String> {
    let reference = resolve(git, cwd, base).await?;
    let result = run(git, &["merge-base", &reference, "HEAD"], at(cwd)).await;
    if result.status != 0 {
        return None;
    }
    let value = result.text.trim();
    if value.is_empty() { None } else { Some(value.to_owned()) }
}

async fn status_names(git: &Git, cwd: &str, untracked: &str, names: Option<&[String]>) -> Vec<Item> {
    let pathspecs = match names {
        Some(names) => names.iter().map(|name| format!(":(literal){name}")).collect::<Vec<_>>(),
        None => vec![".".to_owned()],
    };
    let mut command = vec![
        "status".to_owned(),
        "--porcelain=v1".to_owned(),
        format!("--untracked-files={untracked}"),
        "--no-renames".to_owned(),
        "-z".to_owned(),
        "--".to_owned(),
    ];
    command.extend(pathspecs);
    let command = command.iter().map(String::as_str).collect::<Vec<_>>();
    nuls(&text(git, &command, cwd).await)
        .into_iter()
        .filter_map(|item| {
            let file = item.get(3..)?;
            if file.is_empty() {
                return None;
            }
            let code = item.get(..2)?;
            Some(Item { file: file.to_owned(), code: code.to_owned(), status: kind(code) })
        })
        .collect()
}

async fn diff_names(git: &Git, cwd: &str, reference: &str, target: Option<&str>) -> Result<Vec<Item>, ProviderError> {
    let mut command = vec!["diff", "--no-ext-diff", "--no-renames", "--name-status", "-z", reference];
    if let Some(target) = target {
        command.push(target);
    }
    command.push("--");
    command.push(".");
    let result = run(git, &command, at(cwd)).await;
    if result.status != 0 {
        return Err(DiffError::new("Unable to list Git changes").into());
    }
    let list = nuls(&result.text);
    Ok((0..list.len())
        .step_by(2)
        .filter_map(|index| {
            let code = list[index];
            let file = list.get(index + 1)?;
            if code.is_empty() || file.is_empty() {
                return None;
            }
            Some(Item { file: (*file).to_owned(), code: code.to_owned(), status: kind(code) })
        })
        .collect())
}

async fn diff_stats(git: &Git, cwd: &str, reference: &str, target: Option<&str>) -> Vec<Stat> {
    let mut command = vec!["diff", "--no-ext-diff", "--no-renames", "--numstat", "-z", reference];
    if let Some(target) = target {
        command.push(target);
    }
    command.push("--");
    command.push(".");
    numstat(&text(git, &command, cwd).await)
}

async fn diff_patch(
    git: &Git,
    cwd: &str,
    args: &[&str],
    options: Option<&PatchOptions>,
    mode: PatchMode,
    index: Option<&str>,
) -> Patch {
    let mut command = vec!["diff".to_owned()];
    if mode.no_index {
        command.push("--no-index".to_owned());
    }
    command.push("--patch".to_owned());
    command.push("--no-ext-diff".to_owned());
    command.push("--no-renames".to_owned());
    command.push(format!("--unified={}", options.and_then(|options| options.context).unwrap_or(3)));
    command.extend(args.iter().map(|arg| (*arg).to_owned()));
    let command = command.iter().map(String::as_str).collect::<Vec<_>>();
    let result = run(
        git,
        &command,
        RunOptions {
            max_output_bytes: options.and_then(|options| options.max_output_bytes),
            index: index.map(str::to_owned),
            ..at(cwd)
        },
    )
    .await;
    Patch {
        text: if mode.drop_truncated && result.truncated { String::new() } else { result.text },
        truncated: result.truncated,
    }
}

async fn patch(git: &Git, cwd: &str, reference: &str, file: &str, options: Option<&PatchOptions>) -> Patch {
    let mut args = vec![reference.to_owned()];
    if let Some(target) = options.and_then(|options| options.target.as_deref()) {
        args.push(target.to_owned());
    }
    args.push("--".to_owned());
    args.push(file.to_owned());
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    diff_patch(git, cwd, &args, options, PatchMode { drop_truncated: true, no_index: false }, None).await
}

async fn patch_all(git: &Git, cwd: &str, reference: &str, options: Option<&PatchOptions>) -> Patch {
    let mut args = vec![reference.to_owned()];
    if let Some(target) = options.and_then(|options| options.target.as_deref()) {
        args.push(target.to_owned());
    }
    args.push("--".to_owned());
    args.push(".".to_owned());
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    diff_patch(git, cwd, &args, options, PatchMode { drop_truncated: false, no_index: false }, None).await
}

async fn patch_untracked(git: &Git, cwd: &str, file: &str, options: Option<&PatchOptions>) -> Patch {
    if file.ends_with('/') {
        return Patch { text: String::new(), truncated: false };
    }
    diff_patch(git, cwd, &["--", "/dev/null", file], options, PatchMode { drop_truncated: true, no_index: true }, None)
        .await
}

async fn stat_untracked(git: &Git, cwd: &str, file: &str) -> Option<Stat> {
    if file.ends_with('/') {
        return None;
    }
    let result = run(
        git,
        &["diff", "--no-index", "--numstat", "--", "/dev/null", file],
        RunOptions { max_output_bytes: Some(4096), ..at(cwd) },
    )
    .await;
    if result.truncated {
        return None;
    }
    let parts = result.text.split('\t').collect::<Vec<_>>();
    let additions = parts.first()?;
    let deletions = parts.get(1)?;
    Some(Stat { file: file.to_owned(), additions: count(additions), deletions: count(deletions) })
}

async fn intent_to_add(git: &Git, cwd: &str, list: &str, index: Option<&str>) {
    let path = format!("--pathspec-from-file={list}");
    let command = [
        "--literal-pathspecs",
        "-c",
        "core.splitIndex=false",
        "add",
        "--intent-to-add",
        "--force",
        "--sparse",
        "--ignore-errors",
        &path,
        "--pathspec-file-nul",
    ];
    run(git, &command, RunOptions { index: index.map(str::to_owned), ..at(cwd) }).await;
}

async fn worktree_stats(git: &Git, cwd: &str, index: Option<&str>) -> Vec<Stat> {
    let result = run(
        git,
        &["diff", "--no-ext-diff", "--no-renames", "--numstat", "-z"],
        RunOptions { index: index.map(str::to_owned), ..at(cwd) },
    )
    .await;
    numstat(&result.text)
}

async fn worktree_patch(git: &Git, cwd: &str, options: Option<&PatchOptions>, index: Option<&str>) -> Patch {
    diff_patch(git, cwd, &[], options, PatchMode { drop_truncated: false, no_index: false }, index).await
}

async fn untracked_batch(ctx: &Ctx, files: &[String], patches: Option<&DiffOptions>) -> Untracked {
    let eligible = futures_util::future::join_all(files.iter().map(|file| {
        let file = file.clone();
        let worktree = ctx.worktree.clone();
        async move {
            if file.ends_with('/') {
                return None;
            }
            let metadata = tokio::fs::symlink_metadata(Path::new(&worktree).join(&file)).await.ok()?;
            if !metadata.file_type().is_file() {
                return None;
            }
            if metadata.len() > (MAX_TOTAL_PATCH_BYTES / 2) as u64 {
                return None;
            }
            Some(file)
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if eligible.len() < 2 {
        return Untracked::default();
    }
    let Ok(temp) = TempDir::new() else {
        return Untracked::default();
    };
    let base = temp.path().join("untracked").to_string_lossy().into_owned();
    let list = format!("{base}.list");
    let index = format!("{base}.index");
    let content = eligible.iter().map(|file| format!("{file}\0")).collect::<String>();
    if tokio::fs::write(&list, content).await.is_err() {
        return Untracked::default();
    }
    intent_to_add(&ctx.git, &ctx.worktree, &list, Some(&index)).await;
    let (stats, patch) = tokio::join!(worktree_stats(&ctx.git, &ctx.worktree, Some(&index)), async {
        match patches {
            Some(options) => {
                worktree_patch(
                    &ctx.git,
                    &ctx.worktree,
                    Some(&PatchOptions {
                        context: Some(options.context.unwrap_or(PATCH_CONTEXT_LINES)),
                        target: None,
                        max_output_bytes: Some(MAX_TOTAL_PATCH_BYTES),
                    }),
                    Some(&index),
                )
                .await
            }
            None => Patch { text: String::new(), truncated: false },
        }
    },);
    let wanted = eligible.iter().cloned().collect::<HashSet<_>>();
    let headers =
        eligible.iter().map(|file| (format!("diff --git a/{file} b/{file}"), file.clone())).collect::<HashMap<_, _>>();
    let stats =
        stats.into_iter().filter(|stat| wanted.contains(&stat.file)).map(|stat| (stat.file.clone(), stat)).collect();
    let patches = split_git_patch(&patch)
        .into_iter()
        .filter_map(|chunk| {
            let header = chunk.split('\n').next().unwrap_or(chunk);
            let file = headers.get(header)?;
            Some((file.clone(), chunk.to_owned()))
        })
        .collect();
    Untracked { stats, patches }
}

async fn status_items(ctx: &Ctx, untracked: VcsUntracked) -> StatusItems {
    if untracked == VcsUntracked::Files {
        return StatusItems { list: status_names(&ctx.git, &ctx.directory, "all", None).await, more: None };
    }
    let list = status_names(&ctx.git, &ctx.directory, "normal", None).await;
    let prefix = Flavor::NATIVE.relative(&ctx.worktree, &ctx.directory).replace('\\', "/");
    let covered = list
        .iter()
        .any(|item| item.code == "??" && item.file.ends_with('/') && format!("{prefix}/").starts_with(&item.file));
    if prefix.is_empty() || !covered {
        return StatusItems { list, more: None };
    }
    let Ok(mut entries) = tokio::fs::read_dir(&ctx.directory).await else {
        return StatusItems { list, more: None };
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    let mut found = Vec::new();
    let started = Instant::now();
    let mut checked = 0usize;
    while checked < names.len()
        && found.len() < MAX_FOLDER_ENTRIES
        && checked < MAX_FOLDER_SCAN
        && started.elapsed().as_millis() < MAX_FOLDER_SCAN_MS
    {
        let end = (checked + MAX_FOLDER_ENTRIES).min(names.len());
        let batch = names[checked..end].to_vec();
        checked = end;
        found.extend(status_names(&ctx.git, &ctx.directory, "normal", Some(&batch)).await);
    }
    let more = (found.len() > MAX_FOLDER_ENTRIES || checked < names.len()).then(|| format!("{prefix}/"));
    found.truncate(MAX_FOLDER_ENTRIES);
    StatusItems { list: found, more }
}

fn with_more(mut rows: Vec<FileDiffInfo>, folder: Option<String>) -> Vec<FileDiffInfo> {
    if let Some(folder) = folder {
        rows.push(FileDiffInfo {
            file: folder,
            patch: String::new(),
            additions: 0.0,
            deletions: 0.0,
            status: FileDiffInfoStatus::Added,
            more: Some(FileDiffInfoMore::True),
        });
    }
    rows
}

fn nums(stats: Vec<Stat>) -> HashMap<String, Stat> {
    stats.into_iter().map(|stat| (stat.file.clone(), stat)).collect()
}

fn merge(lists: Vec<Vec<Item>>) -> Vec<Item> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in lists.into_iter().flatten() {
        if seen.insert(item.file.clone()) {
            out.push(item);
        }
    }
    out
}

fn empty_batch() -> Batch {
    Batch { patches: BTreeMap::new(), capped: false }
}

async fn batch_patches(ctx: &Ctx, reference: &str, list: &[Item], options: &DiffOptions) -> Batch {
    if list.is_empty() {
        return empty_batch();
    }
    let result = patch_all(
        &ctx.git,
        &ctx.directory,
        reference,
        Some(&PatchOptions {
            context: Some(options.context.unwrap_or(PATCH_CONTEXT_LINES)),
            target: options.target.clone(),
            max_output_bytes: Some(MAX_TOTAL_PATCH_BYTES),
        }),
    )
    .await;
    Batch {
        patches: chunks_by_file(&result, |index| list.get(index).map(|item| item.file.clone())),
        capped: result.truncated,
    }
}

async fn native_patch(ctx: &Ctx, reference: Option<&str>, item: &Item, options: &DiffOptions) -> String {
    let context = Some(options.context.unwrap_or(PATCH_CONTEXT_LINES));
    let result = match reference {
        Some(reference) if item.code != "??" => {
            patch(
                &ctx.git,
                &ctx.worktree,
                reference,
                &item.file,
                Some(&PatchOptions {
                    context,
                    target: options.target.clone(),
                    max_output_bytes: Some(MAX_PATCH_BYTES),
                }),
            )
            .await
        }
        _ => {
            patch_untracked(
                &ctx.git,
                &ctx.worktree,
                &item.file,
                Some(&PatchOptions { context, target: None, max_output_bytes: Some(MAX_PATCH_BYTES) }),
            )
            .await
        }
    };
    if !result.truncated && !result.text.is_empty() { result.text } else { empty_patch(&item.file) }
}

fn total_patch(file: &str, patch: String, total: usize) -> (String, bool) {
    if total.saturating_add(patch.len()) <= MAX_TOTAL_PATCH_BYTES {
        return (patch, false);
    }
    (empty_patch(file), true)
}

async fn patch_for_item(
    ctx: &Ctx,
    reference: Option<&str>,
    item: &Item,
    batch: &Batch,
    untracked: &HashMap<String, String>,
    capped: bool,
    options: &DiffOptions,
) -> String {
    if capped {
        return empty_patch(&item.file);
    }
    if let Some(patch) = batch.patches.get(&item.file) {
        return patch.clone();
    }
    if item.code != "??" && batch.capped {
        return empty_patch(&item.file);
    }
    if (item.code == "??" || reference.is_none())
        && let Some(patch) = untracked.get(&item.file)
    {
        return patch.clone();
    }
    native_patch(ctx, reference, item, options).await
}

async fn stat_for(
    ctx: &Ctx,
    item: &Item,
    map: &HashMap<String, Stat>,
    untracked: &HashMap<String, Stat>,
    allow_untracked: bool,
) -> Option<Stat> {
    if let Some(stat) = map.get(&item.file) {
        return Some(stat.clone());
    }
    if !allow_untracked || item.status != Kind::Added {
        return None;
    }
    if let Some(stat) = untracked.get(&item.file) {
        return Some(stat.clone());
    }
    stat_untracked(&ctx.git, &ctx.worktree, &item.file).await
}

async fn files(
    ctx: &Ctx,
    reference: Option<&str>,
    list: Vec<Item>,
    map: &HashMap<String, Stat>,
    batch: Batch,
    options: &DiffOptions,
) -> Vec<FileDiffInfo> {
    let candidates = list
        .iter()
        .filter(|item| {
            item.code == "??"
                || reference.is_none()
                || (options.target.is_none() && item.status == Kind::Added && !map.contains_key(&item.file))
        })
        .map(|item| item.file.clone())
        .collect::<Vec<_>>();
    let untracked = untracked_batch(ctx, &candidates, Some(options)).await;
    let mut sorted = list;
    sorted.sort_by(|a, b| a.file.cmp(&b.file));
    let mut next = Vec::new();
    let mut total = 0usize;
    let mut capped = false;
    for item in sorted {
        let stat = stat_for(ctx, &item, map, &untracked.stats, options.target.is_none()).await;
        let patch = patch_for_item(ctx, reference, &item, &batch, &untracked.patches, capped, options).await;
        let (patch, patch_capped) = if capped { (patch, true) } else { total_patch(&item.file, patch, total) };
        capped = capped || patch_capped;
        if !capped {
            total = total.saturating_add(patch.len());
            capped = total >= MAX_TOTAL_PATCH_BYTES;
        }
        next.push(FileDiffInfo {
            file: item.file,
            patch,
            additions: stat.as_ref().map_or(0.0, |stat| stat.additions as f64),
            deletions: stat.as_ref().map_or(0.0, |stat| stat.deletions as f64),
            status: file_status(item.status),
            more: None,
        });
    }
    next
}

async fn diff_against_ref(
    ctx: &Ctx,
    reference: &str,
    options: &DiffOptions,
) -> Result<Vec<FileDiffInfo>, ProviderError> {
    let target = options.target.as_deref();
    let (list, stats, extra) = tokio::join!(
        diff_names(&ctx.git, &ctx.directory, reference, target),
        diff_stats(&ctx.git, &ctx.directory, reference, target),
        async {
            match target {
                Some(_) => StatusItems { list: Vec::new(), more: None },
                None => status_items(ctx, options.untracked.unwrap_or(VcsUntracked::Files)).await,
            }
        },
    );
    let list = list?;
    let batch = batch_patches(ctx, reference, &list, options).await;
    let merged = merge(vec![list, extra.list.into_iter().filter(|item| item.code == "??").collect()]);
    let rows = files(ctx, Some(reference), merged, &nums(stats), batch, options).await;
    Ok(with_more(rows, extra.more))
}

async fn track(ctx: &Ctx, reference: Option<&str>, options: &DiffOptions) -> Result<Vec<FileDiffInfo>, ProviderError> {
    let Some(reference) = reference else {
        let status = status_items(ctx, options.untracked.unwrap_or(VcsUntracked::Files)).await;
        let rows = files(ctx, None, status.list, &HashMap::new(), empty_batch(), options).await;
        return Ok(with_more(rows, status.more));
    };
    diff_against_ref(ctx, reference, options).await
}
