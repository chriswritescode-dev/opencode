use std::ffi::OsString;
use std::sync::Arc;

use opencode_api_types::js::encode_component;
use opencode_util::path::Flavor;
use url::Url;

const INVALID_REFERENCE_MESSAGE: &str =
    "Repository must be a git URL, host/path reference, or GitHub owner/repo shorthand";
const UNSUPPORTED_LOCAL_MESSAGE: &str = "Local file repositories are not supported";
const INVALID_BRANCH_MESSAGE: &str =
    "Branch must contain only alphanumeric characters, /, _, ., and -, and cannot start with - or contain ..";

pub type Vars = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

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
pub enum Reference {
    Remote(RemoteReference),
    File(FileReference),
}

impl Reference {
    pub fn host(&self) -> &str {
        match self {
            Reference::Remote(reference) => &reference.host,
            Reference::File(reference) => &reference.host,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Reference::Remote(reference) => &reference.path,
            Reference::File(reference) => &reference.path,
        }
    }

    pub fn segments(&self) -> &[String] {
        match self {
            Reference::Remote(reference) => &reference.segments,
            Reference::File(reference) => &reference.segments,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    InvalidReference { repository: String, message: String },
    #[error("{message}")]
    UnsupportedLocalRepository { repository: String, message: String },
    #[error("{message}")]
    InvalidBranch { branch: String, message: String },
}

pub fn parse(input: &str, vars: &Vars) -> Option<Reference> {
    let cleaned = normalize_input(input);
    if cleaned.is_empty() {
        return None;
    }

    if let Some((owner, repo)) = github_shorthand(&cleaned) {
        return build_remote("github.com", vec![owner.to_owned(), repo.to_owned()], None, None, vars)
            .map(Reference::Remote);
    }

    if !cleaned.contains("://") {
        if let Some((host, path)) = scp_parts(&cleaned) {
            return build_remote(host, parts(path), Some(cleaned.clone()), None, vars).map(Reference::Remote);
        }

        let direct = parts(&cleaned);
        if direct.len() >= 2 && host_like(&direct[0]) {
            return build_remote(&direct[0], direct[1..].to_vec(), None, None, vars).map(Reference::Remote);
        }
        if direct.len() == 2 {
            return build_remote("github.com", direct, None, None, vars).map(Reference::Remote);
        }
    }

    let url = Url::parse(&cleaned).ok()?;
    if url.scheme() == "file" {
        return build_file(&url, &cleaned).map(Reference::File);
    }
    let segments = parts(url.path());
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    };
    let remote = if host == "github.com" { github_remote(&segments.join("/"), vars) } else { cleaned.clone() };
    build_remote(&host, segments, Some(remote), Some(format!("{}:", url.scheme())), vars).map(Reference::Remote)
}

pub fn parse_remote(input: &str, vars: &Vars) -> Result<RemoteReference, Error> {
    let reference = parse(input, vars).ok_or_else(|| Error::InvalidReference {
        repository: input.to_owned(),
        message: INVALID_REFERENCE_MESSAGE.to_owned(),
    })?;
    match reference {
        Reference::Remote(reference) => Ok(reference),
        Reference::File(_) => Err(Error::UnsupportedLocalRepository {
            repository: input.to_owned(),
            message: UNSUPPORTED_LOCAL_MESSAGE.to_owned(),
        }),
    }
}

pub fn validate_branch(branch: &str) -> Result<(), Error> {
    let valid = !branch.is_empty()
        && branch.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
        && !branch.starts_with('-')
        && !branch.contains("..");
    if valid {
        return Ok(());
    }
    Err(Error::InvalidBranch { branch: branch.to_owned(), message: INVALID_BRANCH_MESSAGE.to_owned() })
}

pub fn is_file(reference: &Reference) -> bool {
    matches!(reference, Reference::File(_))
}

pub fn is_remote(reference: &Reference) -> bool {
    !is_file(reference)
}

pub fn cache_path(root: &str, reference: &Reference, branch: Option<&str>) -> String {
    let mut parts: Vec<&str> = vec![root];
    parts.extend(reference.host().split(':'));
    parts.extend(reference.segments().iter().map(String::as_str));
    let base = Flavor::NATIVE.join(&parts);
    match branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => format!("{base}@{}", encode_component(branch)),
        None => base,
    }
}

pub fn cache_identity(reference: &Reference) -> String {
    format!("{}/{}", reference.host(), reference.path())
}

pub fn same(left: &Reference, right: &Reference) -> bool {
    cache_identity(left) == cache_identity(right)
}

fn normalize_input(input: &str) -> String {
    let input = input.trim();
    let input = input.strip_prefix("git+").unwrap_or(input);
    let input = input.split_once('#').map_or(input, |(head, _)| head);
    input.trim_end_matches('/').to_owned()
}

fn trim_git_suffix(input: &str) -> &str {
    input.strip_suffix(".git").unwrap_or(input)
}

fn parts(input: &str) -> Vec<String> {
    input.split('/').map(str::trim).map(trim_git_suffix).filter(|part| !part.is_empty()).map(str::to_owned).collect()
}

fn safe_host(input: &str) -> bool {
    !input.is_empty() && !input.starts_with('-') && input.split(':').all(safe_segment)
}

fn safe_segment(input: &str) -> bool {
    input != "."
        && input != ".."
        && !input.contains(':')
        && !input.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\'))
}

fn host_like(input: &str) -> bool {
    input.contains('.') || input.contains(':') || input == "localhost"
}

fn github_shorthand(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix("github:")?;
    let mut segments = rest.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let valid = |part: &str| !part.is_empty() && !part.chars().any(char::is_whitespace);
    (valid(owner) && valid(repo)).then_some((owner, repo))
}

fn scp_parts(input: &str) -> Option<(&str, &str)> {
    input
        .find('@')
        .filter(|at| !input[..*at].chars().any(|c| c == '/' || c.is_whitespace()))
        .and_then(|at| scp_from(input, at + 1))
        .or_else(|| scp_from(input, 0))
}

fn scp_from(input: &str, start: usize) -> Option<(&str, &str)> {
    let rest = input.get(start..)?;
    let (offset, _) = rest.char_indices().find(|(_, c)| *c == ':' || *c == '/' || c.is_whitespace())?;
    if input.as_bytes().get(start + offset) != Some(&b':') {
        return None;
    }
    let host = &input[start..start + offset];
    let path = &input[start + offset + 1..];
    (!host.is_empty() && !path.is_empty()).then_some((host, path))
}

fn github_remote(pathname: &str, vars: &Vars) -> String {
    let fallback = || format!("https://github.com/{pathname}.git");
    let Some(base) = vars.as_ref()("OPENCODE_REPO_CLONE_GITHUB_BASE_URL")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
    else {
        return fallback();
    };
    let base = if base.ends_with('/') { base } else { format!("{base}/") };
    match Url::parse(&base).and_then(|url| url.join(&format!("{pathname}.git"))) {
        Ok(url) => url.to_string(),
        Err(_) => fallback(),
    }
}

fn build_remote(
    host: &str,
    segments: Vec<String>,
    remote: Option<String>,
    protocol: Option<String>,
    vars: &Vars,
) -> Option<RemoteReference> {
    let segments: Vec<String> = segments
        .iter()
        .map(|segment| trim_git_suffix(segment).to_owned())
        .filter(|segment| !segment.is_empty())
        .collect();
    if !safe_host(host) || segments.is_empty() || segments.iter().any(|segment| !safe_segment(segment)) {
        return None;
    }
    let path = segments.join("/");
    let host = host.to_lowercase();
    let remote = remote.unwrap_or_else(|| {
        if host == "github.com" { github_remote(&path, vars) } else { format!("https://{host}/{path}.git") }
    });
    let owner = (segments.len() == 2).then(|| segments[0].clone());
    let repo = segments.last().cloned().unwrap_or_default();
    let label = if host == "github.com" && segments.len() == 2 { path.clone() } else { format!("{host}/{path}") };
    Some(RemoteReference { host, path, segments, owner, repo, remote, label, protocol })
}

fn build_file(url: &Url, remote: &str) -> Option<FileReference> {
    let path = url.to_file_path().ok()?;
    let file_path = Flavor::NATIVE.normalize(&path.to_string_lossy());
    let segments: Vec<&str> = file_path.split(['\\', '/']).filter(|segment| !segment.is_empty()).collect();
    let last = segments.last()?;
    Some(FileReference {
        host: "file".to_owned(),
        path: file_path.clone(),
        segments: segments.iter().map(|segment| segment.strip_suffix(':').unwrap_or(segment).to_owned()).collect(),
        owner: None,
        repo: trim_git_suffix(last).to_owned(),
        remote: remote.to_owned(),
        label: file_path,
        protocol: Some("file:".to_owned()),
    })
}
