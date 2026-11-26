use std::path::{Component, Path, PathBuf};

use crate::{Git, WorktreeError, WorktreeOperation};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub worktree: String,
    pub git_directory: String,
    pub common_directory: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeKind {
    Main,
    Linked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub directory: String,
    pub kind: WorktreeKind,
}

impl Git {
    pub async fn discover(&self, directory: &str) -> Option<Repository> {
        let dot_git = dot_git_entry(directory).await?;
        let cwd = parent(&dot_git);
        let output = self.run(&cwd, &["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"]).await;
        let stdout = output.text();
        let mut lines = stdout.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
        let git_directory = lines.next().filter(|line| !line.is_empty())?;
        let common_directory = lines.next().filter(|line| !line.is_empty())?;
        let worktree = lines
            .next()
            .filter(|line| !line.is_empty())
            .map_or_else(|| cwd.clone(), |line| resolve_path(&cwd, line.trim()));
        Some(Repository {
            worktree,
            git_directory: resolve_path(&cwd, git_directory),
            common_directory: resolve_path(&cwd, common_directory),
        })
    }

    pub async fn fingerprint(&self, dot_git: &str) -> Option<String> {
        let metadata = tokio::fs::metadata(dot_git).await.ok()?;
        if !metadata.is_file() && !metadata.is_dir() {
            return None;
        }
        let git_directory = if metadata.is_file() {
            let contents = read_text(dot_git).await?;
            resolve_path(&parent(dot_git), git_target(&contents)?)
        } else {
            dot_git.to_string()
        };
        let common_directory = match read_text(&join(&git_directory, "commondir")).await {
            Some(contents) => {
                let target = contents.trim();
                if target.is_empty() { git_directory.clone() } else { resolve_path(&git_directory, target) }
            }
            None => git_directory.clone(),
        };
        let head = read_text(&join(&git_directory, "HEAD")).await?.trim().to_string();
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
        let home = self
            .var("HOME")
            .or_else(|| std::env::home_dir().map(|path| path.to_string_lossy().into_owned()))
            .unwrap_or_default();
        watched.push(self.var("GIT_CONFIG_GLOBAL").unwrap_or_else(|| join(&home, ".gitconfig")));
        let config_home = self.var("XDG_CONFIG_HOME").unwrap_or_else(|| join(&home, ".config"));
        watched.push(join(&config_home, "git/config"));
        if !system_config_disabled(self.var("GIT_CONFIG_NOSYSTEM").as_deref().unwrap_or_default()) {
            watched.push(self.var("GIT_CONFIG_SYSTEM").unwrap_or_else(|| "/etc/gitconfig".to_string()));
        }
        let mut stamps = Vec::with_capacity(watched.len() + 1);
        for path in std::iter::once(dot_git).chain(watched.iter().map(String::as_str)) {
            stamps.push(stamp(path).await);
        }
        let mut parts = vec![git_directory, common_directory, head];
        parts.extend(watched);
        parts.extend(stamps);
        Some(parts.join("\0"))
    }

    pub async fn worktree_list(&self, repository: &Repository) -> Result<Vec<Worktree>, WorktreeError> {
        let output = self.run(&repository.worktree, &["worktree", "list", "--porcelain"]).await;
        if output.status != 0 {
            return Err(WorktreeError::failed(WorktreeOperation::List, None, &output));
        }
        Ok(output
            .text()
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(str::trim)
            .enumerate()
            .map(|(index, directory)| Worktree {
                directory: resolve_path(&repository.worktree, directory),
                kind: if index == 0 { WorktreeKind::Main } else { WorktreeKind::Linked },
            })
            .collect())
    }
}

async fn dot_git_entry(directory: &str) -> Option<String> {
    for candidate in Path::new(directory).ancestors() {
        let dot_git = candidate.join(".git");
        if tokio::fs::metadata(&dot_git).await.is_ok() {
            return Some(dot_git.to_string_lossy().into_owned());
        }
    }
    None
}

async fn stamp(path: &str) -> String {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return "missing".to_string();
    };
    let kind = if metadata.is_file() {
        "File"
    } else if metadata.is_dir() {
        "Directory"
    } else {
        "Other"
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{kind}:{ino}:{size}:{mtime}", ino = inode(&metadata), size = metadata.len())
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(metadata)
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

async fn read_text(path: &str) -> Option<String> {
    String::from_utf8(tokio::fs::read(path).await.ok()?).ok()
}

fn git_target(contents: &str) -> Option<&str> {
    contents.lines().find_map(|line| {
        let target = line.strip_prefix("gitdir:")?.trim();
        (!target.is_empty()).then_some(target)
    })
}

fn system_config_disabled(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn resolve_path(cwd: &str, value: &str) -> String {
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return normalize(Path::new(cwd)).to_string_lossy().into_owned();
    }
    let path = Path::new(trimmed);
    let resolved = if path.is_absolute() { path.to_path_buf() } else { Path::new(cwd).join(path) };
    normalize(&resolved).to_string_lossy().into_owned()
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
