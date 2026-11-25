use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures_util::StreamExt;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_config::entry::Entry;
use opencode_config::observer::ObserverHandle;
use opencode_config::service::ConfigService;
use opencode_contract::generated::{ConfigEntryDocumentInfoReferencesValue, LocationRef};
use opencode_location::state::{self, Options, Registration, State};
use opencode_util::path::Flavor;
use serde_json::{Map, Value};
use tokio::task::{JoinHandle, JoinSet};

use crate::repository;
use crate::repository_cache::{EnsureInput, Materializer, Refresh};
use crate::task_set;

pub type Vars = repository::Vars;

const EVENT_UPDATED: &str = "reference.updated";
const STATE_NAME: &str = "reference";
const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const REFRESH_CONCURRENCY: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSource {
    pub path: String,
    pub description: Option<String>,
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSource {
    pub repository: String,
    pub branch: Option<String>,
    pub description: Option<String>,
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Local(LocalSource),
    Git(GitSource),
}

impl Source {
    pub fn description(&self) -> Option<&str> {
        match self {
            Source::Local(source) => source.description.as_deref(),
            Source::Git(source) => source.description.as_deref(),
        }
    }

    pub fn hidden(&self) -> Option<bool> {
        match self {
            Source::Local(source) => source.hidden,
            Source::Git(source) => source.hidden,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
    pub hidden: Option<bool>,
    pub source: Source,
}

#[derive(Clone, Debug)]
struct Data {
    sources: Vec<(String, Source)>,
}

pub struct Editor<'a> {
    sources: &'a mut Vec<(String, Source)>,
}

impl Editor<'_> {
    pub fn add(&mut self, name: impl Into<String>, source: Source) {
        let name = name.into();
        match self.sources.iter_mut().find(|(existing, _)| existing == &name) {
            Some(entry) => entry.1 = source,
            None => self.sources.push((name, source)),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.sources.retain(|(existing, _)| existing != name);
    }

    pub fn list(&self) -> Vec<(String, Source)> {
        self.sources.clone()
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|(existing, _)| existing == name).map(|(_, source)| source)
    }
}

pub struct Reference {
    state: State<Data>,
    cache: Arc<dyn Materializer>,
    repos: PathBuf,
    vars: Vars,
    task: Mutex<Option<JoinHandle<()>>>,
    tasks: Arc<Mutex<JoinSet<()>>>,
    registration: Mutex<Option<Registration>>,
    observer: Mutex<Option<ObserverHandle>>,
}

impl Reference {
    pub fn new(
        bus: Arc<Bus>,
        location: LocationRef,
        cache: Arc<dyn Materializer>,
        repos: PathBuf,
        vars: Vars,
    ) -> Arc<Self> {
        let tasks = Arc::new(Mutex::new(JoinSet::new()));
        let notify = {
            let bus = bus.clone();
            let location = location.clone();
            let cache = cache.clone();
            let repos = repos.clone();
            let vars = vars.clone();
            let tasks = tasks.clone();
            state::changes(
                |data: &Data| format!("{:?}", data.sources),
                move |data: Arc<Data>| {
                    let bus = bus.clone();
                    let location = location.clone();
                    let cache = cache.clone();
                    let repos = repos.clone();
                    let vars = vars.clone();
                    let tasks = tasks.clone();
                    Box::pin(async move {
                        let infos = infos(&data.sources, &repos, &vars);
                        task_set::spawn(&tasks, refresh(infos, cache, vars));
                        let options = PublishOptions { location: Some(location), ..Default::default() };
                        let data = Value::Object(Map::new());
                        if let Err(error) = bus.publish(&Definition::ephemeral(EVENT_UPDATED), data, options) {
                            log::warn!("failed to publish a reference update: {error}");
                        }
                    })
                },
            )
        };
        let state = State::new(Options {
            name: Some(STATE_NAME.to_owned()),
            initial: Box::new(|| Data { sources: Vec::new() }),
            notify: Some(notify),
        });
        let reference = Arc::new(Reference {
            state,
            cache,
            repos,
            vars,
            task: Mutex::new(None),
            tasks,
            registration: Mutex::new(None),
            observer: Mutex::new(None),
        });
        let task = {
            let state = reference.state.clone();
            let cache = reference.cache.clone();
            let repos = reference.repos.clone();
            let vars = reference.vars.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                    let current = match state.get().await {
                        Ok(data) => data,
                        Err(error) => {
                            log::warn!("failed to read reference state: {error}");
                            continue;
                        }
                    };
                    let infos = infos(&current.sources, &repos, &vars);
                    refresh(infos, cache.clone(), vars.clone()).await;
                }
            })
        };
        *reference.task.lock().unwrap_or_else(|error| error.into_inner()) = Some(task);
        reference
    }

    pub async fn transform(
        &self,
        edit: impl for<'a, 'b> Fn(&'b mut Editor<'a>) + Send + Sync + 'static,
    ) -> Registration {
        self.state
            .transform(move |data| {
                let mut editor = Editor { sources: &mut data.sources };
                edit(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn list(&self) -> Vec<Info> {
        match self.state.get().await {
            Ok(data) => infos(&data.sources, &self.repos, &self.vars),
            Err(error) => {
                log::warn!("failed to read reference state: {error}");
                Vec::new()
            }
        }
    }

    pub async fn observe_config(&self, config: &ConfigService, directory: &str, home: &str) {
        let entries = Arc::new(RwLock::new(config.entries()));
        let directory = directory.to_owned();
        let home = home.to_owned();
        let slot = entries.clone();
        let registration = self
            .state
            .transform(move |data| {
                let loaded = slot.read().unwrap_or_else(|error| error.into_inner()).clone();
                let mut editor = Editor { sources: &mut data.sources };
                for (name, source) in sources_from_entries(&loaded, &directory, &home) {
                    editor.add(name, source);
                }
                Ok(())
            })
            .await;
        *self.registration.lock().unwrap_or_else(|error| error.into_inner()) = Some(registration);
        let slot = entries.clone();
        let state = self.state.clone();
        let tasks = self.tasks.clone();
        let observer = opencode_config::observer::observe(config, move |entries| {
            *slot.write().unwrap_or_else(|error| error.into_inner()) = entries.to_vec();
            let state = state.clone();
            task_set::spawn(&tasks, async move { state.reload().await });
        });
        *self.observer.lock().unwrap_or_else(|error| error.into_inner()) = Some(observer);
    }

    pub fn close(&self) {
        if let Some(task) = self.task.lock().unwrap_or_else(|error| error.into_inner()).take() {
            task.abort();
        }
        task_set::abort_all(&self.tasks);
    }
}

impl Drop for Reference {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap_or_else(|error| error.into_inner()).take() {
            task.abort();
        }
        task_set::abort_all(&self.tasks);
    }
}

/// Pure mapping of config documents to reference sources, in insertion order.
pub fn sources_from_entries(entries: &[Entry], directory: &str, home: &str) -> Vec<(String, Source)> {
    let mut sources: Vec<(String, Source)> = Vec::new();
    for entry in entries {
        let Entry::Document(document) = entry else {
            continue;
        };
        let Some(references) = &document.info.references else {
            continue;
        };
        let document_directory = document
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|parent| !parent.is_empty())
            .unwrap_or_else(|| directory.to_owned());
        for (name, value) in &references.0 {
            if !valid_alias(name) {
                continue;
            }
            let (description, hidden) = match value {
                ConfigEntryDocumentInfoReferencesValue::String(_) => (None, None),
                ConfigEntryDocumentInfoReferencesValue::Object(object) => (object.description.clone(), object.hidden),
                ConfigEntryDocumentInfoReferencesValue::Object2(object) => (object.description.clone(), object.hidden),
            };
            let source = match value {
                ConfigEntryDocumentInfoReferencesValue::String(value) if is_local(value) => {
                    Source::Local(LocalSource {
                        path: local_path(&document_directory, home, value),
                        description,
                        hidden,
                    })
                }
                ConfigEntryDocumentInfoReferencesValue::String(value) => {
                    Source::Git(GitSource { repository: value.clone(), branch: None, description, hidden })
                }
                ConfigEntryDocumentInfoReferencesValue::Object2(object) => Source::Local(LocalSource {
                    path: local_path(&document_directory, home, &object.path),
                    description,
                    hidden,
                }),
                ConfigEntryDocumentInfoReferencesValue::Object(object) => Source::Git(GitSource {
                    repository: object.repository.clone(),
                    branch: object.branch.clone(),
                    description,
                    hidden,
                }),
            };
            match sources.iter_mut().find(|(existing, _)| existing == name) {
                Some(existing) => existing.1 = source,
                None => sources.push((name.clone(), source)),
            }
        }
    }
    sources
}

fn infos(sources: &[(String, Source)], repos: &Path, vars: &Vars) -> Vec<Info> {
    let repos = repos.to_string_lossy();
    sources
        .iter()
        .filter_map(|(name, source)| {
            let path = match source {
                Source::Local(local) => local.path.clone(),
                Source::Git(git) => {
                    let parsed = repository::parse(&git.repository, vars)?;
                    if !repository::is_remote(&parsed) {
                        return None;
                    }
                    if let Some(branch) = &git.branch
                        && repository::validate_branch(branch).is_err()
                    {
                        return None;
                    }
                    repository::cache_path(&repos, &parsed, git.branch.as_deref())
                }
            };
            Some(Info {
                name: name.clone(),
                path,
                description: source.description().map(str::to_owned),
                hidden: source.hidden(),
                source: source.clone(),
            })
        })
        .collect()
}

async fn refresh(infos: Vec<Info>, cache: Arc<dyn Materializer>, vars: Vars) {
    let git = infos.into_iter().filter_map(|info| {
        let name = info.name;
        match info.source {
            Source::Git(source) => Some((name, source)),
            Source::Local(_) => None,
        }
    });
    futures_util::stream::iter(git)
        .map(|(name, source)| {
            let cache = cache.clone();
            let vars = vars.clone();
            async move {
                let Ok(reference) = repository::parse_remote(&source.repository, &vars) else {
                    log::warn!("failed to materialize reference {name}: invalid repository");
                    return;
                };
                let input = EnsureInput { reference, branch: source.branch, refresh: Some(Refresh::Daily) };
                if let Err(error) = cache.ensure(input).await {
                    log::warn!("failed to materialize reference {name}: {error}");
                }
            }
        })
        .buffer_unordered(REFRESH_CONCURRENCY)
        .collect::<Vec<()>>()
        .await;
}

fn valid_alias(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| matches!(c, '/' | '`' | ',') || c.is_whitespace())
}

fn is_local(value: &str) -> bool {
    value.starts_with('.') || value.starts_with('/') || value.starts_with('~')
}

fn local_path(directory: &str, home: &str, value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        return Flavor::NATIVE.join(&[home, rest]);
    }
    if Flavor::NATIVE.is_absolute(value) {
        return value.to_owned();
    }
    Flavor::NATIVE.resolve(&[directory, value])
}
