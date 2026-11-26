use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use indexmap::IndexMap;
use opencode_contract::codec::JsonMap;
use opencode_fs::FsError;
use opencode_fs::access::{self, Access, AccessError, Authorizer, Kind, ResolveInput, Target};
use opencode_fs::bom;
use opencode_fs::diff::{self, FileDiff, FileDiffStatus};
use opencode_fs::mutation::{DecodedText, Mutation};
use opencode_fs::write::FormatHook;
use serde_json::{Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation, permission_input};
use crate::patch::{self, Hunk, ParseError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

pub trait PatchFiles: Send + Sync {
    fn write<'a>(&'a self, target: &'a Target, content: &'a [u8]) -> BoxFuture<'a, Result<(), FsError>>;
    fn remove<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Result<(), FsError>>;
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), FsError>>;
    fn kind<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Option<EntryKind>>;
}

#[derive(Clone)]
pub struct HostFiles {
    mutation: Mutation,
}

impl HostFiles {
    pub fn new(mutation: Mutation) -> Self {
        HostFiles { mutation }
    }
}

impl PatchFiles for HostFiles {
    fn write<'a>(&'a self, target: &'a Target, content: &'a [u8]) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move { self.mutation.write(target, content).await.map(|_| ()) })
    }

    fn remove<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(absolute).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) if error.kind() == ErrorKind::IsADirectory => {
                    tokio::fs::remove_dir_all(absolute).await.map_err(|error| io_error(absolute, &error))
                }
                Err(error) => Err(io_error(absolute, &error)),
            }
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let destination = match tokio::fs::symlink_metadata(to).await {
                Ok(metadata) if metadata.is_dir() => {
                    let name = Path::new(from).file_name().map(|name| name.to_string_lossy().into_owned());
                    match name {
                        Some(name) => Path::new(to).join(name).to_string_lossy().into_owned(),
                        None => to.to_owned(),
                    }
                }
                _ => to.to_owned(),
            };
            tokio::fs::rename(from, &destination).await.map_err(|error| io_error(from, &error))
        })
    }

    fn kind<'a>(&'a self, absolute: &'a str) -> BoxFuture<'a, Option<EntryKind>> {
        Box::pin(async move {
            let metadata = tokio::fs::symlink_metadata(absolute).await.ok()?;
            let file_type = metadata.file_type();
            Some(if file_type.is_symlink() {
                EntryKind::Symlink
            } else if file_type.is_file() {
                EntryKind::File
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::Other
            })
        })
    }
}

#[derive(Clone)]
pub struct PatchDeps {
    pub access: Access,
    pub mutation: Mutation,
    pub authorizer: Arc<dyn Authorizer>,
    pub format: Arc<dyn FormatHook>,
    pub files: Arc<dyn PatchFiles>,
}

impl PatchDeps {
    pub fn new(
        access: Access,
        mutation: Mutation,
        authorizer: Arc<dyn Authorizer>,
        format: Arc<dyn FormatHook>,
    ) -> Self {
        let files = Arc::new(HostFiles::new(mutation.clone()));
        PatchDeps { access, mutation, authorizer, format, files }
    }
}

pub struct PatchRuntime {
    deps: PatchDeps,
}

impl PatchRuntime {
    pub fn new(deps: PatchDeps) -> Self {
        PatchRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let patch_text = input.get("patchText").and_then(Value::as_str).unwrap_or_default().to_owned();
        let parsed = patch::parse(&patch_text);
        let lock_targets = match &parsed {
            Ok(hunks) => hunks
                .iter()
                .flat_map(|hunk| {
                    let mut paths = vec![hunk.path().to_owned()];
                    if let Some(move_path) = hunk.move_path() {
                        paths.push(move_path.to_owned());
                    }
                    paths
                })
                .map(|path| self.deps.access.locate(&path))
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        self.deps.mutation.with_lock(&lock_targets, self.apply(patch_text, parsed, context)).await
    }

    async fn apply(
        &self,
        patch_text: String,
        parsed: Result<Vec<Hunk>, ParseError>,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        if patch_text.is_empty() {
            return Err(ToolError::new("patchText is required"));
        }
        let hunks = parsed.map_err(|error| ToolError::new(format!("patch verification failed: {error}")))?;
        if hunks.is_empty() {
            return Err(ToolError::new("patch rejected: empty patch"));
        }
        let mut completed: Vec<String> = Vec::new();
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut updates: IndexMap<String, UpdateEntry> = IndexMap::new();
        for hunk in &hunks {
            self.prepare(hunk, context, &mut prepared, &mut updates).await?;
        }

        let patch_files = prepared.iter().map(|change| patch_file(change, None)).collect::<Vec<_>>();
        let targets = prepared.iter().flat_map(Prepared::targets).collect::<Vec<_>>();
        let mut resources = Vec::new();
        let mut save = Vec::new();
        for target in &targets {
            if !resources.contains(&target.resource) {
                resources.push(target.resource.clone());
            }
            let always = access::always(target);
            if !save.contains(&always) {
                save.push(always);
            }
        }
        let mut metadata = permission_input(context, "edit", resources, save);
        metadata.metadata = Some(JsonMap(vec![
            (
                "filepath".to_owned(),
                Value::String(targets.iter().map(|target| target.resource.as_str()).collect::<Vec<_>>().join(", ")),
            ),
            ("diff".to_owned(), Value::String(patch_files.iter().map(|file| format!("{}\n", file.patch)).collect())),
            ("files".to_owned(), json!(patch_files)),
        ]));
        self.deps.authorizer.assert(metadata).await.map_err(|error| ToolError::new(error.to_string()))?;

        for change in &prepared {
            self.apply_change(change, &mut completed).await?;
        }

        let mut formatted: IndexMap<String, String> = IndexMap::new();
        for entry in updates.values() {
            if entry.content.is_none() {
                continue;
            }
            let current = self
                .read(&entry.target, false)
                .await
                .map_err(|message| failure(&completed, "Failed to read", &entry.target.absolute, &message))?;
            let text = if self.deps.format.file(&entry.target).await {
                self.deps.mutation.sync_text_bom(&entry.target, current.bom).await.map_err(|error| {
                    failure(&completed, "Failed to sync", &entry.target.absolute, &error_message(&error))
                })?
            } else {
                current.text
            };
            formatted.insert(access::key(&entry.target), text);
        }

        let files = prepared
            .iter()
            .map(|change| {
                if matches!(change, Prepared::Delete { .. }) {
                    return patch_file(change, None);
                }
                let after = formatted.get(&access::key(change.output_target())).map(String::as_str);
                patch_file(change, after)
            })
            .collect::<Vec<_>>();
        let mut lines = vec!["Success. Updated the following files:".to_owned()];
        lines.extend(files.iter().map(|file| {
            let marker = match file.status {
                FileDiffStatus::Added => "A",
                FileDiffStatus::Deleted => "D",
                FileDiffStatus::Modified => "M",
            };
            format!("{marker} {}", file.file)
        }));
        Ok(ToolOutput {
            output: json!({}),
            content: vec![Content::text(lines.join("\n"))],
            metadata: Some(json!({ "files": files })),
        })
    }

    async fn prepare(
        &self,
        hunk: &Hunk,
        context: &ToolContext,
        prepared: &mut Vec<Prepared>,
        updates: &mut IndexMap<String, UpdateEntry>,
    ) -> Result<(), ToolError> {
        let target = self.resolve_target(hunk.path(), context).await?;
        let key = access::key(&target);
        match hunk {
            Hunk::Add { contents, .. } => {
                let content = if contents.ends_with('\n') || contents.is_empty() {
                    contents.clone()
                } else {
                    format!("{contents}\n")
                };
                let (_, after) = bom::split(&content);
                prepared.push(Prepared::Add {
                    target: target.clone(),
                    content: content.clone(),
                    before: String::new(),
                    after: after.to_owned(),
                });
                updates.insert(key, UpdateEntry { target, content: Some(content) });
            }
            Hunk::Delete { .. } => {
                let previous = updates.get(&key).map(|entry| entry.content.clone());
                if matches!(previous, Some(None)) {
                    return Err(ToolError::new(format!(
                        "patch verification failed: Failed to delete {}: file does not exist",
                        target.resource
                    )));
                }
                let before = match previous.flatten() {
                    Some(previous) => bom::split(&previous).1.to_owned(),
                    None => {
                        self.read(&target, false)
                            .await
                            .map_err(|message| {
                                ToolError::new(format!(
                                    "patch verification failed: Failed to delete {}: {message}",
                                    target.resource
                                ))
                            })?
                            .text
                    }
                };
                prepared.push(Prepared::Delete { target: target.clone(), before, after: String::new() });
                updates.insert(key, UpdateEntry { target, content: None });
            }
            Hunk::Update { path, chunks, move_path } => {
                let previous = updates.get(&key).map(|entry| entry.content.clone());
                if matches!(previous, Some(None)) {
                    return Err(ToolError::new(format!(
                        "patch verification failed: Failed to read file to update {}: file does not exist",
                        target.absolute
                    )));
                }
                let original = match previous.flatten() {
                    Some(previous) => previous,
                    None => {
                        let decoded = self.read(&target, true).await.map_err(|message| {
                            ToolError::new(format!(
                                "patch verification failed: Failed to read file to update {}: {message}",
                                target.absolute
                            ))
                        })?;
                        bom::join(&decoded.text, decoded.bom)
                    }
                };
                let before = bom::split(&original).1.to_owned();
                let update = patch::derive(path, chunks, &original)
                    .map_err(|error| ToolError::new(format!("patch verification failed: {error}")))?;
                let move_target = match move_path {
                    Some(move_path) => {
                        let moved = self.resolve_target(move_path, context).await?;
                        (access::key(&moved) != key).then_some(moved)
                    }
                    None => None,
                };
                let next = bom::join(&update.content, update.bom);
                prepared.push(Prepared::Update {
                    target: target.clone(),
                    content: next.clone(),
                    before,
                    after: update.content,
                    move_target: move_target.clone(),
                });
                match move_target {
                    Some(move_target) => {
                        updates.insert(key, UpdateEntry { target, content: None });
                        updates.insert(
                            access::key(&move_target),
                            UpdateEntry { target: move_target, content: Some(next) },
                        );
                    }
                    None => {
                        updates.insert(key, UpdateEntry { target, content: Some(next) });
                    }
                }
            }
        }
        Ok(())
    }

    async fn apply_change(&self, change: &Prepared, completed: &mut Vec<String>) -> Result<(), ToolError> {
        let target = change.output_target();
        let move_target = change.move_target();
        let recased = match move_target {
            Some(move_target) => {
                move_target.absolute.to_lowercase() == change.target().absolute.to_lowercase()
                    && self.deps.files.kind(&change.target().absolute).await == Some(EntryKind::File)
                    && self.deps.files.kind(&move_target.absolute).await.is_some()
            }
            None => false,
        };
        if matches!(change, Prepared::Delete { .. }) {
            self.deps.files.remove(&change.target().absolute).await.map_err(|error| {
                failed(completed, &format!("Failed to delete {}", change.target().resource), &error)
            })?;
        }
        if let Some(content) = change.content() {
            let written = if recased { change.target() } else { target };
            self.deps
                .files
                .write(written, content.as_bytes())
                .await
                .map_err(|error| failed(completed, &format!("Failed to write {}", target.resource), &error))?;
        }
        if let Some(move_target) = move_target {
            if recased {
                self.deps.files.rename(&change.target().absolute, &move_target.absolute).await.map_err(|error| {
                    failed(completed, &format!("Failed to rename {}", change.target().resource), &error)
                })?;
            } else {
                self.deps.files.remove(&change.target().absolute).await.map_err(|error| {
                    failed(
                        completed,
                        &format!("Wrote {} but failed to remove {}", move_target.resource, change.target().resource),
                        &error,
                    )
                })?;
            }
        }
        completed.push(target.resource.clone());
        Ok(())
    }

    async fn resolve_target(&self, path: &str, context: &ToolContext) -> Result<Target, ToolError> {
        let target = self
            .deps
            .access
            .resolve(ResolveInput { path: path.to_owned(), kind: Some(Kind::File), base: None })
            .await
            .map_err(access_error)?;
        if let Some(external) = &target.external_directory {
            let metadata = JsonMap(vec![
                ("filepath".to_owned(), Value::String(target.absolute.clone())),
                ("parentDir".to_owned(), Value::String(external.directory.clone())),
            ]);
            self.deps
                .access
                .authorize_external(
                    &[&target],
                    &file_invocation(context),
                    Some(metadata),
                    self.deps.authorizer.as_ref(),
                )
                .await
                .map_err(access_error)?;
        }
        Ok(target)
    }

    async fn read(&self, target: &Target, editable: bool) -> Result<DecodedText, String> {
        check_file(&target.absolute).await?;
        let result = if editable {
            self.deps.mutation.read_editable_text(target).await
        } else {
            self.deps.mutation.read_text(target).await
        };
        result.map_err(|error| error_message(&error))
    }
}

impl ToolRuntime for PatchRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

struct UpdateEntry {
    target: Target,
    content: Option<String>,
}

enum Prepared {
    Add { target: Target, content: String, before: String, after: String },
    Delete { target: Target, before: String, after: String },
    Update { target: Target, content: String, before: String, after: String, move_target: Option<Target> },
}

impl Prepared {
    fn target(&self) -> &Target {
        match self {
            Prepared::Add { target, .. } | Prepared::Delete { target, .. } | Prepared::Update { target, .. } => target,
        }
    }

    fn move_target(&self) -> Option<&Target> {
        match self {
            Prepared::Update { move_target, .. } => move_target.as_ref(),
            _ => None,
        }
    }

    fn output_target(&self) -> &Target {
        self.move_target().unwrap_or_else(|| self.target())
    }

    fn targets(&self) -> Vec<&Target> {
        match self.move_target() {
            Some(move_target) => vec![self.target(), move_target],
            None => vec![self.target()],
        }
    }

    fn content(&self) -> Option<&str> {
        match self {
            Prepared::Add { content, .. } | Prepared::Update { content, .. } => Some(content),
            Prepared::Delete { .. } => None,
        }
    }

    fn before(&self) -> &str {
        match self {
            Prepared::Add { before, .. } | Prepared::Delete { before, .. } | Prepared::Update { before, .. } => before,
        }
    }

    fn after(&self) -> &str {
        match self {
            Prepared::Add { after, .. } | Prepared::Delete { after, .. } | Prepared::Update { after, .. } => after,
        }
    }

    fn status(&self) -> FileDiffStatus {
        match self {
            Prepared::Add { .. } => FileDiffStatus::Added,
            Prepared::Delete { .. } => FileDiffStatus::Deleted,
            Prepared::Update { .. } => FileDiffStatus::Modified,
        }
    }
}

fn patch_file(change: &Prepared, after: Option<&str>) -> FileDiff {
    let file = change.output_target().resource.clone();
    let mut diff = diff::file_diff(
        &change.target().absolute,
        change.before(),
        after.unwrap_or_else(|| change.after()),
        change.status(),
    );
    diff.file = file;
    diff.patch = diff::trim_diff(&diff.patch);
    diff
}

fn failure(completed: &[String], operation: &str, target: &str, message: &str) -> ToolError {
    let suffix = if completed.is_empty() {
        String::new()
    } else {
        format!(". Completed before failure: {}", completed.join(", "))
    };
    ToolError::new(format!("{operation} {target}: {message}{suffix}"))
}

fn failed(completed: &[String], message: &str, error: &FsError) -> ToolError {
    let suffix = if completed.is_empty() {
        String::new()
    } else {
        format!(". Completed before failure: {}", completed.join(", "))
    };
    ToolError::new(format!("{message}: {}{suffix}", error_message(error)))
}

fn access_error(error: AccessError) -> ToolError {
    match &error {
        AccessError::Permission(error) => ToolError::new(error.to_string()),
        AccessError::Failed { .. } => ToolError::new(error.to_string()),
    }
}

async fn check_file(target: &str) -> Result<(), String> {
    match tokio::fs::metadata(target).await {
        Ok(metadata) if metadata.is_dir() => Err("path is a directory".to_owned()),
        Ok(metadata) if !metadata.is_file() => Err(format!("path is {}", kind_name(&metadata.file_type()))),
        Ok(_) => Ok(()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Err("file does not exist".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn kind_name(file_type: &std::fs::FileType) -> &'static str {
    if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_file() {
        "file"
    } else if file_type.is_dir() {
        "directory"
    } else {
        "other"
    }
}

fn error_message(error: &FsError) -> String {
    match error {
        FsError::NotFound { .. } | FsError::DirectoryNotFound { .. } => "file does not exist".to_owned(),
        FsError::Failed { cause, .. } => cause.clone(),
        error => error.to_string(),
    }
}

fn io_error(path: &str, error: &std::io::Error) -> FsError {
    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
        FsError::NotFound { path: path.to_owned() }
    } else {
        FsError::Failed { path: path.to_owned(), cause: error.to_string() }
    }
}
