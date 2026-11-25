use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opencode_api_types::js::{decode_component, encode_component};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

const SKILL_CONCURRENCY: usize = 4;
const FILE_CONCURRENCY: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const ATTEMPTS: u32 = 3;

pub fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

pub fn is_safe_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || value.contains('?')
        || value.contains('#')
        || Url::parse(value).is_ok()
        || value.starts_with('/')
        || is_windows_absolute(value)
    {
        return false;
    }
    value.split('/').all(|segment| decode_component(segment).is_some_and(|decoded| is_safe_segment(&decoded)))
}

pub fn cache_directory(cache: &Path, url: &str) -> PathBuf {
    cache.join("skills").join(hex(ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        normalized(url).as_bytes(),
    )
    .as_ref()))
}

pub async fn pull(url: &str, cache: &Path) -> Vec<PathBuf> {
    let base = normalized(url);
    let Ok(source) = Url::parse(&base) else {
        log::error!("failed to parse skill source url {url}");
        return Vec::new();
    };
    let Ok(index_url) = source.join("index.json") else {
        log::error!("failed to build skill index url {base}");
        return Vec::new();
    };
    let client = opencode_config::http::client();
    let Some(body) = fetch(&client, index_url.as_str(), Some("application/json")).await else {
        log::error!("failed to fetch skill index {index_url}");
        return Vec::new();
    };
    let Ok(body) = String::from_utf8(body) else {
        log::error!("failed to parse skill index {index_url}");
        return Vec::new();
    };
    let Some(index) = parse_index(&body) else {
        log::error!("failed to parse skill index {index_url}");
        return Vec::new();
    };
    let source_root = cache_directory(cache, url);
    run(&client, plan(&index, &source, &source_root)).await
}

fn is_windows_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\')
}

fn normalized(url: &str) -> String {
    if url.ends_with('/') {
        return url.to_string();
    }
    format!("{url}/")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

struct Index {
    skills: Vec<IndexSkill>,
}

struct IndexSkill {
    name: String,
    version: Option<String>,
    files: Vec<String>,
}

fn parse_index(body: &str) -> Option<Index> {
    let value: Value = serde_json::from_str(body).ok()?;
    let skills = value.get("skills")?.as_array()?;
    let skills = skills.iter().map(parse_skill).collect::<Option<Vec<_>>>()?;
    Some(Index { skills })
}

fn parse_skill(value: &Value) -> Option<IndexSkill> {
    let name = value.get("name")?.as_str()?.to_string();
    let version = match value.get("version") {
        None | Some(Value::Null) => None,
        Some(Value::String(version)) => Some(version.clone()),
        Some(_) => return None,
    };
    let files = value
        .get("files")?
        .as_array()?
        .iter()
        .map(|file| file.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(IndexSkill { name, version, files })
}

struct PlannedSkill {
    name: String,
    version: Option<String>,
    root: PathBuf,
    files: Vec<PlannedFile>,
}

struct PlannedFile {
    url: String,
    file: String,
}

fn plan(index: &Index, source: &Url, source_root: &Path) -> Vec<PlannedSkill> {
    index.skills.iter().filter_map(|skill| plan_skill(skill, source, source_root)).collect()
}

fn plan_skill(skill: &IndexSkill, source: &Url, source_root: &Path) -> Option<PlannedSkill> {
    if !is_safe_segment(&skill.name) {
        return None;
    }
    let document = format!("{}.md", skill.name);
    if !skill.files.iter().any(|file| file == "SKILL.md" || file == &document) {
        return None;
    }
    let root = source_root.join(&skill.name);
    if !root.starts_with(source_root) || root == source_root {
        return None;
    }
    let skill_url = source.join(&format!("{}/", encode_component(&skill.name))).ok()?;
    let files =
        skill.files.iter().map(|file| plan_file(file, &skill_url, source, &root)).collect::<Option<Vec<_>>>()?;
    Some(PlannedSkill { name: skill.name.clone(), version: skill.version.clone(), root, files })
}

fn plan_file(file: &str, skill_url: &Url, source: &Url, root: &Path) -> Option<PlannedFile> {
    if !is_safe_relative_path(file) {
        return None;
    }
    let resource = skill_url.join(file).ok()?;
    if resource.origin() != source.origin() {
        return None;
    }
    let destination = root.join(file);
    if !destination.starts_with(root) || destination == root {
        return None;
    }
    Some(PlannedFile { url: resource.to_string(), file: file.to_string() })
}

async fn run(client: &reqwest::Client, skills: Vec<PlannedSkill>) -> Vec<PathBuf> {
    let count = skills.len();
    let semaphore = Arc::new(Semaphore::new(SKILL_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for (index, skill) in skills.into_iter().enumerate() {
        let Ok(permit) = semaphore.clone().acquire_owned().await else { continue };
        let client = client.clone();
        tasks.spawn(async move {
            let _permit = permit;
            (index, load_skill(&client, skill).await)
        });
    }
    let mut roots = vec![None; count];
    while let Some(joined) = tasks.join_next().await {
        let Ok((index, root)) = joined else { continue };
        roots[index] = root;
    }
    roots.into_iter().flatten().collect()
}

async fn load_skill(client: &reqwest::Client, skill: PlannedSkill) -> Option<PathBuf> {
    let current = match &skill.version {
        Some(_) => tokio::fs::read_to_string(skill.root.join(".opencode-version")).await.ok(),
        None => None,
    };
    if skill.version.is_none() || current == skill.version {
        download_files(client, &skill.files, &skill.root).await;
    } else {
        refresh(client, &skill).await;
    }
    if has_document(&skill.root, &skill.name).await {
        return Some(skill.root);
    }
    None
}

async fn refresh(client: &reqwest::Client, skill: &PlannedSkill) {
    let Some(version) = skill.version.as_deref() else { return };
    let token = hex(&rand::random::<[u8; 16]>());
    let staging = suffix(&skill.root, ".tmp-", &token);
    let backup = suffix(&skill.root, ".old-", &token);
    if let Err(error) = refresh_staged(client, skill, version, &staging, &backup).await {
        log::error!("failed to refresh skill {}: {error}", skill.name);
    }
    let _ = tokio::fs::remove_dir_all(&staging).await;
}

async fn refresh_staged(
    client: &reqwest::Client,
    skill: &PlannedSkill,
    version: &str,
    staging: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    if !fetch_files(client, &skill.files, staging).await {
        return Err(std::io::Error::other("a skill file failed to download"));
    }
    if !has_document(staging, &skill.name).await {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the refreshed skill has no document"));
    }
    tokio::fs::write(staging.join(".opencode-version"), version).await?;
    swap(&skill.root, staging, backup).await
}

async fn swap(root: &Path, staging: &Path, backup: &Path) -> std::io::Result<()> {
    let cached = tokio::fs::symlink_metadata(root).await.is_ok();
    if cached {
        tokio::fs::rename(root, backup).await?;
    }
    match tokio::fs::rename(staging, root).await {
        Ok(()) => {
            if cached {
                let _ = tokio::fs::remove_dir_all(backup).await;
            }
            Ok(())
        }
        Err(error) => {
            if cached {
                let _ = tokio::fs::rename(backup, root).await;
            }
            Err(error)
        }
    }
}

fn suffix(root: &Path, infix: &str, token: &str) -> PathBuf {
    let mut name = root.as_os_str().to_os_string();
    name.push(infix);
    name.push(token);
    PathBuf::from(name)
}

async fn has_document(root: &Path, name: &str) -> bool {
    tokio::fs::metadata(root.join("SKILL.md")).await.is_ok()
        || tokio::fs::metadata(root.join(format!("{name}.md"))).await.is_ok()
}

async fn download_files(client: &reqwest::Client, files: &[PlannedFile], root: &Path) {
    fetch_files(client, files, root).await;
}

async fn fetch_files(client: &reqwest::Client, files: &[PlannedFile], root: &Path) -> bool {
    let semaphore = Arc::new(Semaphore::new(FILE_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for file in files {
        let Ok(permit) = semaphore.clone().acquire_owned().await else { return false };
        let client = client.clone();
        let url = file.url.clone();
        let destination = root.join(&file.file);
        tasks.spawn(async move {
            let _permit = permit;
            download(&client, &url, &destination).await
        });
    }
    let mut complete = true;
    while let Some(joined) = tasks.join_next().await {
        complete = joined.unwrap_or(false) && complete;
    }
    complete
}

async fn download(client: &reqwest::Client, url: &str, destination: &Path) -> bool {
    if tokio::fs::metadata(destination).await.is_ok() {
        return true;
    }
    let Some(body) = fetch(client, url, None).await else {
        log::error!("failed to download skill file {url}");
        return false;
    };
    let Some(parent) = destination.parent() else {
        log::error!("failed to download skill file {url}: the destination has no parent");
        return false;
    };
    let written = async {
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(destination, &body).await
    }
    .await;
    if let Err(error) = written {
        log::error!("failed to download skill file {url}: {error}");
        return false;
    }
    true
}

async fn fetch(client: &reqwest::Client, url: &str, accept: Option<&str>) -> Option<Vec<u8>> {
    for attempt in 0..ATTEMPTS {
        let mut request = client.get(url).timeout(REQUEST_TIMEOUT);
        if let Some(accept) = accept {
            request = request.header(reqwest::header::ACCEPT, accept);
        }
        let retryable = match request.send().await {
            Ok(response) if response.status().is_success() => match response.bytes().await {
                Ok(body) => return Some(body.to_vec()),
                Err(error) => {
                    log::warn!("failed to read {url}: {error}");
                    true
                }
            },
            Ok(response) => {
                log::warn!("failed to fetch {url}: {}", response.status());
                response.status().is_server_error() || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
            }
            Err(error) => {
                log::warn!("failed to fetch {url}: {error}");
                true
            }
        };
        if !retryable || attempt + 1 == ATTEMPTS {
            return None;
        }
        tokio::time::sleep(backoff(attempt)).await;
    }
    None
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(rand::random_range(0..=(200u64 << attempt)))
}
