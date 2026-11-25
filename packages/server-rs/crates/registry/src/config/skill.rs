use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use indexmap::IndexMap;
use opencode_config::entry::Entry;
use opencode_config::service::ConfigService;
use opencode_contract::generated::SkillInfo;
use opencode_location::state::Registration;
use opencode_util::path::Flavor;
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::task::JoinHandle;

use crate::skill::{SkillEditor, SkillService};
use crate::watch::WatchHandle;

#[derive(Clone, Debug)]
pub struct SkillEnvironment {
    pub directory: PathBuf,
    pub home: PathBuf,
    pub cache: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Directory(PathBuf),
    Url(String),
}

pub fn sources(entries: &[Entry], environment: &SkillEnvironment) -> Vec<Source> {
    let mut seen_paths = HashSet::new();
    let mut seen_urls = HashSet::new();
    let mut sources = Vec::new();
    for entry in entries {
        let Entry::Directory { path } = entry else { continue };
        let directory = path.to_string_lossy();
        add_directory(&mut seen_paths, &mut sources, Flavor::NATIVE.join(&[directory.as_ref(), "skill"]));
        add_directory(&mut seen_paths, &mut sources, Flavor::NATIVE.join(&[directory.as_ref(), "skills"]));
    }
    for entry in entries {
        let Entry::Document(document) = entry else { continue };
        let Some(items) = document.info.skills.as_deref() else { continue };
        for item in items {
            if is_url(item) {
                add_url(&mut seen_urls, &mut sources, item.clone());
                continue;
            }
            let expanded = match item.strip_prefix("~/") {
                Some(rest) => Flavor::NATIVE.join(&[environment.home.to_string_lossy().as_ref(), rest]),
                None => item.clone(),
            };
            let path = if Flavor::NATIVE.is_absolute(&expanded) {
                expanded
            } else {
                Flavor::NATIVE.join(&[environment.directory.to_string_lossy().as_ref(), expanded.as_str()])
            };
            add_directory(&mut seen_paths, &mut sources, path);
        }
    }
    sources
}

pub async fn load(entries: &[Entry], environment: &SkillEnvironment) -> Vec<SkillInfo> {
    let mut skills = IndexMap::new();
    for source in sources(entries, environment) {
        let directories = match source {
            Source::Directory(path) => vec![path],
            Source::Url(url) => {
                let pulled = crate::skill::discovery::pull(&url, &environment.cache).await;
                if pulled.is_empty() {
                    log::debug!("skill source {url} yielded no directories");
                }
                pulled
            }
        };
        for directory in directories {
            insert(&mut skills, &directory);
        }
    }
    skills.into_values().collect()
}

pub fn apply(entries: &[Entry], environment: &SkillEnvironment, editor: &mut SkillEditor<'_>) {
    for skill in load_directories(entries, environment) {
        editor.add(skill, None);
    }
}

fn load_directories(entries: &[Entry], environment: &SkillEnvironment) -> Vec<SkillInfo> {
    let mut skills = IndexMap::new();
    for source in sources(entries, environment) {
        let Source::Directory(directory) = source else { continue };
        insert(&mut skills, &directory);
    }
    skills.into_values().collect()
}

fn insert(skills: &mut IndexMap<String, SkillInfo>, directory: &Path) {
    for file in crate::skill::scan::scan(directory) {
        let Ok(content) = std::fs::read_to_string(&file) else { continue };
        if content.is_empty() {
            continue;
        }
        let crate::skill::file::ParseResult::Parsed(skill) = crate::skill::file::parse(directory, &file, &content)
        else {
            continue;
        };
        skills.insert(skill.id.0.clone(), skill);
    }
}

pub struct ConfigSkillHandle {
    _registration: Registration,
    task: JoinHandle<()>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
}

impl Drop for ConfigSkillHandle {
    fn drop(&mut self) {
        self.task.abort();
        if let Ok(mut watch) = self.watch.lock() {
            watch.take();
        }
    }
}

pub async fn register(
    config: &ConfigService,
    skills: &SkillService,
    environment: SkillEnvironment,
) -> ConfigSkillHandle {
    let loaded = Arc::new(RwLock::new(load(&config.entries(), &environment).await));
    let registration = skills
        .transform({
            let loaded = loaded.clone();
            move |editor| {
                let loaded = loaded.read().expect("the config skill lock is never poisoned");
                for skill in loaded.iter() {
                    editor.add(skill.clone(), None);
                }
            }
        })
        .await;
    let watch = Arc::new(Mutex::new(None));
    let refresher = Arc::new(Refresher {
        config: config.clone(),
        skills: skills.clone(),
        environment,
        loaded,
        lock: AsyncMutex::new(()),
        watch: watch.clone(),
        handle: tokio::runtime::Handle::current(),
    });
    refresher.install_watches();
    let mut updates = config.subscribe_updated();
    let task = tokio::spawn({
        let refresher = refresher.clone();
        async move {
            loop {
                match updates.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => refresher.refresh().await,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    ConfigSkillHandle { _registration: registration, task, watch }
}

struct Refresher {
    config: ConfigService,
    skills: SkillService,
    environment: SkillEnvironment,
    loaded: Arc<RwLock<Vec<SkillInfo>>>,
    lock: AsyncMutex<()>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
    handle: tokio::runtime::Handle,
}

impl Refresher {
    async fn refresh(self: &Arc<Self>) {
        let _guard = self.lock.lock().await;
        let entries = self.config.entries();
        let loaded = load(&entries, &self.environment).await;
        *self.loaded.write().expect("the config skill lock is never poisoned") = loaded;
        self.skills.reload().await;
        self.install_watches();
    }

    fn install_watches(self: &Arc<Self>) {
        let entries = self.config.entries();
        let paths = watch_candidates(&entries, &self.environment);
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
        *self.watch.lock().expect("the config skill watch lock is never poisoned") = Some(watch);
    }
}

fn watch_candidates(entries: &[Entry], environment: &SkillEnvironment) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for source in sources(entries, environment) {
        let Source::Directory(directory) = source else { continue };
        paths.extend(crate::skill::scan::scan(&directory));
        paths.push(directory);
    }
    paths
}

fn add_directory(seen: &mut HashSet<PathBuf>, sources: &mut Vec<Source>, path: String) {
    let path = PathBuf::from(path);
    if seen.insert(path.clone()) {
        sources.push(Source::Directory(path));
    }
}

fn add_url(seen: &mut HashSet<String>, sources: &mut Vec<Source>, url: String) {
    if seen.insert(url.clone()) {
        sources.push(Source::Url(url));
    }
}

fn is_url(source: &str) -> bool {
    url::Url::parse(source).ok().is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}
