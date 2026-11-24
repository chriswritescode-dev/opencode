use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use notify::{RecursiveMode, Watcher as NotifyWatcher};
use opencode_util::path::{Flavor, realpath};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::discovery::{NAMES, Sources};

pub const IGNORED_DIRECTORIES: [&str; 5] = ["node_modules", ".git", ".venv", "venv", "__pycache__"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchUpdate {
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WatchTarget {
    Directory { path: PathBuf, ignore: Vec<String> },
    Entries { path: PathBuf, names: Vec<String> },
}

impl WatchTarget {
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory { path, .. } | Self::Entries { path, .. } => path,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Directory { path, ignore } => {
                format!("directory:{}:{}", path.to_string_lossy(), ignore.join("\u{1f}"))
            }
            Self::Entries { path, names } => format!("entries:{}:{}", path.to_string_lossy(), names.join("\u{1f}")),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputSet {
    keys: HashSet<String>,
}

impl InputSet {
    pub fn matches(&self, file: &Path) -> bool {
        self.keys.contains(&key(file))
    }
}

pub fn inputs(sources: &Sources, references: &BTreeSet<PathBuf>) -> InputSet {
    let mut roots: Vec<&Path> = Vec::new();
    if let Some(system) = &sources.system {
        roots.push(&system.path);
    }
    if let Some(global) = &sources.global {
        roots.push(global);
    }
    roots.extend(sources.project.iter().map(|root| root.path.as_path()));

    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        files.push(root.to_path_buf());
        for name in NAMES {
            files.push(joined(root, name));
        }
    }
    files.extend(sources.direct.iter().cloned());
    files.extend(sources.claude.iter().cloned());
    files.extend(sources.agents.iter().cloned());
    files.extend(sources.explicit.iter().cloned());
    files.extend(references.iter().cloned());
    for reference in references {
        if let Some(real) = realpath(reference) {
            files.push(real);
        }
    }

    InputSet { keys: files.iter().map(|file| key(file)).collect() }
}

pub fn plan(sources: &Sources) -> BTreeMap<String, WatchTarget> {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(system) = &sources.system
        && system.present
    {
        directories.push(system.path.clone());
    }
    if let Some(global) = &sources.global {
        directories.push(global.clone());
    }
    directories.extend(sources.project.iter().filter(|root| root.present).map(|root| root.path.clone()));

    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(system) = &sources.system {
        files.push(system.path.clone());
    }
    files.extend(sources.direct.iter().cloned());
    files.extend(sources.project.iter().map(|root| root.path.clone()));
    files.extend(sources.claude.iter().cloned());
    files.extend(sources.agents.iter().cloned());
    files.extend(sources.explicit.iter().cloned());

    let mut parents: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for file in &files {
        if directories.iter().any(|directory| directory != file && contains(directory, file)) {
            continue;
        }
        let Some(parent) = file.parent() else { continue };
        parents.entry(parent.to_path_buf()).or_default().insert(basename(file));
    }

    let mut targets = BTreeMap::new();
    for path in directories {
        let target = WatchTarget::Directory { path, ignore: ignore_globs() };
        targets.insert(target.key(), target);
    }
    for (path, names) in parents {
        let target = WatchTarget::Entries { path, names: names.into_iter().collect() };
        targets.insert(target.key(), target);
    }
    targets
}

pub fn contains(parent: &Path, child: &Path) -> bool {
    let parent = parent.to_string_lossy();
    let child = child.to_string_lossy();
    let relative = Flavor::NATIVE.relative(&parent, &child);
    relative.is_empty() || !Flavor::NATIVE.outside(&relative)
}

pub struct Watcher {
    enabled: bool,
    updates: broadcast::Sender<WatchUpdate>,
    watchers: Mutex<BTreeMap<String, notify::RecommendedWatcher>>,
}

impl Watcher {
    pub fn new(updates: broadcast::Sender<WatchUpdate>, enabled: bool) -> Self {
        Self { enabled, updates, watchers: Mutex::new(BTreeMap::new()) }
    }

    /// Starts the watches in `targets`, stops the ones no longer planned, and returns how many started.
    pub fn reconcile(&self, targets: &BTreeMap<String, WatchTarget>) -> usize {
        if !self.enabled {
            return 0;
        }
        let mut watchers = self.watchers.lock().expect("the config watcher map is never poisoned");
        watchers.retain(|key, _| targets.contains_key(key));
        let mut started = 0;
        for (key, target) in targets {
            if watchers.contains_key(key) {
                continue;
            }
            let Some(watcher) = self.start(target) else { continue };
            watchers.insert(key.clone(), watcher);
            started += 1;
        }
        started
    }

    fn start(&self, target: &WatchTarget) -> Option<notify::RecommendedWatcher> {
        let updates = self.updates.clone();
        let watch_target = target.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if is_read_only_access(event.kind) {
                return;
            }
            for path in event.paths {
                if !matches(&watch_target, &path) {
                    continue;
                }
                let _ = updates.send(WatchUpdate { path });
            }
        })
        .inspect_err(|error| log::warn!("failed to create the config watcher: {error}"))
        .ok()?;
        let mode = match target {
            WatchTarget::Directory { .. } => RecursiveMode::Recursive,
            WatchTarget::Entries { .. } => RecursiveMode::NonRecursive,
        };
        watcher
            .watch(target.path(), mode)
            .inspect_err(|error| log::warn!("failed to watch {}: {error}", target.path().display()))
            .ok()?;
        Some(watcher)
    }
}

fn is_read_only_access(kind: notify::EventKind) -> bool {
    match kind {
        notify::EventKind::Access(notify::event::AccessKind::Close(notify::event::AccessMode::Write)) => false,
        notify::EventKind::Access(_) => true,
        _ => false,
    }
}

fn matches(target: &WatchTarget, path: &Path) -> bool {
    match target {
        WatchTarget::Directory { path: root, .. } => {
            let relative = Flavor::NATIVE.relative(&root.to_string_lossy(), &path.to_string_lossy());
            !relative.split(Flavor::NATIVE.sep()).any(|part| IGNORED_DIRECTORIES.contains(&part))
        }
        WatchTarget::Entries { names, .. } => {
            names.iter().any(|name| path.file_name().is_some_and(|file| file == name.as_str()))
        }
    }
}

fn key(file: &Path) -> String {
    Flavor::NATIVE.normalize(&file.to_string_lossy()).to_lowercase()
}

fn ignore_globs() -> Vec<String> {
    let mut globs: Vec<String> = IGNORED_DIRECTORIES.iter().map(|name| (*name).to_owned()).collect();
    globs.push(format!("**/{{{}}}/**", IGNORED_DIRECTORIES.join(",")));
    globs
}

fn joined(parent: &Path, name: &str) -> PathBuf {
    PathBuf::from(Flavor::NATIVE.join(&[&parent.to_string_lossy(), name]))
}

fn basename(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}
