use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use futures_util::future::BoxFuture;
use opencode_config::observer::ObserverHandle;
use opencode_config::service::ConfigService;
use opencode_git::Git;
use opencode_location::state::{Options, Registration, State, TransformError};
use opencode_util::path::{Flavor, realpath};

use crate::worktree::WorktreeError;

const ADJECTIVES: &[&str] = &[
    "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing", "happy", "hidden", "jolly",
    "kind", "lucky", "mighty", "misty", "neon", "nimble", "playful", "proud", "quick", "quiet", "shiny", "silent",
    "stellar", "sunny", "swift", "tidy", "witty",
];

const NOUNS: &[&str] = &[
    "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest", "garden", "harbor",
    "island", "knight", "lagoon", "meadow", "moon", "mountain", "nebula", "orchid", "otter", "panda", "pixel",
    "planet", "river", "rocket", "sailor", "squid", "star", "tiger", "wizard", "wolf",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeEntryKind {
    Root,
    Worktree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub directory: String,
    pub kind: WorktreeEntryKind,
}

#[derive(Clone, Copy, Debug)]
pub struct StrategyCreateInput<'a> {
    pub source_directory: &'a str,
    pub directory: &'a str,
    pub branch: Option<&'a str>,
}

#[derive(Clone, Copy, Debug)]
pub struct StrategyRemoveInput<'a> {
    pub directory: &'a str,
    pub force: bool,
}

pub trait WorktreeStrategy: Send + Sync {
    fn id(&self) -> &str;
    fn create<'a>(&'a self, input: StrategyCreateInput<'a>) -> BoxFuture<'a, Result<String, WorktreeError>>;
    fn remove<'a>(&'a self, input: StrategyRemoveInput<'a>) -> BoxFuture<'a, Result<(), WorktreeError>>;
    fn list<'a>(&'a self, directory: &'a str) -> BoxFuture<'a, Result<Vec<WorktreeListEntry>, WorktreeError>>;
}

pub struct GitWorktreeStrategy {
    id: String,
    git: Arc<Git>,
}

impl GitWorktreeStrategy {
    pub fn new(id: impl Into<String>, git: Arc<Git>) -> Self {
        GitWorktreeStrategy { id: id.into(), git }
    }
}

impl WorktreeStrategy for GitWorktreeStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn create<'a>(&'a self, input: StrategyCreateInput<'a>) -> BoxFuture<'a, Result<String, WorktreeError>> {
        Box::pin(async move {
            let Some(repository) = self.git.discover(input.source_directory).await else {
                return Err(WorktreeError::DirectoryUnavailable { directory: input.source_directory.to_owned() });
            };
            self.git.worktree_create(&repository, input.directory, input.branch).await?;
            canonical_directory(input.directory)
        })
    }

    fn remove<'a>(&'a self, input: StrategyRemoveInput<'a>) -> BoxFuture<'a, Result<(), WorktreeError>> {
        Box::pin(async move {
            let Some(repository) = self.git.discover(input.directory).await else {
                return Err(WorktreeError::DirectoryUnavailable { directory: input.directory.to_owned() });
            };
            self.git.worktree_remove(&repository, input.directory, input.force).await?;
            Ok(())
        })
    }

    fn list<'a>(&'a self, directory: &'a str) -> BoxFuture<'a, Result<Vec<WorktreeListEntry>, WorktreeError>> {
        Box::pin(async move {
            let Some(repository) = self.git.discover(directory).await else {
                return Err(WorktreeError::DirectoryUnavailable { directory: directory.to_owned() });
            };
            let entries = self.git.worktree_list(&repository).await?;
            let mut listed = Vec::with_capacity(entries.len());
            for entry in entries {
                let Ok(directory) = canonical_directory(&entry.directory) else {
                    continue;
                };
                let kind = match entry.kind {
                    opencode_git::WorktreeKind::Main => WorktreeEntryKind::Root,
                    opencode_git::WorktreeKind::Linked => WorktreeEntryKind::Worktree,
                };
                listed.push(WorktreeListEntry { directory, kind });
            }
            Ok(listed)
        })
    }
}

#[derive(Clone)]
pub struct WorktreeStrategiesData {
    pub directory: String,
    pub strategies: Vec<(String, Arc<dyn WorktreeStrategy>)>,
    pub selected: String,
}

pub struct Editor<'a> {
    data: &'a mut WorktreeStrategiesData,
}

impl Editor<'_> {
    pub fn add(&mut self, strategy: Arc<dyn WorktreeStrategy>) {
        let id = strategy.id().to_owned();
        self.data.strategies.retain(|(candidate, _)| *candidate != id);
        self.data.strategies.push((id.clone(), strategy));
        self.data.selected = id;
    }

    pub fn configure(&mut self, directory: impl Into<String>) {
        self.data.directory = directory.into();
    }
}

pub struct WorktreeStrategies {
    state: State<WorktreeStrategiesData>,
    canonical: String,
    configured: Arc<RwLock<Option<String>>>,
    registration: Mutex<Option<Registration>>,
    observer: Mutex<Option<ObserverHandle>>,
}

impl WorktreeStrategies {
    pub fn new(git: Arc<Git>, project_id: &str, canonical: &str, data: impl AsRef<Path>) -> Self {
        let data = data.as_ref().to_string_lossy().into_owned();
        let prefix: String = project_id.chars().take(6).collect();
        let initial_directory = Flavor::NATIVE.join(&[&data, "worktree", &prefix]);
        let git: Arc<dyn WorktreeStrategy> = Arc::new(GitWorktreeStrategy::new("git", git));
        let state = State::new(Options {
            name: Some("worktree".to_owned()),
            initial: Box::new(move || WorktreeStrategiesData {
                directory: initial_directory.clone(),
                strategies: vec![("git".to_owned(), git.clone())],
                selected: "git".to_owned(),
            }),
            notify: None,
        });
        WorktreeStrategies {
            state,
            canonical: canonical.to_owned(),
            configured: Arc::new(RwLock::new(None)),
            registration: Mutex::new(None),
            observer: Mutex::new(None),
        }
    }

    pub async fn get(&self) -> Result<Arc<WorktreeStrategiesData>, opencode_location::state::StateError> {
        self.state.get().await
    }

    pub async fn transform(&self, edit: impl Fn(&mut Editor<'_>) + Send + Sync + 'static) -> Registration {
        self.state
            .transform(move |data| {
                let mut editor = Editor { data };
                edit(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }

    pub async fn observe_config(&self, config: &ConfigService, home: impl AsRef<Path>) {
        let configured = opencode_config::entry::latest(&config.entries(), |info| {
            info.worktree.as_ref().map(|worktree| worktree.directory.clone())
        });
        *self.configured.write().unwrap_or_else(|error| error.into_inner()) = configured;
        let slot = self.configured.clone();
        let canonical = self.canonical.clone();
        let home = home.as_ref().to_string_lossy().into_owned();
        let registration = self
            .state
            .transform(move |data| {
                if let Some(directory) = slot.read().unwrap_or_else(|error| error.into_inner()).clone() {
                    data.directory = configured_directory(&canonical, &home, &directory);
                }
                Ok::<(), TransformError>(())
            })
            .await;
        *self.registration.lock().unwrap_or_else(|error| error.into_inner()) = Some(registration);
        let slot = self.configured.clone();
        let state = self.state.clone();
        let observer = opencode_config::observer::observe(config, move |entries| {
            *slot.write().unwrap_or_else(|error| error.into_inner()) =
                opencode_config::entry::latest(entries, |info| {
                    info.worktree.as_ref().map(|worktree| worktree.directory.clone())
                });
            state.invalidate();
        });
        *self.observer.lock().unwrap_or_else(|error| error.into_inner()) = Some(observer);
    }
}

pub fn canonical_directory(directory: &str) -> Result<String, WorktreeError> {
    let resolved = realpath(Path::new(directory))
        .ok_or_else(|| WorktreeError::DirectoryUnavailable { directory: directory.to_owned() })?;
    if !resolved.is_dir() {
        return Err(WorktreeError::DirectoryUnavailable { directory: directory.to_owned() });
    }
    Ok(resolved.to_string_lossy().into_owned())
}

pub fn create_slug() -> String {
    let adjective = ADJECTIVES[(rand::random::<u64>() % ADJECTIVES.len() as u64) as usize];
    let noun = NOUNS[(rand::random::<u64>() % NOUNS.len() as u64) as usize];
    format!("{adjective}-{noun}")
}

fn configured_directory(canonical: &str, home: &str, directory: &str) -> String {
    match directory.strip_prefix("~/") {
        Some(rest) => Flavor::NATIVE.join(&[home, rest]),
        None => Flavor::NATIVE.resolve(&[canonical, directory]),
    }
}
