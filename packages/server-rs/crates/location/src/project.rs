pub mod git;
pub mod icon;

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use opencode_storage::StorageError;
use opencode_storage::project::{self, ProjectMetadataUpdate, ProjectRow};
use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
use rusqlite::TransactionBehavior;
use serde_json::Value;
use tokio::sync::broadcast;
use url::Url;

use crate::environment::{CommandSpec, Environment, Files, Spawner, exists};
use crate::events::LocationEvent;

pub const GLOBAL_ID: &str = "global";
pub const ACTIVATE_INTERVAL_MS: i64 = 60_000;
pub const SWEEP_INTERVAL_MS: i64 = 3_600_000;

const GIT_CACHE_CAPACITY: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub id: String,
    pub canonical: String,
    pub vcs: Option<String>,
    pub name: Option<String>,
    pub icon: Option<ProjectIcon>,
    pub commands: Option<ProjectCommands>,
    pub time: ProjectTime,
    pub sandboxes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIcon {
    pub url: Option<String>,
    pub r#override: Option<String>,
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCommands {
    pub start: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectTime {
    pub created: i64,
    pub updated: i64,
    pub active: i64,
    pub archived: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vcs {
    Git { store: String },
    Hg { store: String },
}

impl Vcs {
    pub fn kind(&self) -> &'static str {
        match self {
            Vcs::Git { .. } => "git",
            Vcs::Hg { .. } => "hg",
        }
    }

    pub fn store(&self) -> &str {
        match self {
            Vcs::Git { store } | Vcs::Hg { store } => store,
        }
    }
}

impl Project {
    pub fn from_row(row: ProjectRow) -> Result<Project, StorageError> {
        let commands = row
            .commands
            .map(|value| ProjectCommands { start: value.get("start").and_then(Value::as_str).map(str::to_string) });
        let icon = (row.icon_url.is_some() || row.icon_url_override.is_some() || row.icon_color.is_some())
            .then_some(ProjectIcon { url: row.icon_url, r#override: row.icon_url_override, color: row.icon_color });
        Ok(Project {
            id: row.id,
            canonical: row.worktree,
            vcs: row.vcs,
            name: row.name,
            icon,
            commands,
            time: ProjectTime {
                created: row.time_created,
                updated: row.time_updated,
                active: row.time_active,
                archived: row.time_archived,
            },
            sandboxes: row.sandboxes,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub previous: Option<String>,
    pub id: String,
    pub directory: String,
    pub canonical: String,
    pub vcs: Option<Vcs>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveOptions {
    pub persist: bool,
}

impl ResolveOptions {
    pub fn persist() -> Self {
        ResolveOptions { persist: true }
    }
}

impl Default for ResolveOptions {
    fn default() -> Self {
        ResolveOptions::persist()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateInput {
    pub project_id: String,
    pub canonical: Option<String>,
    pub name: Option<String>,
    pub icon: Option<IconInput>,
    pub commands: Option<CommandsInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IconInput {
    pub r#override: Option<String>,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandsInput {
    pub start: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("Project not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[derive(Clone)]
struct GitCacheEntry {
    resolved: Option<Resolved>,
    cacheable: bool,
}

pub struct ProjectService {
    database: Arc<opencode_storage::Database>,
    sender: broadcast::Sender<LocationEvent>,
    cache: Mutex<HashMap<String, GitCacheEntry>>,
    announced: Mutex<HashSet<(String, String)>>,
    activated: Mutex<HashMap<String, i64>>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl ProjectService {
    pub fn new(database: Arc<opencode_storage::Database>) -> Self {
        Self::with_now(database, Arc::new(opencode_storage::id::now_ms))
    }

    pub fn with_now(database: Arc<opencode_storage::Database>, now: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let (sender, _) = broadcast::channel(256);
        ProjectService {
            database,
            sender,
            cache: Mutex::new(HashMap::new()),
            announced: Mutex::new(HashSet::new()),
            activated: Mutex::new(HashMap::new()),
            now,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LocationEvent> {
        self.sender.subscribe()
    }

    pub async fn resolve(
        &self,
        environment: &dyn Environment,
        directory: &str,
        options: ResolveOptions,
    ) -> Result<Resolved, ProjectError> {
        let directory = resolve_directory(directory).await;
        let marker = find_marker(environment.files(), &directory, None).await;
        let marker_name = marker.as_ref().and_then(|marker| marker.file_name()).and_then(|name| name.to_str());
        let discovered = match marker_name {
            Some(".git") => {
                let dot_git = marker.as_ref().map_or_else(String::new, |marker| marker.to_string_lossy().into_owned());
                self.resolve_git(environment, &dot_git).await
            }
            Some(".hg") => {
                let dot_hg = marker.as_ref().map_or_else(String::new, |marker| marker.to_string_lossy().into_owned());
                Some(discover_hg(environment, &dot_hg).await)
            }
            _ => None,
        };
        let resolved = match discovered {
            Some(resolved) => resolved,
            None => {
                let directory = directory.to_string_lossy().into_owned();
                Resolved {
                    previous: None,
                    id: sha1_hex(format!("directory:{directory}")),
                    canonical: directory.clone(),
                    directory,
                    vcs: None,
                }
            }
        };
        if options.persist {
            self.persist(environment, &resolved).await?;
        }
        Ok(resolved)
    }

    pub async fn list(&self, archived: bool) -> Result<Vec<Project>, ProjectError> {
        let rows = self.database.read(|connection| project::list(connection, archived))?;
        rows.into_iter().map(Project::from_row).collect::<Result<Vec<_>, _>>().map_err(ProjectError::from)
    }

    pub async fn update(&self, input: UpdateInput) -> Result<Project, ProjectError> {
        let commands = input
            .commands
            .as_ref()
            .and_then(|commands| commands.start.as_deref())
            .map(|start| (!start.is_empty()).then(|| serde_json::json!({ "start": start })));
        let update = ProjectMetadataUpdate {
            canonical: input.canonical.as_deref(),
            name: input.name.as_deref().map(|name| (!name.is_empty()).then_some(name)),
            icon_url_override: input
                .icon
                .as_ref()
                .and_then(|icon| icon.r#override.as_deref().map(|value| (!value.is_empty()).then_some(value))),
            icon_color: input
                .icon
                .as_ref()
                .and_then(|icon| icon.color.as_deref().map(|value| (!value.is_empty()).then_some(value))),
            commands: commands.as_ref().map(|value| value.as_ref()),
        };
        let row = self.database.read(|connection| project::update_metadata(connection, &input.project_id, &update))?;
        let Some(row) = row else {
            return Err(ProjectError::NotFound(input.project_id));
        };
        let project = Project::from_row(row)?;
        self.publish(LocationEvent::ProjectUpdated(project.clone()));
        Ok(project)
    }

    pub async fn activate(&self, id: &str) -> Result<(), ProjectError> {
        let timestamp = (self.now)();
        {
            let mut activated = self.activated.lock().unwrap_or_else(|error| error.into_inner());
            if activated.get(id).is_some_and(|last| timestamp - *last < ACTIVATE_INTERVAL_MS) {
                return Ok(());
            }
            activated.insert(id.to_string(), timestamp);
        }
        let row = self.database.read(|connection| project::touch_active(connection, id, timestamp))?;
        if let Some(row) = row {
            self.publish(LocationEvent::ProjectUpdated(Project::from_row(row)?));
        }
        Ok(())
    }

    pub async fn sweep(&self, environment: &dyn Environment) -> Result<(), ProjectError> {
        let cutoff = (self.now)() - SWEEP_INTERVAL_MS;
        let projects = self.database.read(|connection| project::list(connection, true))?;
        for project in projects {
            if project.id == GLOBAL_ID
                || project.time_archived.is_some()
                || project.time_updated >= cutoff
                || project.time_active >= cutoff
            {
                continue;
            }
            if matches!(exists(environment.files(), &project.worktree).await, Ok(true) | Err(_)) {
                continue;
            }
            let row =
                self.database.read(|connection| project::set_archived(connection, &project.id, Some((self.now)())))?;
            if let Some(row) = row {
                self.publish(LocationEvent::ProjectUpdated(Project::from_row(row)?));
            }
        }
        Ok(())
    }

    async fn resolve_git(&self, environment: &dyn Environment, dot_git: &str) -> Option<Resolved> {
        let Some(fingerprint) = git::fingerprint(environment.files(), dot_git).await else {
            return discover_git(environment, dot_git).await.resolved;
        };
        let key = format!("{dot_git}\0{fingerprint}");
        let cached = {
            let cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
            cache.get(&key).cloned()
        };
        if let Some(entry) = cached {
            if !entry.cacheable {
                self.cache.lock().unwrap_or_else(|error| error.into_inner()).remove(&key);
            }
            return entry.resolved;
        }
        let entry = discover_git(environment, dot_git).await;
        if entry.cacheable {
            let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
            if cache.len() >= GIT_CACHE_CAPACITY
                && let Some(evicted) = cache.keys().next().cloned()
            {
                cache.remove(&evicted);
            }
            cache.insert(key, entry.clone());
        }
        entry.resolved
    }

    async fn persist(&self, environment: &dyn Environment, project: &Resolved) -> Result<(), ProjectError> {
        let previous = self.database.read(|connection| project::get(connection, &project.id))?;
        let replace = match &previous {
            Some(row) => {
                row.worktree != project.canonical
                    && match exists(environment.files(), &row.worktree).await {
                        Ok(found) => !found,
                        Err(_) => false,
                    }
            }
            None => false,
        };
        let vcs = project.vcs.as_ref().map(Vcs::kind);
        let row = self.database.transaction(TransactionBehavior::Immediate, |transaction| {
            project::upsert(transaction, &project.id, &project.canonical, vcs)?;
            let mut changed = false;
            if previous.as_ref().is_some_and(|row| row.time_archived.is_some()) {
                project::set_archived(transaction, &project.id, None)?;
                changed = true;
            }
            if replace {
                project::set_canonical(transaction, &project.id, &project.canonical)?;
                changed = true;
            }
            if !changed {
                return Ok(None);
            }
            project::get(transaction, &project.id)
        })?;
        if let Some(row) = row {
            self.publish(LocationEvent::ProjectUpdated(Project::from_row(row)?));
        }
        match icon::refresh(&self.database, &project.id).await {
            Ok(Some(row)) => self.publish(LocationEvent::ProjectUpdated(Project::from_row(row)?)),
            Ok(None) => {}
            Err(error) => log::warn!("project icon refresh failed for {}: {error}", project.id),
        }
        if project.vcs.is_some() {
            let mut directories = vec![project.canonical.as_str()];
            if project.directory != project.canonical {
                directories.push(project.directory.as_str());
            }
            for directory in directories {
                let key = (project.id.clone(), directory.to_string());
                {
                    let mut announced = self.announced.lock().unwrap_or_else(|error| error.into_inner());
                    if !announced.insert(key) {
                        continue;
                    }
                }
                let adopted = self.adopted(environment, &project.id, directory).await?;
                self.publish(LocationEvent::WorktreeResolved {
                    project_id: project.id.clone(),
                    directory: directory.to_string(),
                    previous: project.previous.clone().unwrap_or_else(|| GLOBAL_ID.to_string()),
                    adopted,
                });
            }
        }
        Ok(())
    }

    async fn adopted(
        &self,
        environment: &dyn Environment,
        project_id: &str,
        directory: &str,
    ) -> Result<Vec<String>, ProjectError> {
        let rows = self.database.read(|connection| project::list(connection, true))?;
        let directory = Path::new(directory);
        let mut adopted = Vec::new();
        for row in rows {
            if row.vcs.is_some() || row.id == project_id {
                continue;
            }
            let worktree = Path::new(&row.worktree);
            if !worktree.starts_with(directory) {
                continue;
            }
            let Some(marker) = find_marker(environment.files(), worktree, Some(directory)).await else {
                continue;
            };
            if marker.parent() == Some(directory) {
                adopted.push(row.id);
            }
        }
        Ok(adopted)
    }

    fn publish(&self, event: LocationEvent) {
        let _ = self.sender.send(event);
    }
}

pub fn normalize_remote(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = Url::parse(value) {
        if parsed.scheme() == "file" {
            return None;
        }
        return remote_parts(parsed.host_str().unwrap_or_default(), parsed.path());
    }
    let (host, path) = scp_remote(value)?;
    remote_parts(host, path)
}

pub fn remote_id(input: &str) -> Option<String> {
    normalize_remote(input).map(|normalized| sha1_hex(format!("git-remote:{normalized}")))
}

pub fn sha1_hex(bytes: impl AsRef<[u8]>) -> String {
    digest(&SHA1_FOR_LEGACY_USE_ONLY, bytes.as_ref()).as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn scp_remote(value: &str) -> Option<(&str, &str)> {
    let (prefix, path) = value.split_once(':')?;
    if path.is_empty() || path.contains(['\n', '\r']) || prefix.contains('/') {
        return None;
    }
    let host = match prefix.split_once('@') {
        Some((user, host)) if !user.is_empty() => host,
        _ => prefix,
    };
    Some((host, path))
}

fn remote_parts(host: &str, name: &str) -> Option<String> {
    let path = name.trim_start_matches('/');
    let path = if let Some(path) = path.strip_suffix(".git") {
        path
    } else if let Some(path) = path.strip_suffix(".git/") {
        path
    } else {
        path
    };
    let path = path.trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_lowercase(), path))
}

async fn resolve_directory(directory: &str) -> PathBuf {
    if let Ok(path) = tokio::fs::canonicalize(directory).await {
        return path;
    }
    match std::path::absolute(directory) {
        Ok(path) => normalize(&path),
        Err(_) => normalize(Path::new(directory)),
    }
}

async fn find_marker(files: &dyn Files, start: &Path, stop: Option<&Path>) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(node) = current {
        for name in [".git", ".hg"] {
            let candidate = node.join(name);
            if matches!(exists(files, &candidate.to_string_lossy()).await, Ok(true)) {
                return Some(candidate);
            }
        }
        if stop == Some(node) {
            break;
        }
        current = node.parent();
    }
    None
}

async fn discover_git(environment: &dyn Environment, dot_git: &str) -> GitCacheEntry {
    let Some(parent) = Path::new(dot_git).parent() else {
        return GitCacheEntry { resolved: None, cacheable: false };
    };
    let repository = match git::discover(environment.spawner(), &parent.to_string_lossy()).await {
        Ok(Some(repository)) => repository,
        Ok(None) | Err(_) => return GitCacheEntry { resolved: None, cacheable: false },
    };
    let mut cacheable = true;
    let previous = cached_id(environment.files(), &repository.common_directory).await;
    let remote = match git::remote_get(environment.spawner(), &repository).await {
        Ok(remote) => remote.and_then(|remote| remote_id(&remote)),
        Err(_) => {
            cacheable = false;
            None
        }
    };
    let id = if let Some(remote) = remote {
        remote
    } else if let Some(previous) = &previous {
        previous.clone()
    } else {
        match git::root_commits(environment.spawner(), &repository).await {
            Ok(commits) => commits.first().cloned().unwrap_or_else(|| GLOBAL_ID.to_string()),
            Err(_) => {
                cacheable = false;
                GLOBAL_ID.to_string()
            }
        }
    };
    let canonical = if repository.git_directory == repository.common_directory {
        repository.worktree.clone()
    } else {
        match git::worktree_list(environment.spawner(), &repository).await {
            Ok(entries) => entries
                .into_iter()
                .find(|entry| entry.kind == git::WorktreeKind::Main)
                .map_or_else(|| repository.worktree.clone(), |entry| entry.directory),
            Err(_) => {
                cacheable = false;
                repository.worktree.clone()
            }
        }
    };
    GitCacheEntry {
        resolved: Some(Resolved {
            previous,
            id,
            directory: repository.worktree.clone(),
            canonical,
            vcs: Some(Vcs::Git { store: repository.common_directory.clone() }),
        }),
        cacheable,
    }
}

async fn discover_hg(environment: &dyn Environment, dot_hg: &str) -> Resolved {
    let worktree = parent_path(dot_hg);
    let previous = cached_id(environment.files(), dot_hg).await;
    let id = match &previous {
        Some(previous) => previous.clone(),
        None => hg_root(environment.spawner(), &worktree).await.unwrap_or_else(|| GLOBAL_ID.to_string()),
    };
    Resolved {
        previous,
        id,
        directory: worktree.clone(),
        canonical: worktree,
        vcs: Some(Vcs::Hg { store: dot_hg.to_string() }),
    }
}

async fn cached_id(files: &dyn Files, directory: &str) -> Option<String> {
    let path = Path::new(directory).join("opencode").to_string_lossy().into_owned();
    let read = files.read(&path, None).await.ok()?;
    let value = String::from_utf8(read.bytes).ok()?;
    let value = value.trim();
    if value.is_empty() || value.chars().any(|character| character <= '\u{1f}' || character == '\u{7f}') {
        return None;
    }
    Some(value.to_string())
}

async fn hg_root(spawner: &dyn Spawner, worktree: &str) -> Option<String> {
    let command = CommandSpec {
        program: "hg".to_string(),
        args: ["log", "-r", "roots(all())", "-T", "{node}\n"].iter().map(|arg| (*arg).to_string()).collect(),
        cwd: Some(worktree.to_string()),
        env: vec![("HGPLAIN".to_string(), "1".to_string())],
    };
    let output = spawner.spawn(&command).await.ok()?;
    if output.status != 0 {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut nodes: Vec<&str> = stdout.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    nodes.sort_unstable();
    nodes.first().map(|node| (*node).to_string())
}

fn parent_path(path: &str) -> String {
    Path::new(path).parent().map_or_else(String::new, |parent| parent.to_string_lossy().into_owned())
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
