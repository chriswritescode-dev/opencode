use std::future::Future;
use std::io::ErrorKind;
use std::time::Duration;

use opencode_contract::codec::JsonMap;
use opencode_fs::filesystem::{Entry, EntryKind};
use opencode_fs::ripgrep::DEFAULT_SEARCH_TIMEOUT_MS;
use opencode_permission::service::AssertInput;
use serde_json::{Map, Value, json};

use crate::context::{ToolContext, ToolError, permission_input};

pub struct Bounded<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

pub enum SearchError<E> {
    Timeout,
    Inner(E),
}

/// Fetches one result past the limit so truncation can be reported without a second search.
pub async fn bounded<T, E, F, Fut>(limit: usize, search: F) -> Result<Bounded<T>, SearchError<E>>
where
    F: FnOnce(usize) -> Fut,
    Fut: Future<Output = Result<Vec<T>, E>>,
{
    match tokio::time::timeout(Duration::from_millis(DEFAULT_SEARCH_TIMEOUT_MS), search(limit.saturating_add(1))).await
    {
        Ok(Ok(items)) => {
            let truncated = items.len() > limit;
            Ok(Bounded { items: items.into_iter().take(limit).collect(), truncated })
        }
        Ok(Err(error)) => Err(SearchError::Inner(error)),
        Err(_) => Err(SearchError::Timeout),
    }
}

pub fn timeout_error() -> ToolError {
    ToolError::new(format!(
        "Search timed out after {} seconds. Consider using a more specific path or pattern.",
        DEFAULT_SEARCH_TIMEOUT_MS / 1_000
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    NotFound,
    Failed,
}

/// `Environment.typeFollowing`: follows a final symlink and reports what it points at.
pub async fn path_kind(absolute: &str) -> Result<PathKind, PathError> {
    match tokio::fs::metadata(absolute).await {
        Ok(metadata) if metadata.is_dir() => Ok(PathKind::Directory),
        Ok(metadata) if metadata.is_file() => Ok(PathKind::File),
        Ok(_) => Ok(PathKind::Other),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Err(PathError::NotFound)
        }
        Err(_) => Err(PathError::Failed),
    }
}

pub fn permission(context: &ToolContext, action: &str, pattern: &str, metadata: Map<String, Value>) -> AssertInput {
    let mut input = permission_input(context, action, vec![pattern.to_owned()], vec!["*".to_owned()]);
    input.metadata = Some(JsonMap(metadata.into_iter().collect()));
    input
}

pub fn entry_value(entry: &Entry) -> Value {
    json!({
        "path": entry.path,
        "type": match entry.kind {
            EntryKind::File => "file",
            EntryKind::Directory => "directory",
        },
    })
}
