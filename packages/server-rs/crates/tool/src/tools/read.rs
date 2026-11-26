use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::future::BoxFuture;
use opencode_fs::access::{Access, AccessError, ResolveInput, Target};
use opencode_fs::path;
use opencode_registry::instructions::discovery;
use opencode_registry::skill::{self, SkillService};
use opencode_util::path::Flavor;
use serde_json::{Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation, permission_input};
use crate::tools::read_filesystem::{self, DirEntry, FileKind, PageInput, ReadError, ReadFileError, ReadOutput};

#[derive(Clone)]
pub struct ReadDeps {
    pub files: Arc<dyn read_filesystem::Files>,
    pub access: Access,
    pub skills: Option<SkillService>,
    pub instructions: Arc<dyn NearbyInstructions>,
    pub claude: bool,
}

pub trait NearbyInstructions: Send + Sync {
    fn load<'a>(&'a self, session_id: &'a str, paths: Vec<String>) -> BoxFuture<'a, ()>;
}

pub struct ReadRuntime {
    deps: ReadDeps,
}

impl ReadRuntime {
    pub fn new(deps: ReadDeps) -> Self {
        ReadRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let file = input.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
        let page = PageInput {
            offset: input.get("offset").and_then(Value::as_i64),
            limit: input.get("limit").and_then(Value::as_i64),
        };
        if page.limit == Some(0) {
            return Err(ToolError::new("Limit must be a positive integer"));
        }
        if skill::is_uri(&file) {
            return self.read_skill(&file, &page, context).await;
        }
        let target = self.authorize_read(&file, context, None).await?;
        let read = read_target(self.deps.files.as_ref(), &target, &page).await;
        let (output, read_target, display) = match read {
            Ok(output) => (output, target, file),
            Err(ReadError::File(ReadFileError::NotFound { .. })) => {
                let alternate = alternate_path(self.deps.files.as_ref(), &target).await;
                let Some(alternate) = alternate else {
                    return Err(self.missing(&file, &target, context).await);
                };
                let sibling = self.authorize_read(&alternate, context, Some(&target)).await?;
                match read_target(self.deps.files.as_ref(), &sibling, &page).await {
                    Ok(output) if !matches!(output, ReadOutput::Directory(_)) => {
                        let display =
                            Flavor::NATIVE.join(&[&path::dirname(&file), Flavor::NATIVE.basename(&alternate)]);
                        (output, sibling, display)
                    }
                    Err(ReadError::File(ReadFileError::NotFound { .. })) => {
                        return Err(self.missing(&file, &target, context).await);
                    }
                    Ok(_) => return Err(self.missing(&file, &target, context).await),
                    Err(error) => return Err(read_error(error, &file)),
                }
            }
            Err(error) => return Err(read_error(error, &file)),
        };
        self.nearby(&read_target, &output, &context.session_id).await;
        Ok(output_value(&display, page.offset, &output))
    }

    async fn read_skill(&self, file: &str, page: &PageInput, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let not_found = || ToolError::new(format!("File not found: {file}"));
        let Some(parsed) = parse_skill_uri(file) else {
            return Err(not_found());
        };
        let Some(skills) = &self.deps.skills else {
            return Err(not_found());
        };
        let found = skills.get(&parsed.id).await.map_err(|_| not_found())?;
        let Some(skill) = found.filter(|skill| skill::is_uri(&skill.path.0)) else {
            return Err(not_found());
        };
        let extra = skills.files(&parsed.id).await.map_err(|_| not_found())?.unwrap_or_default();
        context
            .authorizer
            .assert(permission_input(context, "skill", vec![skill.id.0.clone()], vec![skill.id.0.clone()]))
            .await
            .map_err(|error| ToolError::permission(&error))?;
        context
            .authorizer
            .assert(permission_input(context, "read", vec![parsed.uri.clone()], vec!["*".to_owned()]))
            .await
            .map_err(|error| ToolError::permission(&error))?;
        let files = VirtualFiles::new(&skill.content, &extra);
        let output = read_filesystem::read(&files, &parsed.target, file, page).await.map_err(|error| match error {
            ReadError::File(ReadFileError::NotFound { .. }) => not_found(),
            error => read_error(error, file),
        })?;
        Ok(output_value(file, page.offset, &output))
    }

    async fn authorize_read(
        &self,
        file: &str,
        context: &ToolContext,
        sibling: Option<&Target>,
    ) -> Result<Target, ToolError> {
        self.deps
            .access
            .authorize_read(file, &file_invocation(context), sibling, context.authorizer.as_ref())
            .await
            .map_err(|error| access_error(error, file))
    }

    async fn missing(&self, file: &str, target: &Target, context: &ToolContext) -> ToolError {
        let files = self.deps.files.as_ref();
        let moved = self.reroot(file, target, context).await.unwrap_or_default();
        let folder = path::dirname(&target.absolute);
        let found = listing(files, &folder).await;
        if found.is_missing() {
            return missing_folder(files, file, &folder, &moved).await;
        }
        let mut suggestions = moved;
        if let Listing::Found(names) = &found {
            let requested = Flavor::NATIVE.basename(file);
            suggestions.extend(
                similar(names, requested).into_iter().map(|entry| Flavor::NATIVE.join(&[&path::dirname(file), &entry])),
            );
        }
        ToolError::new(format!("File not found: {file}{}", did_you_mean(&suggestions)))
    }

    async fn reroot(&self, file: &str, miss: &Target, context: &ToolContext) -> Option<Vec<String>> {
        let root = self.deps.access.working_directory().to_owned();
        if path::contains(&root, &miss.absolute) || !path::contains(&path::dirname(&root), &miss.absolute) {
            return None;
        }
        let relative = Flavor::NATIVE.relative(&path::dirname(&root), &miss.absolute);
        let target = self
            .deps
            .access
            .resolve(ResolveInput { path: Flavor::NATIVE.join(&[&root, &relative]), ..ResolveInput::default() })
            .await
            .ok()?;
        if tokio::fs::metadata(&target.absolute).await.is_err() {
            return None;
        }
        let readable = context
            .authorizer
            .allows(permission_input(context, "read", vec![target.resource.clone()], vec!["*".to_owned()]))
            .await
            .ok()?;
        if !readable {
            return None;
        }
        if Flavor::NATIVE.is_absolute(file) || file.starts_with("~/") {
            Some(vec![target.absolute])
        } else {
            Some(vec![target.resource])
        }
    }

    async fn nearby(&self, target: &Target, output: &ReadOutput, session_id: &str) {
        if target.external_directory.is_some() {
            return;
        }
        let resolved = path::resolve(&target.absolute);
        let root = path::resolve(self.deps.access.working_directory());
        let start = match output {
            ReadOutput::Directory(_) => resolved.clone(),
            _ => path::dirname(&resolved),
        };
        let names = discovery::filenames(self.deps.claude);
        let mut discovered = Vec::new();
        let mut current = start;
        loop {
            for name in &names {
                let candidate = Flavor::NATIVE.join(&[&current, *name]);
                if tokio::fs::metadata(&candidate).await.is_ok_and(|metadata| metadata.is_file()) {
                    discovered.push(std::path::PathBuf::from(candidate));
                }
            }
            if same_directory(&root, &current) {
                break;
            }
            let parent = path::dirname(&current);
            if parent == current {
                break;
            }
            current = parent;
        }
        let candidates: Vec<String> = discovery::prefer_agents(&discovered)
            .into_iter()
            .map(|file| path::resolve(&file.to_string_lossy()))
            .filter(|file| !path::contains(&path::dirname(file), &root) && *file != resolved)
            .collect();
        if candidates.is_empty() {
            return;
        }
        self.deps.instructions.load(session_id, candidates).await;
    }
}

impl ToolRuntime for ReadRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

async fn read_target(
    files: &dyn read_filesystem::Files,
    target: &Target,
    page: &PageInput,
) -> Result<ReadOutput, ReadError> {
    read_filesystem::read(files, &target.absolute, &target.resource, page).await
}

fn access_error(error: AccessError, file: &str) -> ToolError {
    match error {
        AccessError::Permission(error) => ToolError::permission(error),
        AccessError::Failed { .. } => ToolError::new(format!("Unable to read {file}")),
    }
}

fn read_error(error: ReadError, file: &str) -> ToolError {
    match error {
        ReadError::Binary { .. }
        | ReadError::MediaIngestLimit { .. }
        | ReadError::PathKind { .. }
        | ReadError::DirectoryOffset { .. } => ToolError::new(error.to_string()),
        ReadError::File(_) => ToolError::new(format!("Unable to read {file}")),
    }
}

async fn alternate_path(files: &dyn read_filesystem::Files, target: &Target) -> Option<String> {
    let base = canonical(Flavor::NATIVE.basename(&target.absolute));
    let parent = path::dirname(&target.absolute);
    let entries = files.list(&parent).await.ok()?;
    let matches: Vec<&DirEntry> =
        entries.iter().filter(|entry| entry.kind == FileKind::File && canonical(&entry.name) == base).collect();
    if matches.len() != 1 {
        return None;
    }
    Some(Flavor::NATIVE.join(&[&parent, &matches[0].name]))
}

enum Listing {
    Found(Vec<String>),
    Missing,
    Unknown,
}

impl Listing {
    fn is_missing(&self) -> bool {
        matches!(self, Listing::Missing)
    }
}

async fn listing(files: &dyn read_filesystem::Files, directory: &str) -> Listing {
    match files.list(directory).await {
        Ok(entries) => Listing::Found(entries.into_iter().map(|entry| entry.name).collect()),
        Err(ReadFileError::NotFound { .. }) => Listing::Missing,
        Err(_) => Listing::Unknown,
    }
}

async fn missing_folder(files: &dyn read_filesystem::Files, file: &str, folder: &str, moved: &[String]) -> ToolError {
    let mut child = folder.to_owned();
    let mut parent = path::dirname(folder);
    while parent != child {
        match listing(files, &parent).await {
            Listing::Unknown => break,
            Listing::Found(names) => {
                let mut suggestions = moved.to_vec();
                suggestions.extend(
                    similar(&names, Flavor::NATIVE.basename(&child))
                        .into_iter()
                        .map(|entry| Flavor::NATIVE.join(&[&parent, &entry])),
                );
                let message = [
                    format!("File not found: {file}"),
                    format!("The folder {child} does not exist. The nearest folder that does is {parent}."),
                ]
                .into_iter()
                .chain(did_you_mean_lines(&suggestions))
                .collect::<Vec<_>>()
                .join("\n\n");
                return ToolError::new(message);
            }
            Listing::Missing => {}
        }
        child = parent;
        parent = path::dirname(&child);
    }
    ToolError::new(format!("File not found: {file}{}", did_you_mean(moved)))
}

fn did_you_mean(suggestions: &[String]) -> String {
    did_you_mean_lines(suggestions).map_or_else(String::new, |line| format!("\n\n{line}"))
}

fn did_you_mean_lines(suggestions: &[String]) -> Option<String> {
    if suggestions.is_empty() {
        return None;
    }
    Some(format!("Did you mean one of these?\n{}", suggestions.iter().take(3).cloned().collect::<Vec<_>>().join("\n")))
}

fn similar(names: &[String], requested: &str) -> Vec<String> {
    let name = requested.to_lowercase();
    let wanted = stem(&name);
    let wanted_units = wanted.encode_utf16().count();
    let typos = if wanted_units < 3 {
        0
    } else if wanted_units < 5 {
        1
    } else {
        2
    };
    let tier = |candidate: &str| -> Option<u8> {
        if candidate == name {
            return Some(1);
        }
        if canonical(candidate) == canonical(&name) {
            return None;
        }
        let found = stem(candidate);
        if found == wanted {
            return Some(2);
        }
        let found_units = found.encode_utf16().count();
        if found_units.min(wanted_units) >= 3
            && (candidate.contains(&name)
                || name.contains(candidate)
                || found.contains(&wanted)
                || wanted.contains(&found))
        {
            return Some(3);
        }
        if typos > 0
            && found_units.abs_diff(wanted_units) <= typos as usize
            && edit_distance(&found, &wanted) as i64 <= typos
        {
            return Some(4);
        }
        None
    };
    let mut matched: Vec<(String, u8, usize)> = names
        .iter()
        .filter(|entry| entry.as_str() != requested)
        .flat_map(|entry| {
            let lower = entry.to_lowercase();
            tier(&lower).map(|rank| (entry.clone(), rank, edit_distance(&lower, &name)))
        })
        .collect();
    matched.sort_by(|left, right| {
        left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)).then_with(|| left.0.cmp(&right.0))
    });
    matched.into_iter().take(3).map(|entry| entry.0).collect()
}

fn canonical(name: &str) -> String {
    name.replace(['\u{00a0}', '\u{202f}'], " ").replace(['\u{2018}', '\u{2019}'], "'")
}

fn stem(name: &str) -> String {
    let extension = opencode_fs::mime::extname(name);
    let stem = &name[..name.len() - extension.len()];
    if stem.is_empty() { name.to_owned() } else { stem.to_owned() }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for i in 1..=left.len() {
        let mut current = vec![i; right.len() + 1];
        for j in 1..=right.len() {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + usize::from(left[i - 1] != right[j - 1]));
        }
        previous = current;
    }
    previous[right.len()]
}

struct ParsedSkill {
    id: String,
    target: String,
    uri: String,
}

fn parse_skill_uri(value: &str) -> Option<ParsedSkill> {
    let trimmed = value.strip_prefix("skill://")?.trim_end_matches('/');
    let (id, rest) = match trimmed.split_once('/') {
        Some((id, rest)) => (id, rest),
        None => (trimmed, ""),
    };
    if !skill::discovery::is_safe_segment(id) {
        return None;
    }
    if !rest.is_empty() && !rest.split('/').all(skill::discovery::is_safe_segment) {
        return None;
    }
    Some(ParsedSkill {
        id: id.to_owned(),
        target: if rest.is_empty() { "/".to_owned() } else { format!("/{rest}") },
        uri: skill::uri(id, if rest.is_empty() { None } else { Some(rest) }),
    })
}

struct VirtualFiles {
    entries: indexmap::IndexMap<String, Vec<u8>>,
}

impl VirtualFiles {
    fn new(content: &str, extra: &indexmap::IndexMap<String, String>) -> Self {
        let mut entries = indexmap::IndexMap::new();
        entries.insert("/SKILL.md".to_owned(), content.as_bytes().to_vec());
        for (relative, text) in extra {
            entries.insert(format!("/{relative}"), text.as_bytes().to_vec());
        }
        VirtualFiles { entries }
    }
}

impl read_filesystem::Files for VirtualFiles {
    fn read<'a>(
        &'a self,
        path: &'a str,
        range: Option<read_filesystem::ReadRange>,
    ) -> BoxFuture<'a, Result<read_filesystem::ReadBytes, ReadFileError>> {
        Box::pin(async move {
            let Some(bytes) = self.entries.get(path) else {
                if self.is_directory(path) {
                    return Err(ReadFileError::WrongKind { path: path.to_owned(), actual: FileKind::Directory });
                }
                return Err(ReadFileError::NotFound { path: path.to_owned() });
            };
            let selected = match range {
                Some(range) => {
                    let start = (range.offset as usize).min(bytes.len());
                    let end = start.saturating_add(range.length as usize).min(bytes.len());
                    bytes[start..end].to_vec()
                }
                None => bytes.clone(),
            };
            Ok(read_filesystem::ReadBytes { bytes: selected, size: bytes.len() as u64 })
        })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, ReadFileError>> {
        Box::pin(async move {
            if !self.is_directory(path) {
                return Err(ReadFileError::NotFound { path: path.to_owned() });
            }
            let prefix = if path == "/" { "/".to_owned() } else { format!("{}/", path.trim_end_matches('/')) };
            let mut entries = indexmap::IndexMap::new();
            for key in self.entries.keys() {
                let Some(rest) = key.strip_prefix(&prefix) else { continue };
                let (name, nested) = match rest.split_once('/') {
                    Some((name, _)) => (name, true),
                    None => (rest, false),
                };
                let kind = if nested { FileKind::Directory } else { FileKind::File };
                entries.entry(name.to_owned()).or_insert(kind);
            }
            Ok(entries.into_iter().map(|(name, kind)| DirEntry { name, kind }).collect())
        })
    }
}

impl VirtualFiles {
    fn is_directory(&self, path: &str) -> bool {
        if path == "/" {
            return true;
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        self.entries.keys().any(|key| key.starts_with(&prefix))
    }
}

fn output_value(display: &str, offset: Option<i64>, output: &ReadOutput) -> ToolOutput {
    let content = to_model_content(display, offset, output);
    let truncated = !matches!(output, ReadOutput::Media(_))
        && match output {
            ReadOutput::Text(text) => text.next.is_some(),
            ReadOutput::Directory(directory) => directory.next.is_some(),
            ReadOutput::Media(_) => false,
        };
    ToolOutput { output: output.to_value(), content, metadata: Some(json!({ "truncated": truncated })) }
}

pub fn to_model_content(path: &str, offset: Option<i64>, output: &ReadOutput) -> Vec<Content> {
    match output {
        ReadOutput::Media(media) => vec![
            Content::text(if media.mime == "application/pdf" {
                "PDF read successfully"
            } else {
                "Image read successfully"
            }),
            Content::File {
                uri: format!("data:{};base64,{}", media.mime, BASE64.encode(&media.bytes)),
                mime: media.mime.clone(),
                name: Some(path.to_owned()),
            },
        ],
        ReadOutput::Text(text) => vec![Content::text(text_model(path, offset, text))],
        ReadOutput::Directory(directory) => vec![Content::text(directory_model(path, offset, directory))],
    }
}

fn text_model(path: &str, offset: Option<i64>, output: &read_filesystem::TextContent) -> String {
    let start = output.offset;
    let lines: Vec<&str> =
        if output.line_count == 0 { Vec::new() } else { output.content.split('\n').take(output.line_count).collect() };
    let total = start + lines.len() as u64 - 1;
    let past_end = offset.is_some_and(|offset| (start as i64) < offset);
    let count = if total == 1 { "line" } else { "lines" };
    let mut content = vec![
        if lines.is_empty() {
            format!("Read file {path}, 0 lines")
        } else {
            format!("Read file {path}, lines {start}-{total}")
        } + &if past_end {
            format!(" (offset {} is past the end; the file has {total} {count})", offset.unwrap_or_default())
        } else {
            String::new()
        },
    ];
    for (index, line) in lines.iter().enumerate() {
        content.push(format!("{}: {line}", start + index as u64));
    }
    if let Some(next) = output.next {
        content.push(format!("[Output truncated. Continue reading with offset: {next}]"));
    }
    content.join("\n")
}

fn directory_model(path: &str, offset: Option<i64>, output: &read_filesystem::DirectoryContent) -> String {
    let start = match offset {
        Some(0) | None => 1,
        Some(offset) => offset,
    };
    let mut content = vec![if output.entries.is_empty() {
        format!("Read directory {path}, 0 entries")
    } else {
        format!("Read directory {path}, entries {start}-{}", start + output.entries.len() as i64 - 1)
    }];
    content.extend(output.entries.iter().cloned());
    if let Some(next) = output.next {
        content.push(format!("[Output truncated. Continue reading with offset: {next}]"));
    }
    content.join("\n")
}

fn same_directory(left: &str, right: &str) -> bool {
    Flavor::NATIVE.normalize(left) == Flavor::NATIVE.normalize(right)
}
