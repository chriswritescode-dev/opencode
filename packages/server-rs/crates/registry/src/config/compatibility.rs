use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use indexmap::IndexMap;
use opencode_config::service::{Compatibility, ConfigService};
use opencode_config::watch;
use opencode_contract::generated::SkillInfo;
use opencode_location::state::Registration;
use opencode_util::path::{Flavor, realpath};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::task::JoinHandle;

use crate::skill::{SkillEditor, SkillService};
use crate::watch::WatchHandle;

const SOURCE_DIRECTORIES: [&str; 2] = [".claude", ".agents"];

pub fn load(compatibility: &Compatibility) -> Vec<SkillInfo> {
    let mut skills = IndexMap::new();
    for root in compatibility.claude.iter().chain(compatibility.agents.iter()) {
        let directory = skill_directory(root);
        let Some(resolved) = realpath(&directory) else { continue };
        for file in crate::skill::scan::scan(&resolved) {
            let Ok(content) = std::fs::read_to_string(&file) else { continue };
            if content.is_empty() {
                continue;
            }
            let crate::skill::file::ParseResult::Parsed(skill) = crate::skill::file::parse(&resolved, &file, &content)
            else {
                continue;
            };
            skills.insert(skill.id.0.clone(), skill);
        }
    }
    skills.into_values().collect()
}

pub fn apply(compatibility: &Compatibility, editor: &mut SkillEditor<'_>) {
    for skill in load(compatibility) {
        editor.add(skill, None);
    }
}

pub fn is_source(compatibility: &Compatibility, file: &Path) -> bool {
    let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if SOURCE_DIRECTORIES.contains(&name.as_str()) {
        return true;
    }
    compatibility.claude.iter().chain(compatibility.agents.iter()).any(|root| watch::contains(root, file))
}

pub struct ConfigCompatibilityHandle {
    _registration: Registration,
    task: JoinHandle<()>,
    changes_task: JoinHandle<()>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
}

impl Drop for ConfigCompatibilityHandle {
    fn drop(&mut self) {
        self.task.abort();
        self.changes_task.abort();
        if let Ok(mut watch) = self.watch.lock() {
            watch.take();
        }
    }
}

pub async fn register(config: &ConfigService, skills: &SkillService) -> ConfigCompatibilityHandle {
    let initial = config.compatibility();
    let compatibility = Arc::new(RwLock::new(initial.clone()));
    let loaded = Arc::new(RwLock::new(load(&initial)));
    let registration = skills
        .transform({
            let loaded = loaded.clone();
            move |editor| {
                let loaded = loaded.read().expect("the config compatibility lock is never poisoned");
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
        compatibility: compatibility.clone(),
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
    let changes_task = tokio::spawn({
        let refresher = refresher.clone();
        async move {
            let mut changes = refresher.config.changes();
            loop {
                match changes.recv().await {
                    Ok(update) => {
                        let compatibility = refresher
                            .compatibility
                            .read()
                            .expect("the config compatibility lock is never poisoned")
                            .clone();
                        if is_source(&compatibility, &update.path) {
                            refresher.refresh().await;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => refresher.refresh().await,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    ConfigCompatibilityHandle { _registration: registration, task, changes_task, watch }
}

struct Refresher {
    config: ConfigService,
    skills: SkillService,
    compatibility: Arc<RwLock<Compatibility>>,
    loaded: Arc<RwLock<Vec<SkillInfo>>>,
    lock: AsyncMutex<()>,
    watch: Arc<Mutex<Option<WatchHandle>>>,
    handle: tokio::runtime::Handle,
}

impl Refresher {
    async fn refresh(self: &Arc<Self>) {
        let _guard = self.lock.lock().await;
        let compatibility = self.config.compatibility();
        let loaded = load(&compatibility);
        *self.compatibility.write().expect("the config compatibility lock is never poisoned") = compatibility;
        *self.loaded.write().expect("the config compatibility lock is never poisoned") = loaded;
        self.skills.reload().await;
        self.install_watches();
    }

    fn install_watches(self: &Arc<Self>) {
        let compatibility = self.compatibility.read().expect("the config compatibility lock is never poisoned").clone();
        let paths = watch_candidates(&compatibility);
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
        *self.watch.lock().expect("the config compatibility watch lock is never poisoned") = Some(watch);
    }
}

fn watch_candidates(compatibility: &Compatibility) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in compatibility.claude.iter().chain(compatibility.agents.iter()) {
        let directory = skill_directory(root);
        let resolved = realpath(&directory).unwrap_or(directory);
        paths.extend(crate::skill::scan::scan(&resolved));
        paths.push(resolved);
    }
    paths
}

fn skill_directory(root: &Path) -> PathBuf {
    PathBuf::from(Flavor::NATIVE.join(&[root.to_string_lossy().as_ref(), "skills"]))
}
