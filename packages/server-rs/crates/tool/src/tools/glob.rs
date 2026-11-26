use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, AccessError, Kind, ResolveInput};
use opencode_fs::filesystem::Entry;
use opencode_fs::ripgrep::{self, DEFAULT_SEARCH_LIMIT, GlobInput, Ripgrep};
use opencode_util::path::Flavor;
use serde_json::{Map, Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation};
use crate::tools::search::{self, PathError, PathKind, SearchError};

#[derive(Clone)]
pub struct GlobDeps {
    pub ripgrep: Arc<Ripgrep>,
    pub access: Access,
}

pub struct GlobRuntime {
    deps: GlobDeps,
}

impl GlobRuntime {
    pub fn new(deps: GlobDeps) -> Self {
        GlobRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = input.get("pattern").and_then(Value::as_str).unwrap_or_default().to_owned();
        let search_path = optional_path(&input, "path");
        let root_display = search_path.clone().unwrap_or_else(|| ".".to_owned());
        let hidden = input.get("hidden").and_then(Value::as_bool);
        let limit = input.get("limit").and_then(Value::as_u64).map(|value| value as usize);

        let target = self
            .deps
            .access
            .resolve(ResolveInput { path: root_display.clone(), kind: Some(Kind::Directory), base: None })
            .await
            .map_err(|error| access_error(error, &pattern))?;
        self.deps
            .access
            .authorize_external(&[&target], &file_invocation(context), None, context.authorizer.as_ref())
            .await
            .map_err(|error| access_error(error, &pattern))?;

        let mut metadata = Map::new();
        metadata.insert("root".to_owned(), json!(root_display));
        if let Some(path) = &search_path {
            metadata.insert("path".to_owned(), json!(path));
        }
        if let Some(hidden) = hidden {
            metadata.insert("hidden".to_owned(), json!(hidden));
        }
        if let Some(limit) = limit {
            metadata.insert("limit".to_owned(), json!(limit));
        }
        context
            .authorizer
            .assert(search::permission(context, "glob", &pattern, metadata))
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;

        let kind =
            search::path_kind(&target.absolute).await.map_err(|error| path_error(error, &root_display, &pattern))?;
        if kind != PathKind::Directory {
            return Err(ToolError::new(format!("Search path is not a directory: {root_display}")));
        }

        let root = target.absolute.clone();
        let directory = self.deps.access.working_directory().to_owned();
        let bounded = search::bounded(limit.unwrap_or(DEFAULT_SEARCH_LIMIT), |limit| {
            let ripgrep = self.deps.ripgrep.clone();
            let cwd = root.clone();
            let pattern = pattern.clone();
            async move {
                ripgrep
                    .glob(GlobInput { cwd, pattern, hidden: hidden.unwrap_or(false), limit, ..GlobInput::default() })
                    .await
            }
        })
        .await
        .map_err(|error| glob_error(error, &pattern))?;

        let entries: Vec<Entry> = bounded
            .items
            .into_iter()
            .map(|entry| Entry {
                path: Flavor::NATIVE.relative(&directory, &Flavor::NATIVE.resolve(&[&root, &entry.path])),
                kind: entry.kind,
            })
            .collect();
        let output = Value::Array(entries.iter().map(search::entry_value).collect());
        let content = vec![Content::text(model_content(&directory, &entries, bounded.truncated))];
        let metadata = json!({ "count": entries.len(), "truncated": bounded.truncated });
        Ok(ToolOutput { output, content, metadata: Some(metadata) })
    }
}

impl ToolRuntime for GlobRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

fn optional_path(input: &Value, key: &str) -> Option<String> {
    match input.get(key).and_then(Value::as_str) {
        None | Some("undefined") | Some("null") => None,
        Some(path) => Some(path.to_owned()),
    }
}

fn model_content(directory: &str, entries: &[Entry], truncated: bool) -> String {
    let mut lines = if entries.is_empty() {
        vec!["No files found".to_owned()]
    } else {
        entries.iter().map(|entry| Flavor::NATIVE.resolve(&[directory, &entry.path])).collect()
    };
    if truncated {
        lines.push(String::new());
        lines.push(format!(
            "(Results are truncated: showing first {} results. Consider using a more specific path or pattern.)",
            entries.len()
        ));
    }
    lines.join("\n")
}

fn access_error(error: AccessError, pattern: &str) -> ToolError {
    match error {
        AccessError::Permission(error) => ToolError::new(error.to_string()),
        AccessError::Failed { .. } => ToolError::new(format!("Unable to find files matching {pattern}")),
    }
}

fn path_error(error: PathError, display: &str, pattern: &str) -> ToolError {
    match error {
        PathError::NotFound => ToolError::new(format!("Search path does not exist: {display}")),
        PathError::Failed => ToolError::new(format!("Unable to find files matching {pattern}")),
    }
}

fn glob_error(error: SearchError<ripgrep::Error>, pattern: &str) -> ToolError {
    match error {
        SearchError::Timeout => search::timeout_error(),
        SearchError::Inner(ripgrep::Error::InvalidGlob { message }) => {
            ToolError::new(format!("Invalid glob in pattern {message}"))
        }
        SearchError::Inner(_) => ToolError::new(format!("Unable to find files matching {pattern}")),
    }
}
