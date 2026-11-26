use std::path::{Path, PathBuf};

use opencode_util::path::Flavor;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReference {
    pub host: String,
    pub path: String,
    pub segments: Vec<String>,
    pub owner: Option<String>,
    pub repo: String,
    pub remote: String,
    pub label: String,
    pub protocol: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReference {
    pub path: String,
    pub segments: Vec<String>,
    pub repo: String,
    pub remote: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    Remote(RemoteReference),
    File(FileReference),
}

#[derive(Debug, thiserror::Error)]
#[error("invalid repository reference {repository}: {message}")]
pub struct InvalidReferenceError {
    pub repository: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported local repository {repository}: {message}")]
pub struct UnsupportedLocalRepositoryError {
    pub repository: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid branch {branch}: {message}")]
pub struct InvalidBranchError {
    pub branch: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseRemoteError {
    #[error(transparent)]
    Invalid(#[from] InvalidReferenceError),
    #[error(transparent)]
    UnsupportedLocal(#[from] UnsupportedLocalRepositoryError),
}

pub fn parse(input: &str) -> Option<Reference> {
    let cleaned = normalize_input(input);
    if cleaned.is_empty() {
        return None;
    }

    if let Some(rest) = cleaned.strip_prefix("github:")
        && let Some((owner, repo)) = split_github_shorthand(rest)
    {
        return build_remote(RemoteInput {
            host: "github.com".to_string(),
            segments: vec![owner.to_string(), repo.to_string()],
            remote: None,
            protocol: None,
        });
    }

    if !cleaned.contains("://") {
        if let Some((host, path)) = split_scp(&cleaned) {
            return build_remote(RemoteInput {
                host: host.to_string(),
                segments: parts(path),
                remote: Some(cleaned.clone()),
                protocol: None,
            });
        }

        let direct = parts(&cleaned);
        if direct.len() >= 2 && host_like(&direct[0]) {
            return build_remote(RemoteInput {
                host: direct[0].clone(),
                segments: direct[1..].to_vec(),
                remote: None,
                protocol: None,
            });
        }
        if direct.len() == 2 {
            return build_remote(RemoteInput {
                host: "github.com".to_string(),
                segments: direct,
                remote: None,
                protocol: None,
            });
        }
    }

    let url = Url::parse(&cleaned).ok()?;
    if url.scheme() == "file" {
        return build_file(&url, &cleaned).map(Reference::File);
    }
    let host = url_host(&url)?;
    let segments = parts(url.path());
    let remote = if host == "github.com" { github_remote(&segments.join("/")) } else { cleaned.clone() };
    build_remote(RemoteInput { host, segments, remote: Some(remote), protocol: Some(format!("{}:", url.scheme())) })
}

pub fn parse_remote(input: &str) -> Result<RemoteReference, ParseRemoteError> {
    let Some(reference) = parse(input) else {
        return Err(InvalidReferenceError {
            repository: input.to_string(),
            message: "Repository must be a git URL, host/path reference, or GitHub owner/repo shorthand".to_string(),
        }
        .into());
    };
    let Reference::Remote(reference) = reference else {
        return Err(UnsupportedLocalRepositoryError {
            repository: input.to_string(),
            message: "Local file repositories are not supported".to_string(),
        }
        .into());
    };
    Ok(reference)
}

pub fn validate_branch(branch: &str) -> Result<(), InvalidBranchError> {
    let valid = !branch.is_empty()
        && branch.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
        && !branch.starts_with('-')
        && !branch.contains("..");
    if valid {
        return Ok(());
    }
    Err(InvalidBranchError {
        branch: branch.to_string(),
        message:
            "Branch must contain only alphanumeric characters, /, _, ., and -, and cannot start with - or contain .."
                .to_string(),
    })
}

pub fn is_remote(reference: &Reference) -> bool {
    matches!(reference, Reference::Remote(_))
}

pub fn cache_path(root: &Path, reference: &Reference, branch: Option<&str>) -> PathBuf {
    let (host, segments) = match reference {
        Reference::Remote(reference) => (reference.host.as_str(), &reference.segments),
        Reference::File(reference) => ("file", &reference.segments),
    };
    let root = root.to_string_lossy();
    let mut parts: Vec<&str> = Vec::new();
    parts.push(&root);
    parts.extend(host.split(':'));
    parts.extend(segments.iter().map(String::as_str));
    let base = Flavor::NATIVE.join(&parts);
    match branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => PathBuf::from(format!("{base}@{}", encode_uri_component(branch))),
        None => PathBuf::from(base),
    }
}

pub fn cache_identity(reference: &Reference) -> String {
    let (host, path) = match reference {
        Reference::Remote(reference) => (reference.host.as_str(), reference.path.as_str()),
        Reference::File(reference) => ("file", reference.path.as_str()),
    };
    format!("{host}/{path}")
}

pub fn same(left: &Reference, right: &Reference) -> bool {
    cache_identity(left) == cache_identity(right)
}

struct RemoteInput {
    host: String,
    segments: Vec<String>,
    remote: Option<String>,
    protocol: Option<String>,
}

fn build_remote(input: RemoteInput) -> Option<Reference> {
    let segments: Vec<String> =
        input.segments.iter().map(|segment| trim_git_suffix(segment)).filter(|segment| !segment.is_empty()).collect();
    if !safe_host(&input.host) || segments.is_empty() || segments.iter().any(|segment| !safe_segment(segment)) {
        return None;
    }
    let path = segments.join("/");
    let host = input.host.to_lowercase();
    let remote = input.remote.unwrap_or_else(|| {
        if host == "github.com" { github_remote(&path) } else { format!("https://{host}/{path}.git") }
    });
    let label = if host == "github.com" && segments.len() == 2 { path.clone() } else { format!("{host}/{path}") };
    Some(Reference::Remote(RemoteReference {
        host,
        path,
        owner: if segments.len() == 2 { Some(segments[0].clone()) } else { None },
        repo: segments[segments.len() - 1].clone(),
        remote,
        label,
        protocol: input.protocol,
        segments,
    }))
}

fn build_file(url: &Url, remote: &str) -> Option<FileReference> {
    let file_path = Flavor::NATIVE.normalize(&url.to_file_path().ok()?.to_string_lossy());
    let segments: Vec<String> = file_path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.strip_suffix(':').unwrap_or(segment).to_string())
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(FileReference {
        path: file_path.clone(),
        repo: trim_git_suffix(&segments[segments.len() - 1]),
        segments,
        remote: remote.to_string(),
        label: file_path,
    })
}

fn normalize_input(input: &str) -> String {
    let trimmed = input.trim_matches(is_js_whitespace);
    let without_prefix = trimmed.strip_prefix("git+").unwrap_or(trimmed);
    let without_fragment = without_prefix.split('#').next().unwrap_or_default();
    without_fragment.trim_end_matches('/').to_string()
}

fn trim_git_suffix(input: &str) -> String {
    input.strip_suffix(".git").unwrap_or(input).to_string()
}

fn parts(input: &str) -> Vec<String> {
    input
        .split('/')
        .map(|item| trim_git_suffix(item.trim_matches(is_js_whitespace)))
        .filter(|item| !item.is_empty())
        .collect()
}

fn safe_host(input: &str) -> bool {
    !input.is_empty() && !input.starts_with('-') && input.split(':').all(safe_segment)
}

fn safe_segment(input: &str) -> bool {
    input != "."
        && input != ".."
        && !input.contains(':')
        && !input.chars().any(|c| c == '/' || c == '\\' || is_js_whitespace(c))
}

fn host_like(input: &str) -> bool {
    input.contains('.') || input.contains(':') || input == "localhost"
}

fn github_remote(path: &str) -> String {
    format!("https://github.com/{path}.git")
}

fn split_github_shorthand(input: &str) -> Option<(&str, &str)> {
    let (owner, repo) = input.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    if owner.chars().any(is_js_whitespace) || repo.chars().any(is_js_whitespace) {
        return None;
    }
    Some((owner, repo))
}

fn split_scp(input: &str) -> Option<(&str, &str)> {
    if let Some(split) = split_user_scp(input) {
        return Some(split);
    }
    let (host, rest) = input.split_once(':')?;
    if !safe_scp_host(host) || !safe_scp_path(rest) {
        return None;
    }
    Some((host, rest))
}

fn split_user_scp(input: &str) -> Option<(&str, &str)> {
    let (user, after) = input.split_once('@')?;
    if user.is_empty() || user.contains('/') || user.chars().any(is_js_whitespace) {
        return None;
    }
    let (host, rest) = after.split_once(':')?;
    if !safe_scp_host(host) || !safe_scp_path(rest) {
        return None;
    }
    Some((host, rest))
}

fn safe_scp_host(host: &str) -> bool {
    !host.is_empty() && !host.contains(':') && !host.contains('/') && !host.chars().any(is_js_whitespace)
}

fn safe_scp_path(rest: &str) -> bool {
    !rest.is_empty() && !rest.chars().any(is_line_terminator)
}

fn url_host(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}
