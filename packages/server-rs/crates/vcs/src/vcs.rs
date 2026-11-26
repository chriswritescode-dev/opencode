use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::codec::{Decode, Encode, decode_json, encode_json};
use opencode_contract::generated::requests::{VcsMode, VcsUntracked};
use opencode_contract::generated::{
    AbsolutePath, FileDiffInfo, LocationRef, VcsBase, VcsBranch, VcsFileStatus, VcsInfo, WorkspaceID,
};
use opencode_git::{MAX_TOTAL_PATCH_BYTES, PATCH_CONTEXT_LINES, empty_patch};
use opencode_location::location::LocationInfo;
use opencode_location::state::{Options, Registration, State};
use opencode_util::path::{Flavor, realpath};
use serde_json::{Map, Value};
use tokio::task::JoinHandle;

use crate::provider::{
    BranchOptions, BranchesInput, DiffError, Editor, ProviderDiffInput, ProviderError, ProviderFuture, VcsData,
    VcsProvider, VcsScope,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InitializeError {
    #[error("missing")]
    Missing,
    #[error("conflict")]
    Conflict,
    #[error("unknown")]
    Unknown,
    #[error("unsupported")]
    Unsupported,
    #[error("failed")]
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct DiffOptions {
    pub context: Option<i32>,
    pub base: Option<String>,
    pub untracked: Option<VcsUntracked>,
}

struct Inner {
    bus: Arc<Bus>,
    scope: VcsScope,
    location: LocationRef,
    kind: Option<&'static str>,
    info: Mutex<VcsInfo>,
    refresh_lock: tokio::sync::Mutex<()>,
    state: State<VcsData>,
}

pub struct Vcs {
    inner: Arc<Inner>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Vcs {
    pub fn new(bus: Arc<Bus>, info: &LocationInfo) -> Arc<Self> {
        let scope = VcsScope {
            directory: info.directory.clone(),
            worktree: info.project.directory.clone(),
            canonical: info.project.canonical.clone(),
            store: info.vcs.as_ref().map(|vcs| vcs.store().to_owned()),
        };
        let location = LocationRef {
            directory: AbsolutePath(info.directory.clone()),
            workspace_id: info.workspace_id.clone().map(WorkspaceID),
        };
        let kind = info.vcs.as_ref().map(|vcs| vcs.kind());
        let subscription = info.vcs.as_ref().map(|vcs| {
            let store = realpath(Path::new(vcs.store()))
                .map_or_else(|| vcs.store().to_owned(), |path| path.to_string_lossy().into_owned());
            (vcs.kind() == "git", store, bus.subscribe(&["filesystem.changed"], Some(location.clone())))
        });
        let inner = Arc::new_cyclic(|weak| {
            let weak: Weak<Inner> = weak.clone();
            let state = State::new(Options {
                name: Some("vcs".to_owned()),
                initial: Box::new(VcsData::default),
                notify: Some(Box::new(move |_current: Arc<VcsData>| {
                    let weak = weak.clone();
                    Box::pin(async move {
                        let Some(inner) = weak.upgrade() else {
                            return;
                        };
                        inner.refresh().await;
                    })
                })),
            });
            Inner {
                bus,
                scope,
                location,
                kind,
                info: Mutex::new(empty_info()),
                refresh_lock: tokio::sync::Mutex::new(()),
                state,
            }
        });
        let task = subscription.map(|(git, store, subscription)| {
            let weak = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let mut subscription = subscription;
                while let Some(event) = subscription.recv().await {
                    let Some(file) = event.data().get("file").and_then(Value::as_str) else {
                        continue;
                    };
                    let branch_metadata = if git {
                        Flavor::NATIVE.basename(file) == "HEAD" && contains(&store, file)
                    } else {
                        Flavor::NATIVE.resolve(&[file]) == Flavor::NATIVE.join(&[&store, "branch"])
                    };
                    if !branch_metadata {
                        continue;
                    }
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    inner.refresh().await;
                }
            })
        });
        Arc::new(Vcs { inner, task: Mutex::new(task) })
    }

    pub async fn transform(&self, edit: impl Fn(&mut Editor<'_>) + Send + Sync + 'static) -> Registration {
        self.inner
            .state
            .transform(move |data| {
                crate::provider::edit(data, |editor| edit(editor));
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.inner.state.reload().await;
    }

    pub async fn initialize(&self, provider_id: &str) -> Result<(), InitializeError> {
        if !matches!(tokio::fs::metadata(&self.inner.scope.worktree).await, Ok(metadata) if metadata.is_dir()) {
            return Err(InitializeError::Missing);
        }
        if has_marker(&self.inner.scope.worktree).await {
            return Err(InitializeError::Conflict);
        }
        let provider = match self.inner.state.get().await {
            Ok(data) => data.providers.get(provider_id).cloned(),
            Err(error) => {
                log::warn!("failed to read vcs state: {error}");
                None
            }
        };
        let Some(provider) = provider else {
            return Err(InitializeError::Unknown);
        };
        let Some(future) = provider.init(self.inner.scope.clone()) else {
            return Err(InitializeError::Unsupported);
        };
        match future.await {
            Ok(()) => Ok(()),
            Err(error) => {
                log::warn!("vcs initialization failed provider={provider_id}: {error}");
                Err(InitializeError::Failed)
            }
        }
    }

    pub async fn info(&self) -> VcsInfo {
        self.inner.info.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub async fn base(&self) -> Result<Option<VcsBase>, DiffError> {
        let provider = match self.inner.selected().await {
            Ok(provider) => provider,
            Err(error) => {
                log::warn!("failed to read vcs state: {error}");
                return Ok(None);
            }
        };
        let Some(provider) = provider else {
            return Ok(None);
        };
        let Some(future) = provider.base(self.inner.scope.clone()) else {
            return Ok(None);
        };
        review(provider.as_ref(), "base", future).await
    }

    pub async fn branches(&self, options: BranchOptions) -> Vec<String> {
        let provider = match self.inner.selected().await {
            Ok(provider) => provider,
            Err(error) => {
                log::warn!("failed to read vcs state: {error}");
                return Vec::new();
            }
        };
        let Some(provider) = provider else {
            return Vec::new();
        };
        let input = BranchesInput { scope: self.inner.scope.clone(), search: options.search, limit: options.limit };
        protect(provider.as_ref(), "branches", provider.branches(input), Vec::new()).await
    }

    pub async fn status(&self) -> Vec<VcsFileStatus> {
        let provider = match self.inner.selected().await {
            Ok(provider) => provider,
            Err(error) => {
                log::warn!("failed to read vcs state: {error}");
                return Vec::new();
            }
        };
        let Some(provider) = provider else {
            return Vec::new();
        };
        protect(provider.as_ref(), "status", provider.status(self.inner.scope.clone()), Vec::new()).await
    }

    pub async fn diff(&self, mode: VcsMode, options: DiffOptions) -> Result<Vec<FileDiffInfo>, DiffError> {
        let provider = match self.inner.selected().await {
            Ok(provider) => provider,
            Err(error) => {
                log::warn!("failed to read vcs state: {error}");
                return Ok(Vec::new());
            }
        };
        let Some(provider) = provider else {
            return Ok(Vec::new());
        };
        let input = ProviderDiffInput {
            scope: self.inner.scope.clone(),
            mode,
            base: options.base,
            context: options.context.unwrap_or(PATCH_CONTEXT_LINES),
            max_output_bytes: MAX_TOTAL_PATCH_BYTES,
            untracked: options.untracked,
        };
        let rows = review(provider.as_ref(), "diff", provider.diff(input)).await?;
        let mut total = 0;
        Ok(rows
            .into_iter()
            .map(|mut row| {
                let bytes = row.patch.len();
                if total + bytes > MAX_TOTAL_PATCH_BYTES {
                    row.patch = empty_patch(&row.file);
                    return row;
                }
                total += bytes;
                row
            })
            .collect())
    }

    pub fn close(&self) {
        if let Some(task) = self.task.lock().unwrap_or_else(|error| error.into_inner()).take() {
            task.abort();
        }
    }
}

impl Drop for Vcs {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap_or_else(|error| error.into_inner()).take() {
            task.abort();
        }
    }
}

impl Inner {
    async fn selected(&self) -> Result<Option<Arc<dyn VcsProvider>>, opencode_location::state::StateError> {
        let data = self.state.get().await?;
        let Some(id) = data.selection.clone().or_else(|| self.kind.map(str::to_owned)) else {
            return Ok(None);
        };
        Ok(data.providers.get(&id).cloned())
    }

    async fn refresh(&self) {
        let changed = {
            let _permit = self.refresh_lock.lock().await;
            let next = match self.selected().await {
                Ok(Some(provider)) => {
                    let mut info =
                        protect(provider.as_ref(), "info", provider.info(self.scope.clone()), empty_info()).await;
                    info.provider = Some(provider.id().to_owned());
                    info
                }
                Ok(None) => empty_info(),
                Err(error) => {
                    log::warn!("vcs refresh failed: {error}");
                    return;
                }
            };
            let mut current = self.info.lock().unwrap_or_else(|error| error.into_inner());
            let changed = current.branch.current != next.branch.current;
            *current = next;
            changed
        };
        if !changed {
            return;
        }
        loop {
            let branch = self.info.lock().unwrap_or_else(|error| error.into_inner()).branch.current.clone();
            self.publish(branch.clone());
            let latest = self.info.lock().unwrap_or_else(|error| error.into_inner()).branch.current.clone();
            if latest == branch {
                return;
            }
        }
    }

    fn publish(&self, branch: Option<String>) {
        let mut data = Map::new();
        if let Some(branch) = branch {
            data.insert("branch".to_owned(), Value::String(branch));
        }
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&Definition::ephemeral("vcs.branch.updated"), Value::Object(data), options)
        {
            log::warn!("failed to publish vcs branch update: {error}");
        }
    }
}

async fn protect<T: Encode + Decode>(
    provider: &dyn VcsProvider,
    operation: &str,
    future: ProviderFuture<T>,
    fallback: T,
) -> T {
    match future.await {
        Ok(value) => match validated(&value) {
            Some(value) => value,
            None => {
                log::warn!("vcs provider failed provider={} operation={operation}: invalid output", provider.id());
                fallback
            }
        },
        Err(error) => {
            log::warn!("vcs provider failed provider={} operation={operation}: {error}", provider.id());
            fallback
        }
    }
}

async fn review<T: Encode + Decode>(
    provider: &dyn VcsProvider,
    operation: &str,
    future: ProviderFuture<T>,
) -> Result<T, DiffError> {
    let message = match operation {
        "base" => "VCS provider could not resolve a review base",
        _ => "VCS provider could not produce a diff",
    };
    match future.await {
        Ok(value) => validated(&value).ok_or_else(|| {
            log::warn!("vcs provider failed provider={} operation={operation}: invalid output", provider.id());
            DiffError::new(message)
        }),
        Err(error) => {
            log::warn!("vcs provider failed provider={} operation={operation}: {error}", provider.id());
            Err(match error {
                ProviderError::Diff(error) => error,
                ProviderError::Message(_) => DiffError::new(message),
            })
        }
    }
}

fn validated<T: Encode + Decode>(value: &T) -> Option<T> {
    decode_json(&encode_json(value)).ok()
}

fn empty_info() -> VcsInfo {
    VcsInfo { provider: None, branch: VcsBranch { current: None, default: None } }
}

fn contains(parent: &str, child: &str) -> bool {
    let relative = Flavor::NATIVE.relative(parent, child);
    relative.is_empty() || !Flavor::NATIVE.outside(&relative)
}

async fn has_marker(start: &str) -> bool {
    let mut current = Some(PathBuf::from(start));
    while let Some(node) = current {
        for name in [".git", ".hg"] {
            if tokio::fs::symlink_metadata(node.join(name)).await.is_ok() {
                return true;
            }
        }
        current = node.parent().map(Path::to_path_buf);
    }
    false
}
