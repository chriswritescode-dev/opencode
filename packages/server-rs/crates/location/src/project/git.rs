use std::path::{Component, Path, PathBuf};

use crate::environment::{CommandOutput, CommandSpec, FileType, Files, Spawner, type_following};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub worktree: String,
    pub git_directory: String,
    pub common_directory: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub directory: String,
    pub kind: WorktreeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeKind {
    Main,
    Linked,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("{0}")]
    Failed(String),
}

pub async fn discover(spawner: &dyn Spawner, directory: &str) -> Result<Option<Repository>, GitError> {
    let output = run(spawner, directory, &["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"]).await?;
    if output.status != 0 {
        return Ok(None);
    }
    let stdout = decode(&output.stdout);
    let mut lines = stdout.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
    let Some(git_directory) = lines.next().filter(|line| !line.is_empty()) else {
        return Ok(None);
    };
    let Some(common_directory) = lines.next().filter(|line| !line.is_empty()) else {
        return Ok(None);
    };
    let worktree = lines
        .next()
        .filter(|line| !line.is_empty())
        .map_or_else(|| directory.to_string(), |line| resolve_path(directory, line.trim()));
    Ok(Some(Repository {
        worktree,
        git_directory: resolve_path(directory, git_directory),
        common_directory: resolve_path(directory, common_directory),
    }))
}

pub async fn remote_get(spawner: &dyn Spawner, repository: &Repository) -> Result<Option<String>, GitError> {
    let args = ["remote", "get-url", "origin"];
    let output = run(spawner, &repository.worktree, &args).await?;
    if output.status == 2 {
        return Ok(None);
    }
    if output.status != 0 {
        return Err(failed(&args, &output));
    }
    Ok(non_empty(decode(&output.stdout).trim()))
}

pub async fn root_commits(spawner: &dyn Spawner, repository: &Repository) -> Result<Vec<String>, GitError> {
    let args = ["rev-list", "--max-parents=0", "HEAD"];
    let output = run(spawner, &repository.worktree, &args).await?;
    if output.status == 0 {
        let stdout = decode(&output.stdout);
        let mut commits: Vec<String> =
            stdout.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect();
        commits.sort();
        return Ok(commits);
    }
    let verify = run(spawner, &repository.worktree, &["rev-parse", "-q", "--verify", "HEAD"]).await?;
    if verify.status == 1 {
        return Ok(Vec::new());
    }
    Err(failed(&args, &output))
}

pub async fn worktree_list(spawner: &dyn Spawner, repository: &Repository) -> Result<Vec<WorktreeEntry>, GitError> {
    let args = ["worktree", "list", "--porcelain"];
    let output = run(spawner, &repository.worktree, &args).await?;
    if output.status != 0 {
        return Err(failed(&args, &output));
    }
    let stdout = decode(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::trim)
        .enumerate()
        .map(|(index, directory)| WorktreeEntry {
            directory: resolve_path(&repository.worktree, directory),
            kind: if index == 0 { WorktreeKind::Main } else { WorktreeKind::Linked },
        })
        .collect())
}

pub async fn fingerprint(files: &dyn Files, dot_git: &str) -> Option<String> {
    let kind = type_following(files, dot_git).await.ok()?;
    if kind != FileType::File && kind != FileType::Directory {
        return None;
    }
    let git_directory = match kind {
        FileType::File => {
            let contents = read_text(files, dot_git).await?;
            resolve_path(&parent(dot_git), git_target(&contents)?)
        }
        _ => dot_git.to_string(),
    };
    let common_directory = match read_text(files, &join(&git_directory, "commondir")).await {
        Some(contents) => {
            let target = contents.trim();
            if target.is_empty() { git_directory.clone() } else { resolve_path(&git_directory, target) }
        }
        None => git_directory.clone(),
    };
    let head = read_text(files, &join(&git_directory, "HEAD")).await?.trim().to_string();
    let reference = head.strip_prefix("ref:").map(str::trim).filter(|value| !value.is_empty());
    let mut watched = vec![
        join(&git_directory, "HEAD"),
        join(&common_directory, "config"),
        join(&common_directory, "config.worktree"),
        join(&git_directory, "config.worktree"),
        join(&common_directory, "packed-refs"),
        join(&common_directory, "reftable/tables.list"),
        join(&common_directory, "opencode"),
    ];
    if let Some(reference) = reference {
        watched.push(join(&common_directory, reference));
        watched.push(join(&git_directory, reference));
    }
    let home = std::env::var("HOME").unwrap_or_default();
    watched.push(env_value("GIT_CONFIG_GLOBAL").unwrap_or_else(|| join(&home, ".gitconfig")));
    let config_home = env_value("XDG_CONFIG_HOME").unwrap_or_else(|| join(&home, ".config"));
    watched.push(join(&config_home, "git/config"));
    if !system_config_disabled() {
        watched.push(env_value("GIT_CONFIG_SYSTEM").unwrap_or_else(|| "/etc/gitconfig".to_string()));
    }
    let mut stamps = Vec::with_capacity(watched.len() + 1);
    for path in std::iter::once(dot_git).chain(watched.iter().map(String::as_str)) {
        stamps.push(stamp(files, path).await);
    }
    let mut parts = vec![git_directory, common_directory, head];
    parts.extend(watched);
    parts.extend(stamps);
    Some(parts.join("\0"))
}

async fn run(spawner: &dyn Spawner, cwd: &str, args: &[&str]) -> Result<CommandOutput, GitError> {
    let command = CommandSpec {
        program: "git".to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        cwd: Some(cwd.to_string()),
        ..Default::default()
    };
    spawner.spawn(&command).await.map_err(|error| GitError::Failed(error.to_string()))
}

async fn stamp(files: &dyn Files, path: &str) -> String {
    match files.stat(path).await {
        Ok(info) => format!("{kind}:{size}:{mtime:.0}", kind = info.kind, size = info.size, mtime = info.mtime_ms),
        Err(_) => "missing".to_string(),
    }
}

async fn read_text(files: &dyn Files, path: &str) -> Option<String> {
    let read = files.read(path, None).await.ok()?;
    String::from_utf8(read.bytes).ok()
}

fn git_target(contents: &str) -> Option<&str> {
    contents.lines().find_map(|line| {
        let target = line.strip_prefix("gitdir:")?.trim();
        (!target.is_empty()).then_some(target)
    })
}

fn failed(args: &[&str], output: &CommandOutput) -> GitError {
    let stderr = decode(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        return GitError::Failed(format!("git {} exited with {status}", args.join(" "), status = output.status));
    }
    GitError::Failed(message.to_string())
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn system_config_disabled() -> bool {
    let value = std::env::var("GIT_CONFIG_NOSYSTEM").unwrap_or_default();
    matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn resolve_path(base: &str, path: &str) -> String {
    normalize(&Path::new(base).join(path)).to_string_lossy().into_owned()
}

fn parent(path: &str) -> String {
    Path::new(path).parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default()
}

fn join(base: &str, path: &str) -> String {
    Path::new(base).join(path).to_string_lossy().into_owned()
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => normalized.push(component.as_os_str()),
            },
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
