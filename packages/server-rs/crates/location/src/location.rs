use std::sync::Arc;

use crate::environment::{Environment, FileType, type_following};
use crate::project::{ProjectError, ProjectService, ResolveOptions, Vcs};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocationRef {
    pub directory: String,
    pub workspace_id: Option<String>,
}

impl LocationRef {
    pub fn new(directory: impl Into<String>, workspace_id: Option<String>) -> Self {
        LocationRef { directory: directory.into(), workspace_id }
    }

    pub fn canonical(&self) -> LocationRef {
        let windows = cfg!(windows) && !self.directory.starts_with('/');
        let normalized = match windows {
            true => normalize_windows(&self.directory),
            false => normalize_posix(&self.directory),
        };
        let (root_length, separator) = match windows {
            true => (split_windows_root(&normalized).0.len(), '\\'),
            false => (posix_root(&normalized).len(), '/'),
        };
        let directory = strip_trailing_separator(normalized, root_length, separator);
        LocationRef { directory, workspace_id: self.workspace_id.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationProject {
    pub id: String,
    pub directory: String,
    pub canonical: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocationInfo {
    pub directory: String,
    pub workspace_id: Option<String>,
    pub project: LocationProject,
    pub vcs: Option<Vcs>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum BootError {
    #[error("Location not found: {0}")]
    DirectoryNotFound(String),
    #[error("Location service is closed")]
    Closed,
    #[error("Location boot was invalidated")]
    Invalidated,
    #[error("location boot failed")]
    Panicked,
    #[error(transparent)]
    Project(Arc<ProjectError>),
}

impl From<ProjectError> for BootError {
    fn from(error: ProjectError) -> Self {
        BootError::Project(Arc::new(error))
    }
}

pub async fn boot_location(
    reference: &LocationRef,
    projects: &ProjectService,
    environment: &dyn Environment,
) -> Result<LocationInfo, BootError> {
    let is_dir = reference.workspace_id.is_none()
        && matches!(type_following(environment.files(), &reference.directory).await, Ok(FileType::Directory));
    let persist = reference.workspace_id.is_some() || is_dir;
    let resolved = projects.resolve(environment, &reference.directory, ResolveOptions { persist }).await?;
    if reference.workspace_id.is_none() && !is_dir {
        return Err(BootError::DirectoryNotFound(reference.directory.clone()));
    }
    Ok(LocationInfo {
        directory: reference.directory.clone(),
        workspace_id: reference.workspace_id.clone(),
        project: LocationProject { id: resolved.id, directory: resolved.directory, canonical: resolved.canonical },
        vcs: resolved.vcs,
    })
}

pub fn normalize_posix(input: &str) -> String {
    if input.is_empty() {
        return ".".to_string();
    }
    let absolute = input.starts_with('/');
    let trailing = input.ends_with('/');
    let segments = resolve_segments(input, '/', absolute);
    let mut normalized = segments.join("/");
    if absolute {
        normalized.insert(0, '/');
    }
    if normalized.is_empty() {
        normalized.push('.');
    }
    if trailing && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized
}

pub fn normalize_windows(input: &str) -> String {
    if input.is_empty() {
        return ".".to_string();
    }
    let replaced = input.replace('/', "\\");
    let trailing = replaced.len() > 1 && replaced.ends_with('\\');
    let (root, rest) = split_windows_root(&replaced);
    let absolute = root.ends_with('\\');
    let segments = resolve_segments(rest, '\\', absolute);
    let mut normalized = root;
    normalized.push_str(&segments.join("\\"));
    if normalized.is_empty() {
        normalized.push('.');
    }
    if normalized.ends_with(':') {
        normalized.push('.');
    }
    if trailing && !normalized.ends_with('\\') {
        normalized.push('\\');
    }
    normalized
}

fn strip_trailing_separator(normalized: String, root_length: usize, separator: char) -> String {
    if normalized.len() > root_length {
        return normalized.strip_suffix(separator).unwrap_or(&normalized).to_string();
    }
    normalized
}

fn resolve_segments<'a>(input: &'a str, separator: char, absolute: bool) -> Vec<&'a str> {
    let mut segments: Vec<&'a str> = Vec::new();
    for segment in input.split(separator) {
        match segment {
            "" | "." => {}
            ".." => match segments.last() {
                Some(&last) if last != ".." => {
                    segments.pop();
                }
                _ if absolute => {}
                _ => segments.push(".."),
            },
            other => segments.push(other),
        }
    }
    segments
}

fn posix_root(normalized: &str) -> &str {
    if normalized.starts_with('/') {
        return "/";
    }
    ""
}

fn split_windows_root(input: &str) -> (String, &str) {
    if let Some((root, remainder)) = unc_root(input) {
        return (root, remainder);
    }
    let bytes = input.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() >= 3 && bytes[2] == b'\\' {
            return (input[..3].to_string(), &input[3..]);
        }
        return (input[..2].to_string(), &input[2..]);
    }
    if bytes.first().is_some_and(|byte| *byte == b'\\') {
        return ("\\".to_string(), &input[1..]);
    }
    (String::new(), input)
}

fn unc_root(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix("\\\\")?;
    let (server, after_server) = rest.split_once('\\')?;
    if server.is_empty() {
        return None;
    }
    let share_length = after_server.find('\\').unwrap_or(after_server.len());
    let share = &after_server[..share_length];
    if share.is_empty() {
        return None;
    }
    Some((format!("\\\\{server}\\{share}\\"), &after_server[share_length..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_posix_collapses_separators_and_preserves_trailing_ones() {
        assert_eq!(normalize_posix(""), ".");
        assert_eq!(normalize_posix("/"), "/");
        assert_eq!(normalize_posix("//"), "/");
        assert_eq!(normalize_posix("/a//b/"), "/a/b/");
        assert_eq!(normalize_posix("a/b/"), "a/b/");
        assert_eq!(normalize_posix("./"), "./");
    }

    #[test]
    fn normalize_posix_resolves_dots_at_and_above_the_root() {
        assert_eq!(normalize_posix("/a/./child/../.."), "/");
        assert_eq!(normalize_posix("/a/./child/.."), "/a");
        assert_eq!(normalize_posix("/a/b/../../.."), "/");
        assert_eq!(normalize_posix("a/../../b"), "../b");
        assert_eq!(normalize_posix("a/.."), ".");
    }

    #[test]
    fn canonical_strips_one_trailing_separator() {
        assert_eq!(LocationRef::new("/a//b/", None).canonical().directory, "/a/b");
        assert_eq!(LocationRef::new("/", None).canonical().directory, "/");
        assert_eq!(LocationRef::new("", None).canonical().directory, ".");
    }

    #[test]
    fn canonical_copies_the_workspace_id() {
        let canonical = LocationRef::new("/a/b/", Some("workspace".to_string())).canonical();
        assert_eq!(canonical.directory, "/a/b");
        assert_eq!(canonical.workspace_id, Some("workspace".to_string()));
    }

    #[test]
    fn normalize_windows_resolves_segments() {
        assert_eq!(normalize_windows("C:\\a\\..\\b"), "C:\\b");
        assert_eq!(normalize_windows("C:/a/b/"), "C:\\a\\b\\");
        assert_eq!(normalize_windows("\\\\server\\share\\a\\..\\b"), "\\\\server\\share\\b");
    }

    #[test]
    fn normalize_windows_keeps_roots() {
        assert_eq!(normalize_windows(""), ".");
        assert_eq!(normalize_windows("C:\\"), "C:\\");
        assert_eq!(normalize_windows("\\"), "\\");
        assert_eq!(normalize_windows("\\a\\.."), "\\");
        assert_eq!(normalize_windows("\\\\server\\share"), "\\\\server\\share\\");
    }

    #[test]
    fn normalize_windows_keeps_drive_relative_parents() {
        assert_eq!(normalize_windows("C:"), "C:.");
        assert_eq!(normalize_windows("C:..\\a"), "C:..\\a");
        assert_eq!(normalize_windows("a\\\\b"), "a\\b");
    }

    #[test]
    fn normalize_windows_preserves_a_trailing_separator() {
        assert_eq!(normalize_windows("a\\..\\"), ".\\");
    }

    #[test]
    fn canonical_strips_win32_trailing_separators_above_the_root() {
        let normalized = normalize_windows("C:/a/b/");
        let root_length = split_windows_root(&normalized).0.len();
        assert_eq!(strip_trailing_separator(normalized, root_length, '\\'), "C:\\a\\b");

        let drive = normalize_windows("C:\\");
        let root_length = split_windows_root(&drive).0.len();
        assert_eq!(strip_trailing_separator(drive, root_length, '\\'), "C:\\");

        let share = normalize_windows("\\\\server\\share");
        let root_length = split_windows_root(&share).0.len();
        assert_eq!(strip_trailing_separator(share, root_length, '\\'), "\\\\server\\share\\");
    }
}
