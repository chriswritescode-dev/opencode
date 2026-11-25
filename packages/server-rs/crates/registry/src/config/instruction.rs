use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use opencode_config::entry::Entry;
use opencode_config::service::ConfigService;
use opencode_location::state::Registration;
use opencode_util::path::realpath;
use tokio::sync::{Semaphore, broadcast};
use tokio::task::JoinSet;

use crate::instructions::discovery::{File, InstructionDiscovery, InstructionOptions, filenames, prefer_agents};
use crate::scan::{self, ScanOptions};
use crate::watch::WatchHandle;

#[derive(Clone, Debug)]
pub struct InstructionEnvironment {
    pub directory: PathBuf,
    pub project_directory: PathBuf,
    pub home: PathBuf,
    pub config_directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Discovered {
    Available(Vec<File>),
    Unavailable,
}

pub fn configured_sources(entries: &[Entry]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for entry in entries {
        let Entry::Document(document) = entry else { continue };
        let Some(instructions) = &document.info.instructions else { continue };
        for source in instructions {
            if seen.insert(source.clone()) {
                sources.push(source.clone());
            }
        }
    }
    sources
}

pub fn is_url(source: &str) -> bool {
    url::Url::parse(source).ok().is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

pub fn discover(
    environment: &InstructionEnvironment,
    options: &InstructionOptions,
    instructions: &[String],
    remote: &HashMap<String, File>,
) -> Discovered {
    let start = resolve(&environment.directory);
    let root = resolve(&environment.project_directory);
    let home = resolve(&environment.home);
    let project = options.project && contains(&root, &start);
    let stop = if contains(&home, &start) { home.clone() } else { root.clone() };
    let names = filenames(options.claude);
    let global_files = global_candidates(environment, options, &home);

    let global = if options.global { global_source(&global_files) } else { Ok(Vec::new()) };
    let walked = if project { project_source(&names, options, &start, &stop, &global_files) } else { Ok(Vec::new()) };
    let configured = configured_source(environment, options, instructions, remote, &start, &root, project);

    let (Ok(mut global), Ok(walked), Ok(configured)) = (global, walked, configured) else {
        return Discovered::Unavailable;
    };
    global.extend(walked);
    global.extend(configured);
    Discovered::Available(global)
}

pub async fn fetch_remote(previous: &HashMap<String, File>, sources: &[String]) -> HashMap<String, File> {
    let mut remote = HashMap::new();
    let client = opencode_config::http::client();
    let semaphore = Arc::new(Semaphore::new(4));
    let mut tasks = JoinSet::new();
    for url in sources.iter().filter(|source| is_url(source)) {
        let Ok(permit) = semaphore.clone().acquire_owned().await else { continue };
        let client = client.clone();
        let url = url.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let content = match client.get(&url).timeout(Duration::from_secs(5)).send().await {
                Ok(response) if response.status().is_success() => response.text().await.ok(),
                Ok(_) => None,
                Err(error) => {
                    log::warn!("failed to fetch instruction url {url}: {error}");
                    None
                }
            };
            (url, content)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let Ok((url, content)) = joined else { continue };
        match content {
            Some(content) => {
                remote.insert(url.clone(), File { path: url, content });
            }
            None => {
                if let Some(file) = previous.get(&url) {
                    remote.insert(url, file.clone());
                }
            }
        }
    }
    remote
}

pub struct ConfigInstructionHandle {
    task: tokio::task::JoinHandle<()>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
}

impl Drop for ConfigInstructionHandle {
    fn drop(&mut self) {
        self.task.abort();
        if let Ok(mut watch) = self.watch.lock() {
            watch.take();
        }
    }
}

pub async fn register(
    config: &ConfigService,
    discovery: &InstructionDiscovery,
    environment: InstructionEnvironment,
) -> ConfigInstructionHandle {
    let mut updates = config.subscribe_updated();
    let options = discovery.options().clone();
    let sources = configured_sources(&config.entries());
    let remote = fetch_remote(&HashMap::new(), &sources).await;
    let watch = Arc::new(Mutex::new(None));
    let refresher = Arc::new(Refresher {
        discovery: discovery.clone(),
        environment,
        options,
        sources: Arc::new(RwLock::new(sources)),
        remote: Arc::new(RwLock::new(remote)),
        loaded: Arc::new(RwLock::new(None)),
        watch: watch.clone(),
        handle: tokio::runtime::Handle::current(),
        registration: Mutex::new(None),
    });
    refresher.activate().await;
    let task = tokio::spawn({
        let refresher = refresher.clone();
        let config = config.clone();
        async move {
            loop {
                match updates.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => refresher.reconfigure(&config).await,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    ConfigInstructionHandle { task, watch }
}

struct Refresher {
    discovery: InstructionDiscovery,
    environment: InstructionEnvironment,
    options: InstructionOptions,
    sources: Arc<RwLock<Vec<String>>>,
    remote: Arc<RwLock<HashMap<String, File>>>,
    loaded: Arc<RwLock<Option<Vec<File>>>>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
    handle: tokio::runtime::Handle,
    registration: Mutex<Option<Registration>>,
}

impl Refresher {
    async fn activate(self: &Arc<Self>) {
        self.reload_loaded();
        let loaded = self.loaded.clone();
        let registration = self
            .discovery
            .transform(move |editor| {
                let loaded = loaded.read().expect("the instruction state lock is never poisoned");
                match &*loaded {
                    None => editor.unavailable(),
                    Some(files) => {
                        for file in files {
                            editor.add(file.clone());
                        }
                    }
                }
            })
            .await;
        *self.registration.lock().expect("the instruction registration lock is never poisoned") = Some(registration);
        self.install_watches();
    }

    async fn reconfigure(self: &Arc<Self>, config: &ConfigService) {
        let sources = configured_sources(&config.entries());
        let previous = self.remote.read().expect("the instruction remote lock is never poisoned").clone();
        let remote = fetch_remote(&previous, &sources).await;
        *self.sources.write().expect("the instruction source lock is never poisoned") = sources;
        *self.remote.write().expect("the instruction remote lock is never poisoned") = remote;
        self.refresh().await;
    }

    async fn refresh(self: &Arc<Self>) {
        self.reload_loaded();
        self.discovery.reload().await;
        self.install_watches();
    }

    fn reload_loaded(&self) {
        let sources = self.sources.read().expect("the instruction source lock is never poisoned").clone();
        let remote = self.remote.read().expect("the instruction remote lock is never poisoned").clone();
        let loaded = match discover(&self.environment, &self.options, &sources, &remote) {
            Discovered::Available(files) => Some(files),
            Discovered::Unavailable => None,
        };
        *self.loaded.write().expect("the instruction state lock is never poisoned") = loaded;
    }

    fn install_watches(self: &Arc<Self>) {
        let paths = self.watch_candidates();
        let handle = self.handle.clone();
        let refresher = Arc::downgrade(self);
        let watch = crate::watch::watch_files(
            paths,
            |name| std::env::var(name).ok(),
            move || {
                let Some(refresher) = refresher.upgrade() else { return };
                handle.spawn(async move { refresher.refresh().await });
            },
        );
        *self.watch.lock().expect("the instruction watch lock is never poisoned") = Some(watch);
    }

    fn watch_candidates(&self) -> Vec<PathBuf> {
        let start = resolve(&self.environment.directory);
        let root = resolve(&self.environment.project_directory);
        let home = resolve(&self.environment.home);
        let global = global_candidates(&self.environment, &self.options, &home);
        let project = self.options.project && contains(&root, &start);
        let stop = if contains(&home, &start) { home } else { root.clone() };
        let mut candidates = Vec::new();
        if self.options.global {
            candidates.extend(global.iter().cloned());
        }
        if project {
            let names = filenames(self.options.claude);
            for directory in ancestor_directories(&start, &stop) {
                for name in &names {
                    let file = directory.join(name);
                    if self.options.global || !global.contains(&file) {
                        candidates.push(file);
                    }
                }
            }
        }
        let sources = self.sources.read().expect("the instruction source lock is never poisoned").clone();
        for source in &sources {
            if is_url(source) {
                continue;
            }
            for path in match_source(&self.environment, &self.options, source, &start, &root, project) {
                candidates.push(resolve(&path));
            }
        }
        dedupe(candidates)
    }
}

fn global_source(files: &[PathBuf]) -> Result<Vec<File>, ()> {
    for candidate in files {
        if !candidate.is_file() {
            continue;
        }
        return match read_file(candidate) {
            ReadResult::File(file) => Ok(vec![file]),
            ReadResult::Missing | ReadResult::Denied => Ok(Vec::new()),
            ReadResult::Failed => Err(()),
        };
    }
    Ok(Vec::new())
}

fn project_source(
    names: &[&str],
    options: &InstructionOptions,
    start: &Path,
    stop: &Path,
    global_files: &[PathBuf],
) -> Result<Vec<File>, ()> {
    let found = scan::up(names, start, Some(stop));
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for file in prefer_agents(&found) {
        let resolved = resolve(&file);
        if !options.global && global_files.contains(&resolved) {
            continue;
        }
        if !seen.insert(resolved.clone()) {
            continue;
        }
        match read_file(&resolved) {
            ReadResult::File(file) => files.push(file),
            ReadResult::Denied => {}
            ReadResult::Missing | ReadResult::Failed => return Err(()),
        }
    }
    Ok(files)
}

fn configured_source(
    environment: &InstructionEnvironment,
    options: &InstructionOptions,
    instructions: &[String],
    remote: &HashMap<String, File>,
    start: &Path,
    root: &Path,
    project: bool,
) -> Result<Vec<File>, ()> {
    let mut files = Vec::new();
    let mut watched = HashSet::new();
    for source in instructions {
        if is_url(source) {
            if let Some(file) = remote.get(source) {
                files.push(file.clone());
            }
            continue;
        }
        let mut matches = match_source(environment, options, source, start, root, project);
        matches.sort();
        for item in matches {
            let resolved = resolve(&item);
            if !watched.insert(resolved.clone()) {
                continue;
            }
            match read_file(&resolved) {
                ReadResult::File(file) => files.push(file),
                ReadResult::Missing | ReadResult::Denied => {}
                ReadResult::Failed => return Err(()),
            }
        }
    }
    Ok(files)
}

fn match_source(
    environment: &InstructionEnvironment,
    options: &InstructionOptions,
    source: &str,
    start: &Path,
    root: &Path,
    project: bool,
) -> Vec<PathBuf> {
    let expanded = match source.strip_prefix("~/") {
        Some(rest) => environment.home.join(rest),
        None => PathBuf::from(source),
    };
    let scan_options = ScanOptions { dot: true, symlink: false, include_files_only: true, ignore: &[] };
    let mut results = if expanded.is_absolute() {
        match expanded.parent() {
            Some(directory) if directory.is_dir() => {
                let name = expanded.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                scan::scan(directory, &name, scan_options)
            }
            _ => absolute_glob(&expanded),
        }
    } else if project {
        scan::glob_up(source, start, Some(root))
    } else if !options.global {
        Vec::new()
    } else {
        scan::scan(&environment.config_directory, source, scan_options)
    };
    results.sort();
    results.dedup();
    results
}

fn absolute_glob(pattern: &Path) -> Vec<PathBuf> {
    let mut base = pattern.to_path_buf();
    while !base.is_dir() {
        if !base.pop() {
            return Vec::new();
        }
    }
    let Ok(relative) = pattern.strip_prefix(&base) else { return Vec::new() };
    let relative = relative.to_string_lossy().replace('\\', "/");
    if relative.is_empty() {
        return Vec::new();
    }
    scan::scan(&base, &relative, ScanOptions { dot: true, symlink: false, include_files_only: true, ignore: &[] })
}

fn global_candidates(environment: &InstructionEnvironment, options: &InstructionOptions, home: &Path) -> Vec<PathBuf> {
    let agents = resolve(&environment.config_directory.join("AGENTS.md"));
    if options.claude { vec![agents, resolve(&home.join(".claude").join("CLAUDE.md"))] } else { vec![agents] }
}

fn resolve(path: &Path) -> PathBuf {
    realpath(path).unwrap_or_else(|| path.to_path_buf())
}

enum ReadResult {
    File(File),
    Missing,
    Denied,
    Failed,
}

fn read_file(path: &Path) -> ReadResult {
    match std::fs::read_to_string(path) {
        Ok(content) => ReadResult::File(File { path: path.to_string_lossy().into_owned(), content }),
        Err(error) if error.kind() == ErrorKind::NotFound => ReadResult::Missing,
        Err(error) if error.kind() == ErrorKind::PermissionDenied => ReadResult::Denied,
        Err(error) => {
            log::warn!("failed to read instruction file {}: {error}", path.display());
            ReadResult::Failed
        }
    }
}

#[cfg(windows)]
fn contains(parent: &Path, child: &Path) -> bool {
    let parent = normalized(parent);
    let child = normalized(child);
    child == parent || child.starts_with(&parent)
}

#[cfg(not(windows))]
fn contains(parent: &Path, child: &Path) -> bool {
    child == parent || child.starts_with(parent)
}

#[cfg(windows)]
fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/").to_lowercase())
}

fn same_directory(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn ancestor_directories(start: &Path, stop: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut current = start.to_path_buf();
    loop {
        directories.push(current.clone());
        if same_directory(&current, stop) {
            break;
        }
        let Some(parent) = current.parent() else { break };
        if parent == current {
            break;
        }
        let parent = parent.to_path_buf();
        if parent.as_os_str().is_empty() {
            break;
        }
        current = parent;
    }
    directories
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|path| seen.insert(path.clone())).collect()
}
