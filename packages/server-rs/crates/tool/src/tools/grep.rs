use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, AccessError, ResolveInput};
use opencode_fs::filesystem::Entry;
use opencode_fs::path;
use opencode_fs::ripgrep::{self, DEFAULT_SEARCH_LIMIT, GrepInput, Match, Ripgrep, Submatch};
use opencode_util::path::Flavor;
use serde_json::{Map, Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation};
use crate::tools::search::{self, PathError, PathKind, SearchError};

#[derive(Clone)]
pub struct GrepDeps {
    pub ripgrep: Arc<Ripgrep>,
    pub access: Access,
}

pub struct GrepRuntime {
    deps: GrepDeps,
}

impl GrepRuntime {
    pub fn new(deps: GrepDeps) -> Self {
        GrepRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = input.get("pattern").and_then(Value::as_str).unwrap_or_default().to_owned();
        let search_path = input.get("path").and_then(Value::as_str).map(str::to_owned);
        let display = search_path.clone().unwrap_or_else(|| ".".to_owned());
        let include = input.get("include").and_then(Value::as_str).map(str::to_owned);
        let literal = input.get("literal").and_then(Value::as_bool);
        let case_sensitive = input.get("caseSensitive").and_then(Value::as_bool);
        let limit = input.get("limit").and_then(Value::as_u64).map(|value| value as usize);

        let target = self
            .deps
            .access
            .resolve(ResolveInput { path: display.clone(), kind: None, base: None })
            .await
            .map_err(|error| access_error(error, &pattern))?;
        self.deps
            .access
            .authorize_external(&[&target], &file_invocation(context), None, context.authorizer.as_ref())
            .await
            .map_err(|error| access_error(error, &pattern))?;

        let mut metadata = Map::new();
        metadata.insert("root".to_owned(), json!("."));
        if let Some(path) = &search_path {
            metadata.insert("path".to_owned(), json!(path));
        }
        if let Some(include) = &include {
            metadata.insert("include".to_owned(), json!(include));
        }
        if let Some(literal) = literal {
            metadata.insert("literal".to_owned(), json!(literal));
        }
        if let Some(case_sensitive) = case_sensitive {
            metadata.insert("caseSensitive".to_owned(), json!(case_sensitive));
        }
        if let Some(limit) = limit {
            metadata.insert("limit".to_owned(), json!(limit));
        }
        context
            .authorizer
            .assert(search::permission(context, "grep", &pattern, metadata))
            .await
            .map_err(|error| ToolError::permission(&error))?;

        let root = target.absolute.clone();
        let kind = search::path_kind(&root).await.map_err(|error| path_error(error, &display, &pattern))?;
        let cwd = if kind == PathKind::Directory { root.clone() } else { path::dirname(&root) };
        let file = if kind == PathKind::File { Some(Flavor::NATIVE.basename(&root).to_owned()) } else { None };
        let directory = self.deps.access.working_directory().to_owned();
        let bounded = search::bounded(limit.unwrap_or(DEFAULT_SEARCH_LIMIT), |limit| {
            let ripgrep = self.deps.ripgrep.clone();
            let cwd = cwd.clone();
            let pattern = pattern.clone();
            let file = file.clone();
            let include = include.clone();
            async move {
                ripgrep
                    .grep(GrepInput {
                        cwd,
                        pattern,
                        file,
                        include,
                        literal: literal.unwrap_or(false),
                        case_sensitive,
                        limit,
                        ..GrepInput::default()
                    })
                    .await
            }
        })
        .await
        .map_err(|error| grep_error(error, &pattern))?;

        let matches: Vec<Match> = bounded
            .items
            .into_iter()
            .map(|item| Match {
                entry: Entry {
                    path: Flavor::NATIVE.relative(&directory, &Flavor::NATIVE.resolve(&[&cwd, &item.entry.path])),
                    kind: item.entry.kind,
                },
                line: item.line,
                offset: item.offset,
                text: item.text,
                submatches: item.submatches,
            })
            .collect();
        let output = Value::Array(matches.iter().map(match_value).collect());
        let content = vec![Content::text(model_content(&directory, &matches, bounded.truncated))];
        let metadata = json!({ "matches": matches.len(), "truncated": bounded.truncated });
        Ok(ToolOutput { output, content, metadata: Some(metadata) })
    }
}

impl ToolRuntime for GrepRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

fn submatch_value(submatch: &Submatch) -> Value {
    json!({ "text": submatch.text, "start": submatch.start, "end": submatch.end })
}

fn match_value(item: &Match) -> Value {
    json!({
        "entry": search::entry_value(&item.entry),
        "line": item.line,
        "offset": item.offset,
        "text": item.text,
        "submatches": item.submatches.iter().map(submatch_value).collect::<Vec<_>>(),
    })
}

fn model_content(directory: &str, matches: &[Match], truncated: bool) -> String {
    let mut lines = if matches.is_empty() {
        vec!["No matches found".to_owned()]
    } else {
        vec![format!("Found {} matches", matches.len())]
    };
    let mut current = String::new();
    for item in matches {
        let path = Flavor::NATIVE.resolve(&[directory, &item.entry.path]);
        if current != path {
            if !current.is_empty() {
                lines.push(String::new());
            }
            current = path.clone();
            lines.push(format!("{path}:"));
        }
        lines.push(format!("  Line {}: {}", item.line, item.text));
    }
    if truncated {
        lines.push(String::new());
        lines.push(format!(
            "(Results are truncated: showing first {} results. Consider using a more specific path or pattern.)",
            matches.len()
        ));
    }
    lines.join("\n")
}

fn access_error(error: AccessError, pattern: &str) -> ToolError {
    match error {
        AccessError::Permission(error) => ToolError::permission(error),
        AccessError::Failed { .. } => ToolError::new(format!("Unable to grep for {pattern}")),
    }
}

fn path_error(error: PathError, display: &str, pattern: &str) -> ToolError {
    match error {
        PathError::NotFound => ToolError::new(format!("Search path does not exist: {display}")),
        PathError::Failed => ToolError::new(format!("Unable to grep for {pattern}")),
    }
}

fn grep_error(error: SearchError<ripgrep::Error>, pattern: &str) -> ToolError {
    match error {
        SearchError::Timeout => search::timeout_error(),
        SearchError::Inner(ripgrep::Error::InvalidGlob { message }) => {
            ToolError::new(format!("Invalid glob in include {message}"))
        }
        SearchError::Inner(ripgrep::Error::InvalidPattern { message, .. }) => {
            ToolError::new(format!("Invalid regex pattern: {message}"))
        }
        SearchError::Inner(_) => ToolError::new(format!("Unable to grep for {pattern}")),
    }
}
