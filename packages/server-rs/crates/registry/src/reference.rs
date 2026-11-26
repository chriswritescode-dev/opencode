pub mod instructions;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use indexmap::IndexMap;
use opencode_contract::codec::Encode;
use opencode_contract::generated::{
    AbsolutePath, ReferenceGitSource, ReferenceGitSourceType, ReferenceInfo, ReferenceLocalSource,
    ReferenceLocalSourceType, ReferenceSource,
};
use opencode_location::state::{Options, Registration, State, StateError, changes};
use opencode_vcs::repository::{self, Vars};
use opencode_vcs::repository_cache::{EnsureInput, Materializer, Refresh};
use tokio::task::{JoinHandle, JoinSet};

use crate::events::RegistryEvents;

pub const EVENT: &str = "reference.updated";

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

#[derive(Default)]
pub struct ReferenceData {
    pub sources: IndexMap<String, Source>,
}

pub struct ReferenceEditor<'a> {
    data: &'a mut ReferenceData,
}

impl<'a> ReferenceEditor<'a> {
    pub fn new(data: &'a mut ReferenceData) -> Self {
        ReferenceEditor { data }
    }

    pub fn add(&mut self, name: String, source: Source) {
        self.data.sources.insert(name, source);
    }

    pub fn remove(&mut self, name: &str) {
        self.data.sources.shift_remove(name);
    }

    pub fn list(&self) -> Vec<(&str, &Source)> {
        self.data.sources.iter().map(|(name, source)| (name.as_str(), source)).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.data.sources.get(name)
    }
}

pub trait ReferenceRefresh: Send + Sync {
    fn refresh(&self, sources: Vec<GitSource>);

    fn close(&self) {}
}

pub struct NoRefresh;

impl ReferenceRefresh for NoRefresh {
    fn refresh(&self, _sources: Vec<GitSource>) {
        log::debug!("reference cache refresh is unavailable without a repository cache");
    }
}

pub struct CacheRefresh {
    cache: Arc<dyn Materializer>,
    vars: Vars,
    tasks: Mutex<JoinSet<()>>,
}

impl CacheRefresh {
    pub fn new(cache: Arc<dyn Materializer>, vars: Vars) -> Self {
        CacheRefresh { cache, vars, tasks: Mutex::new(JoinSet::new()) }
    }
}

impl ReferenceRefresh for CacheRefresh {
    fn refresh(&self, sources: Vec<GitSource>) {
        let cache = self.cache.clone();
        let vars = self.vars.clone();
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            futures_util::stream::iter(sources)
                .for_each_concurrent(REFRESH_CONCURRENCY, |source| {
                    let cache = cache.clone();
                    let vars = vars.clone();
                    async move {
                        let reference = match repository::parse_remote(&source.repository, &vars) {
                            Ok(reference) => reference,
                            Err(error) => {
                                log::warn!("failed to materialize reference {}: {error}", source.repository);
                                return;
                            }
                        };
                        let branch = source.branch.filter(|branch| !branch.is_empty());
                        let input = EnsureInput { reference, branch, refresh: Some(Refresh::Daily) };
                        if let Err(error) = cache.ensure(input).await {
                            log::warn!("failed to materialize reference {}: {error}", source.repository);
                        }
                    }
                })
                .await;
        });
    }

    fn close(&self) {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner).abort_all();
    }
}

pub fn list(data: &ReferenceData, repos: &Path, vars: &Vars) -> Vec<ReferenceInfo> {
    data.sources
        .iter()
        .flat_map(|(name, source)| {
            let (path, generated) = match source {
                Source::Local(local) => (
                    local.path.clone(),
                    ReferenceSource::Local(ReferenceLocalSource {
                        r#type: ReferenceLocalSourceType::Local,
                        path: AbsolutePath(local.path.clone()),
                    }),
                ),
                Source::Git(git) => {
                    let reference = repository::parse(&git.repository, vars)?;
                    if !repository::is_remote(&reference) {
                        return None;
                    }
                    if let Some(branch) = git.branch.as_deref().filter(|branch| !branch.is_empty()) {
                        repository::validate_branch(branch).ok()?;
                    }
                    let path = repository::cache_path(&repos.to_string_lossy(), &reference, git.branch.as_deref());
                    (
                        path,
                        ReferenceSource::Git(ReferenceGitSource {
                            r#type: ReferenceGitSourceType::Git,
                            repository: git.repository.clone(),
                            branch: git.branch.clone(),
                        }),
                    )
                }
            };
            Some(ReferenceInfo {
                name: name.clone(),
                path: AbsolutePath(path),
                description: source.description().map(str::to_string),
                hidden: source.hidden(),
                source: generated,
            })
        })
        .collect()
}

pub fn refresh_sources(data: &ReferenceData, vars: &Vars) -> Vec<GitSource> {
    data.sources
        .values()
        .filter_map(|source| {
            let Source::Git(git) = source else {
                return None;
            };
            let reference = repository::parse(&git.repository, vars)?;
            if !repository::is_remote(&reference) {
                return None;
            }
            if let Some(branch) = git.branch.as_deref().filter(|branch| !branch.is_empty()) {
                repository::validate_branch(branch).ok()?;
            }
            Some(git.clone())
        })
        .collect()
}

#[derive(Clone)]
pub struct ReferenceService {
    inner: Arc<Inner>,
}

struct Inner {
    state: State<ReferenceData>,
    repos: PathBuf,
    vars: Vars,
    refresh: Arc<dyn ReferenceRefresh>,
    task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ReferenceService {
    pub fn new(
        events: Arc<dyn RegistryEvents>,
        repos: PathBuf,
        vars: Vars,
        refresh: Arc<dyn ReferenceRefresh>,
    ) -> Self {
        let state = State::new(Options {
            name: Some("reference".to_string()),
            notify: Some(changes(
                {
                    let repos = repos.clone();
                    let vars = vars.clone();
                    move |data: &ReferenceData| projection(data, &repos, &vars)
                },
                {
                    let events = events.clone();
                    let refresh = refresh.clone();
                    let vars = vars.clone();
                    move |data: Arc<ReferenceData>| {
                        let sources = refresh_sources(&data, &vars);
                        let events = events.clone();
                        let refresh = refresh.clone();
                        Box::pin(async move {
                            events.updated(EVENT);
                            refresh.refresh(sources);
                        })
                    }
                },
            )),
            ..Options::new(ReferenceData::default)
        });
        let task = tokio::spawn(hourly(state.clone(), refresh.clone(), vars.clone()));
        ReferenceService { inner: Arc::new(Inner { state, repos, vars, refresh, task }) }
    }

    pub async fn list(&self) -> Result<Vec<ReferenceInfo>, StateError> {
        let data = self.inner.state.get().await?;
        Ok(list(&data, &self.inner.repos, &self.inner.vars))
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a, 'b> Fn(&'a mut ReferenceEditor<'b>) + Send + Sync + 'static,
    {
        self.inner
            .state
            .transform(move |data| {
                let mut editor = ReferenceEditor::new(data);
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.inner.state.reload().await;
    }

    pub fn close(&self) {
        self.inner.task.abort();
        self.inner.refresh.close();
    }
}

async fn hourly(state: State<ReferenceData>, refresh: Arc<dyn ReferenceRefresh>, vars: Vars) {
    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        let Ok(data) = state.get().await else {
            continue;
        };
        refresh.refresh(refresh_sources(&data, &vars));
    }
}

fn projection(data: &ReferenceData, repos: &Path, vars: &Vars) -> String {
    let mut out = String::new();
    for info in list(data, repos, vars) {
        info.encode(&mut out);
    }
    out
}
