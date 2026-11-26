//! Port of `packages/core/src/filesystem/watcher.ts` on `notify`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher as NotifyWatcher};
use opencode_util::path::Flavor;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateKind {
    Create,
    Update,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub path: PathBuf,
    pub kind: UpdateKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchInput {
    File { path: PathBuf },
    Entries { path: PathBuf, names: Vec<String> },
    Directory { path: PathBuf, ignore: Vec<String> },
}

/// Starts watches and hands their updates to every subscriber of an equivalent input. All watches
/// of one `Watcher` share a single notify instance.
pub struct Watcher {
    enabled: bool,
    registry: Arc<Mutex<Registry>>,
    routes: Arc<Mutex<HashMap<Key, Route>>>,
}

/// The seam `FileWatch` uses so tests can drive subscription lifecycles without the filesystem.
pub trait WatchSource: Send + Sync {
    fn subscribe(&self, input: WatchInput) -> Subscription;
}

impl WatchSource for Watcher {
    fn subscribe(&self, input: WatchInput) -> Subscription {
        Watcher::subscribe(self, input)
    }
}

impl Watcher {
    pub fn new(enabled: bool) -> Watcher {
        Watcher {
            enabled,
            registry: Arc::new(Mutex::new(Registry::default())),
            routes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Subscribes to `input`, sharing one underlying watch with equivalent subscribers. An input
    /// that cannot be watched, or a disabled watcher, yields an already-ended subscription.
    pub fn subscribe(&self, input: WatchInput) -> Subscription {
        if !self.enabled {
            return Subscription::closed();
        }
        let (key, matcher) = prepare(input);
        let mut registry = lock(&self.registry);
        if let Some(entry) = registry.entries.get_mut(&key) {
            let Some(route) = lock(&self.routes).get(&key).map(|route| route.sender.subscribe()) else {
                return Subscription::closed();
            };
            entry.users += 1;
            return Subscription { receiver: route, release: Some(self.release(key)) };
        }
        let (target, mode) = matcher.target();
        let recursive = mode == RecursiveMode::Recursive;
        let watched = registry.targets.get(&target).map(|watch| watch.recursive);
        if watched.is_none() || (recursive && watched == Some(false)) {
            let Some(notify) = registry.notify(&self.routes) else {
                return Subscription::closed();
            };
            if let Err(error) = notify.watch(&target, mode) {
                if !matches!(error.kind, notify::ErrorKind::PathNotFound) {
                    log::error!("failed to watch {}: {error}", target.display());
                }
                return Subscription::closed();
            }
        }
        let watch = registry.targets.entry(target.clone()).or_insert(TargetWatch { users: 0, recursive });
        watch.users += 1;
        watch.recursive |= recursive;
        let (sender, receiver) = broadcast::channel(CHANNEL_CAPACITY);
        lock(&self.routes).insert(key.clone(), Route { matcher, sender });
        registry.entries.insert(key.clone(), Entry { target, users: 1 });
        Subscription { receiver, release: Some(self.release(key)) }
    }

    /// The number of underlying watches currently held, for diagnostics and tests.
    pub fn active(&self) -> usize {
        lock(&self.registry).entries.len()
    }

    fn release(&self, key: Key) -> Release {
        Release { registry: self.registry.clone(), routes: self.routes.clone(), key }
    }
}

pub struct Subscription {
    receiver: broadcast::Receiver<Update>,
    release: Option<Release>,
}

impl Subscription {
    /// An already-ended subscription.
    pub fn closed() -> Subscription {
        let (sender, receiver) = broadcast::channel(1);
        drop(sender);
        Subscription { receiver, release: None }
    }

    #[doc(hidden)]
    pub fn from_receiver(receiver: broadcast::Receiver<Update>) -> Subscription {
        Subscription { receiver, release: None }
    }

    pub async fn recv(&mut self) -> Result<Update, broadcast::error::RecvError> {
        self.receiver.recv().await
    }

    /// Blocks the current thread until an update arrives. Must not be called from inside an async
    /// runtime; the registry bridge threads call it to avoid owning a runtime.
    pub fn blocking_recv(&mut self) -> Result<Update, broadcast::error::RecvError> {
        self.receiver.blocking_recv()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let mut registry = lock(&self.registry);
        registry.entries.clear();
        registry.targets.clear();
        registry.notify = None;
        lock(&self.routes).clear();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let Some(release) = self.release.take() else { return };
        let mut registry = lock(&release.registry);
        let Some(entry) = registry.entries.get_mut(&release.key) else { return };
        entry.users = entry.users.saturating_sub(1);
        if entry.users > 0 {
            return;
        }
        let Some(entry) = registry.entries.remove(&release.key) else { return };
        lock(&release.routes).remove(&release.key);
        let remaining = registry.targets.get_mut(&entry.target).map(|watch| {
            watch.users = watch.users.saturating_sub(1);
            watch.users
        });
        if remaining == Some(0) {
            registry.targets.remove(&entry.target);
            if let Some(notify) = registry.notify.as_mut() {
                let _ = notify.unwatch(&entry.target);
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    notify: Option<notify::RecommendedWatcher>,
    targets: HashMap<PathBuf, TargetWatch>,
    entries: HashMap<Key, Entry>,
}

impl Registry {
    fn notify(&mut self, routes: &Arc<Mutex<HashMap<Key, Route>>>) -> Option<&mut notify::RecommendedWatcher> {
        if self.notify.is_none() {
            let routes = Arc::downgrade(routes);
            let created = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                let Some(routes) = routes.upgrade() else { return };
                for route in lock(&routes).values() {
                    let Some(kind) = update_kind(&event.kind, &route.matcher) else { continue };
                    for path in &event.paths {
                        if route.matcher.matches(path) {
                            let _ = route.sender.send(Update { path: path.clone(), kind });
                        }
                    }
                }
            });
            match created {
                Ok(watcher) => self.notify = Some(watcher),
                Err(error) => {
                    log::error!("failed to create a file watcher: {error}");
                    return None;
                }
            }
        }
        self.notify.as_mut()
    }
}

struct Entry {
    target: PathBuf,
    users: usize,
}

struct TargetWatch {
    users: usize,
    recursive: bool,
}

struct Route {
    matcher: Matcher,
    sender: broadcast::Sender<Update>,
}

struct Release {
    registry: Arc<Mutex<Registry>>,
    routes: Arc<Mutex<HashMap<Key, Route>>>,
    key: Key,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    File(PathBuf),
    Entries(PathBuf, Vec<String>),
    Directory(PathBuf, Vec<String>),
}

#[derive(Clone)]
enum Matcher {
    File { target: PathBuf },
    Entries { directory: PathBuf, names: BTreeSet<String> },
    Directory { target: PathBuf, ignore: Vec<Ignore> },
}

#[derive(Clone)]
enum Ignore {
    Literal(PathBuf),
    Glob(String),
}

impl Matcher {
    fn target(&self) -> (PathBuf, RecursiveMode) {
        match self {
            Matcher::File { target } => {
                let directory = target.parent().map(Path::to_path_buf).unwrap_or_else(|| target.clone());
                (directory, RecursiveMode::NonRecursive)
            }
            Matcher::Entries { directory, .. } => (directory.clone(), RecursiveMode::NonRecursive),
            Matcher::Directory { target, .. } => (target.clone(), RecursiveMode::Recursive),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        match self {
            Matcher::File { target } => path == target,
            Matcher::Entries { directory, names } => {
                path.parent().is_some_and(|parent| parent == directory)
                    && path.file_name().is_some_and(|name| names.contains(&*name.to_string_lossy()))
            }
            Matcher::Directory { target, ignore } => {
                crate::path::contains(&target.to_string_lossy(), &path.to_string_lossy())
                    && !ignore.iter().any(|entry| entry.matches(target, path))
            }
        }
    }
}

impl Ignore {
    fn matches(&self, target: &Path, path: &Path) -> bool {
        match self {
            Ignore::Literal(literal) => crate::path::contains(&literal.to_string_lossy(), &path.to_string_lossy()),
            Ignore::Glob(pattern) => {
                let flavor = Flavor::NATIVE;
                let candidate = if flavor.is_absolute(pattern) {
                    path.to_string_lossy().into_owned()
                } else {
                    flavor.relative(&target.to_string_lossy(), &path.to_string_lossy())
                };
                glob_matches(pattern, &candidate)
            }
        }
    }
}

fn prepare(input: WatchInput) -> (Key, Matcher) {
    match input {
        WatchInput::File { path } => {
            let target = resolve(&path);
            (Key::File(target.clone()), Matcher::File { target })
        }
        WatchInput::Entries { path, names } => {
            let directory = resolve(&path);
            let names = sorted_unique(names);
            (
                Key::Entries(directory.clone(), names.clone()),
                Matcher::Entries { directory, names: names.into_iter().collect() },
            )
        }
        WatchInput::Directory { path, ignore } => {
            let target = resolve(&path);
            let ignore = sorted_unique(ignore);
            let matcher = ignore.iter().map(|entry| ignore_entry(&target, entry)).collect();
            (Key::Directory(target.clone(), ignore), Matcher::Directory { target, ignore: matcher })
        }
    }
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values.into_iter().collect::<BTreeSet<_>>().into_iter().collect()
}

fn ignore_entry(target: &Path, entry: &str) -> Ignore {
    if entry.contains(['*', '?', '[', '{']) {
        return Ignore::Glob(entry.to_owned());
    }
    Ignore::Literal(PathBuf::from(Flavor::NATIVE.resolve(&[&target.to_string_lossy(), entry])))
}

fn resolve(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().map(|cwd| cwd.to_string_lossy().into_owned()).unwrap_or_default();
    PathBuf::from(Flavor::NATIVE.resolve(&[&cwd, &path.to_string_lossy()]))
}

fn update_kind(kind: &EventKind, matcher: &Matcher) -> Option<UpdateKind> {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => Some(UpdateKind::Update),
        EventKind::Access(_) => None,
        _ if matches!(matcher, Matcher::File { .. } | Matcher::Entries { .. }) => Some(UpdateKind::Update),
        EventKind::Create(_) => Some(UpdateKind::Create),
        EventKind::Remove(_) => Some(UpdateKind::Delete),
        _ => Some(UpdateKind::Update),
    }
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    let pattern = segments(pattern);
    let candidate = segments(candidate);
    match_segments(&pattern, &candidate)
}

fn segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|segment| !segment.is_empty()).collect()
}

fn match_segments(pattern: &[&str], candidate: &[&str]) -> bool {
    match pattern.split_first() {
        None => candidate.is_empty(),
        Some((segment, rest)) if *segment == "**" => {
            (0..=candidate.len()).any(|index| match_segments(rest, &candidate[index..]))
        }
        Some((segment, rest)) => match candidate.split_first() {
            Some((first, candidate_rest)) => segment_matches(segment, first) && match_segments(rest, candidate_rest),
            None => false,
        },
    }
}

fn segment_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut backtrack = None;
    let mut backtrack_text = 0;
    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            backtrack = Some(pattern_index);
            backtrack_text = text_index;
            pattern_index += 1;
        } else if let Some(star) = backtrack {
            pattern_index = star + 1;
            backtrack_text += 1;
            text_index = backtrack_text;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
