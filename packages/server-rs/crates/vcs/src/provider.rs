use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_contract::generated::requests::{VcsMode, VcsUntracked};
use opencode_contract::generated::{FileDiffInfo, VcsBase, VcsFileStatus, VcsInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsScope {
    pub directory: String,
    pub worktree: String,
    pub canonical: String,
    pub store: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchOptions {
    pub search: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct BranchesInput {
    pub scope: VcsScope,
    pub search: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ProviderDiffInput {
    pub scope: VcsScope,
    pub mode: VcsMode,
    pub base: Option<String>,
    pub context: i32,
    pub max_output_bytes: usize,
    pub untracked: Option<VcsUntracked>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct DiffError {
    pub message: String,
}

impl DiffError {
    pub fn new(message: impl Into<String>) -> Self {
        DiffError { message: message.into() }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Diff(DiffError),
    #[error("{0}")]
    Message(String),
}

impl ProviderError {
    pub fn message(message: impl Into<String>) -> Self {
        ProviderError::Message(message.into())
    }
}

impl From<DiffError> for ProviderError {
    fn from(error: DiffError) -> Self {
        ProviderError::Diff(error)
    }
}

pub type ProviderFuture<T> = BoxFuture<'static, Result<T, ProviderError>>;

pub trait VcsProvider: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    fn init(&self, _scope: VcsScope) -> Option<ProviderFuture<()>> {
        None
    }

    fn info(&self, scope: VcsScope) -> ProviderFuture<VcsInfo>;

    fn base(&self, _scope: VcsScope) -> Option<ProviderFuture<Option<VcsBase>>> {
        None
    }

    fn branches(&self, input: BranchesInput) -> ProviderFuture<Vec<String>>;

    fn status(&self, scope: VcsScope) -> ProviderFuture<Vec<VcsFileStatus>>;

    fn diff(&self, input: ProviderDiffInput) -> ProviderFuture<Vec<FileDiffInfo>>;
}

#[derive(Default)]
pub struct VcsData {
    pub providers: BTreeMap<String, Arc<dyn VcsProvider>>,
    pub selection: Option<String>,
}

pub struct Editor<'a> {
    data: &'a mut VcsData,
}

impl Editor<'_> {
    pub fn add(&mut self, provider: Arc<dyn VcsProvider>) {
        self.data.providers.insert(provider.id().to_owned(), provider);
    }

    pub fn default(&self) -> Option<&str> {
        self.data.selection.as_deref()
    }

    pub fn set_default(&mut self, id: impl Into<String>) {
        self.data.selection = Some(id.into());
    }
}

pub(crate) fn edit(data: &mut VcsData, f: impl FnOnce(&mut Editor<'_>)) {
    f(&mut Editor { data });
}
