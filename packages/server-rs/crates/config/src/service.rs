use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use opencode_contract::generated::ConfigEntryDocumentInfo;
use opencode_util::path::{Flavor, realpath};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::discovery::{Sources, discover};
use crate::entry::{Document, Entry, Origin};
use crate::flag::{self, FlagState};
use crate::load::{FlagInputs, LoadDeps, load};
use crate::options::{ConfigLocation, ConfigOptions};
use crate::paths::GlobalPaths;
use crate::seams::{CredentialLookup, NoKv};
use crate::watch::{self, InputSet, WatchTarget, WatchUpdate, Watcher};
use crate::wellknown::WellKnown;

const DEBOUNCE: Duration = Duration::from_millis(100);
const REFRESH: Duration = Duration::from_secs(10 * 60);
const UPDATES_CAPACITY: usize = 256;
const EVENTS_CAPACITY: usize = 16;

pub struct ServiceDeps {
    pub credentials: Arc<dyn CredentialLookup>,
    pub wellknown: Arc<WellKnown>,
    pub channel: String,
    pub flag_overrides: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compatibility {
    pub claude: Vec<PathBuf>,
    pub agents: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntegrationDocument {
    pub connection: Value,
    pub organization: Option<String>,
    pub info: ConfigEntryDocumentInfo,
}

struct State {
    entries: Vec<Entry>,
    sources: Sources,
    inputs: InputSet,
    targets: BTreeMap<String, WatchTarget>,
    integrations: BTreeMap<String, Document>,
}

struct Inner {
    options: ConfigOptions,
    location: ConfigLocation,
    paths: GlobalPaths,
    deps: ServiceDeps,
    state: RwLock<State>,
    reload_lock: Mutex<()>,
    updates: broadcast::Sender<WatchUpdate>,
    updated: broadcast::Sender<()>,
    requests: mpsc::UnboundedSender<()>,
    watcher: Watcher,
}

#[derive(Clone)]
pub struct ConfigService {
    inner: Arc<Inner>,
}

impl ConfigService {
    pub async fn start(
        options: ConfigOptions,
        location: ConfigLocation,
        paths: GlobalPaths,
        deps: ServiceDeps,
    ) -> Self {
        let (updates, _) = broadcast::channel(UPDATES_CAPACITY);
        let (updated, _) = broadcast::channel(EVENTS_CAPACITY);
        let (requests, request_receiver) = mpsc::unbounded_channel();
        let watching = !opencode_local::watch::disabled(|name| std::env::var(name).ok());
        let inner = Arc::new(Inner {
            options,
            location,
            paths,
            deps,
            state: RwLock::new(State {
                entries: Vec::new(),
                sources: Sources::default(),
                inputs: InputSet::default(),
                targets: BTreeMap::new(),
                integrations: BTreeMap::new(),
            }),
            reload_lock: Mutex::new(()),
            updates: updates.clone(),
            updated,
            requests,
            watcher: Watcher::new(updates, watching),
        });
        let service = Self { inner };

        let sources = discover(&service.inner.options, &service.inner.location, &service.inner.paths);
        let loaded = service.load(&sources).await;
        service.store(loaded);

        service.spawn_updates();
        service.spawn_reloads(request_receiver);
        service.spawn_wellknown();
        if service.reconcile() > 0 {
            service.request_reload();
        }
        service
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.inner.state.read().expect("the config state lock is never poisoned").entries.clone()
    }

    pub fn compatibility(&self) -> Compatibility {
        let state = self.inner.state.read().expect("the config state lock is never poisoned");
        Compatibility {
            claude: state.sources.claude.iter().filter(|path| path.is_dir()).cloned().collect(),
            agents: state.sources.agents.iter().filter(|path| path.is_dir()).cloned().collect(),
        }
    }

    pub fn changes(&self) -> broadcast::Receiver<WatchUpdate> {
        self.inner.updates.subscribe()
    }

    pub fn subscribe_updated(&self) -> broadcast::Receiver<()> {
        self.inner.updated.subscribe()
    }

    pub async fn integration(&self, integration_id: &str, document: Option<IntegrationDocument>) {
        let next = document.map(|document| Document {
            path: None,
            info: document.info,
            origin: Origin::Integration {
                integration_id: integration_id.to_owned(),
                connection: document.connection,
                organization: document.organization,
            },
        });
        {
            let mut state = self.inner.state.write().expect("the config state lock is never poisoned");
            if state.integrations.get(integration_id) == next.as_ref() {
                return;
            }
            match next {
                Some(document) => state.integrations.insert(integration_id.to_owned(), document),
                None => state.integrations.remove(integration_id),
            };
        }
        self.reload().await;
    }

    pub async fn credential_switched(&self, integration_id: &str) {
        let entries = self.inner.deps.wellknown.entries().await;
        if entries.iter().any(|entry| entry.integration_id == integration_id) {
            self.reload().await;
        }
    }

    pub fn flags(&self) -> Vec<(&'static str, FlagState)> {
        let state = self.inner.state.read().expect("the config state lock is never poisoned");
        let infos: Vec<&ConfigEntryDocumentInfo> = state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Document(document) => Some(&document.info),
                Entry::Directory { .. } => None,
            })
            .collect();
        flag::resolve(&infos, &self.inner.deps.channel, self.inner.deps.flag_overrides.as_deref())
    }

    pub async fn reload(&self) {
        let _guard = self.inner.reload_lock.lock().await;
        let sources = discover(&self.inner.options, &self.inner.location, &self.inner.paths);
        let loaded = self.load(&sources).await;
        let changed = {
            let state = self.inner.state.read().expect("the config state lock is never poisoned");
            loaded.entries != state.entries
                || loaded.sources.claude != state.sources.claude
                || loaded.sources.agents != state.sources.agents
        };
        self.store(loaded);
        let started = self.reconcile();
        if started > 0 {
            self.request_reload();
        }
        if changed {
            let _ = self.inner.updated.send(());
        }
    }

    async fn load(&self, sources: &Sources) -> crate::load::Loaded {
        let integrations: Vec<Document> = {
            let state = self.inner.state.read().expect("the config state lock is never poisoned");
            state.integrations.values().cloned().collect()
        };
        let deps = LoadDeps {
            credentials: &*self.inner.deps.credentials,
            wellknown: &*self.inner.deps.wellknown,
            kv: &NoKv,
            integrations: &integrations,
            home: &self.inner.paths.home,
        };
        let flags =
            FlagInputs { channel: self.inner.deps.channel.clone(), overrides: self.inner.deps.flag_overrides.clone() };
        load(sources, &self.inner.options, &self.inner.location, &deps, flags).await
    }

    fn store(&self, loaded: crate::load::Loaded) {
        let targets = watch::plan(&loaded.sources);
        let inputs = watch::inputs(&loaded.sources, &loaded.references);
        let mut state = self.inner.state.write().expect("the config state lock is never poisoned");
        state.entries = loaded.entries;
        state.sources = loaded.sources;
        state.inputs = inputs;
        state.targets = targets;
    }

    fn reconcile(&self) -> usize {
        let targets = self.inner.state.read().expect("the config state lock is never poisoned").targets.clone();
        self.inner.watcher.reconcile(&targets)
    }

    fn request_reload(&self) {
        let _ = self.inner.requests.send(());
    }

    fn spawn_updates(&self) {
        let service = self.clone();
        let mut updates = service.inner.updates.subscribe();
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(update) => {
                        if service.needs_reload(&update.path) {
                            service.request_reload();
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    fn spawn_reloads(&self, mut requests: mpsc::UnboundedReceiver<()>) {
        let service = self.clone();
        tokio::spawn(async move {
            while requests.recv().await.is_some() {
                loop {
                    match tokio::time::timeout(DEBOUNCE, requests.recv()).await {
                        Ok(Some(())) => continue,
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                service.reload().await;
            }
        });
    }

    fn spawn_wellknown(&self) {
        let service = self.clone();
        let mut updates = self.inner.deps.wellknown.subscribe();
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(()) => service.reload().await,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        let service = self.clone();
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + REFRESH;
            let mut interval = tokio::time::interval_at(start, REFRESH);
            loop {
                interval.tick().await;
                if service.inner.deps.wellknown.snapshot().is_empty() {
                    continue;
                }
                if !service.inner.deps.wellknown.refresh().await {
                    service.reload().await;
                }
            }
        });
    }

    fn needs_reload(&self, path: &std::path::Path) -> bool {
        let state = self.inner.state.read().expect("the config state lock is never poisoned");
        if state.inputs.matches(path) {
            return true;
        }
        for target in state.targets.values() {
            let root = target.path();
            let real = realpath(root).unwrap_or_else(|| root.to_path_buf());
            if real == root || !watch::contains(&real, path) {
                continue;
            }
            let relative = Flavor::NATIVE.relative(&real.to_string_lossy(), &path.to_string_lossy());
            if state.inputs.matches(&root.join(relative)) {
                return true;
            }
        }
        false
    }
}
