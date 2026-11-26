use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream;
use opencode_ai::route::executor::{HttpResponse, HttpSender};
use opencode_contract::generated::{ConnectionInfo, CredentialValue};
use percent_encoding::utf8_percent_encode;
use serde::Deserialize;

use crate::console_access;
use crate::integration::IntegrationService;
use crate::provider_plugin::http::{self, HttpError};
use crate::shared_cache::SharedCache;

use super::follow::{FollowError, follow};
use super::remote::{ConsoleError, ENCODE_URI_COMPONENT, ListedSkill};

const TIMEOUT: Duration = Duration::from_secs(30);

const CONCURRENCY: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleInstruction {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleSkill {
    pub slug: String,
    pub main: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct CachedSkill {
    version: String,
    skill: ConsoleSkill,
}

#[derive(Deserialize)]
struct SkillBundle {
    slug: String,
    files: Vec<BundleFile>,
}

#[derive(Deserialize)]
struct BundleFile {
    path: String,
    content: String,
}

pub fn is_safe_segment(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

pub fn is_safe_relative_path(value: &str) -> bool {
    value.split('/').all(|segment| decode_segment(segment).is_some_and(|decoded| is_safe_segment(&decoded)))
        && !value.is_empty()
        && !value.contains(['\\', '\0', '?', '#'])
        && url::Url::parse(value).is_err()
        && !is_absolute_path(value)
}

fn is_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'))
}

fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let hex = segment.get(index + 1..index + 3)?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        index += 3;
    }
    percent_encoding::percent_decode_str(segment).decode_utf8().ok().map(|decoded| decoded.into_owned())
}

pub async fn load_skills(
    integration: &IntegrationService,
    http: &Arc<dyn HttpSender>,
    cache: &Arc<SharedCache>,
    connection: &ConnectionInfo,
    user_agent: &str,
    listed: &[ListedSkill],
) -> Option<Vec<ConsoleSkill>> {
    if listed.is_empty() {
        return Some(Vec::new());
    }
    let credential = integration.connection().resolve(connection).await.ok().flatten()?;
    if matches!(credential, CredentialValue::External(_)) {
        return Some(Vec::new());
    }
    let server = match console_access::normalize_server(&console_access::server(&credential)) {
        Ok(server) => server,
        Err(error) => {
            log::warn!("failed to load Console skills: {error}");
            return None;
        }
    };
    let workspace = console_access::workspace_key(connection, &credential);
    let headers = console_access::headers(&credential);
    let downloads: Vec<_> = listed
        .iter()
        .map(|item| {
            let http = http.clone();
            let cache = cache.clone();
            let server = server.clone();
            let workspace = workspace.clone();
            let headers = headers.clone();
            let user_agent = user_agent.to_owned();
            let item = item.clone();
            async move { load_skill(&http, &cache, &server, &workspace, &user_agent, &headers, &item).await }
        })
        .collect();
    let loaded: Vec<Option<ConsoleSkill>> = stream::iter(downloads).buffered(CONCURRENCY).collect().await;
    Some(loaded.into_iter().flatten().collect())
}

async fn load_skill(
    http: &Arc<dyn HttpSender>,
    cache: &Arc<SharedCache>,
    server: &str,
    workspace: &str,
    user_agent: &str,
    headers: &[(String, String)],
    item: &ListedSkill,
) -> Option<ConsoleSkill> {
    let key = format!("opencode-console-skill:{workspace}:{}", item.slug);
    let download = {
        let http = http.clone();
        let server = server.to_owned();
        let user_agent = user_agent.to_owned();
        let headers = headers.to_vec();
        let item = item.clone();
        move || {
            let http = http.clone();
            let server = server.clone();
            let user_agent = user_agent.clone();
            let headers = headers.clone();
            let item = item.clone();
            async move {
                let skill = download_skill(http.as_ref(), &server, &item.slug, &user_agent, headers).await?;
                Ok(CachedSkill { version: item.version, skill })
            }
        }
    };
    let cached =
        match cache.get::<CachedSkill, ConsoleError, _, _>(&key, Some(Duration::MAX), false, download.clone()).await {
            Ok(cached) if cached.version == item.version => cached,
            _ => match cache.get(&key, Some(Duration::MAX), true, download).await {
                Ok(cached) => cached,
                Err(error) => {
                    log::warn!("failed to load Console skill {}: {error:?}", item.slug);
                    return None;
                }
            },
        };
    Some(cached.skill)
}

pub async fn download_skill(
    http: &dyn HttpSender,
    server: &str,
    slug: &str,
    user_agent: &str,
    mut headers: Vec<(String, String)>,
) -> Result<ConsoleSkill, ConsoleError> {
    if !is_safe_segment(slug) {
        return Err(ConsoleError::Invalid(format!("unsafe skill slug: {slug}")));
    }
    headers.push(("accept".to_owned(), "application/json".to_owned()));
    headers.push(("user-agent".to_owned(), user_agent.to_owned()));
    let request =
        http::get(format!("{server}/api/v2/skills/{}", utf8_percent_encode(slug, ENCODE_URI_COMPONENT)), headers);
    let body = tokio::time::timeout(TIMEOUT, async {
        let response = follow(http, request, 0).await.map_err(follow_error)?;
        read_body(response).await
    })
    .await
    .map_err(|_| ConsoleError::Transient(HttpError::Timeout))??;
    let bundle: SkillBundle = serde_json::from_slice(&body)
        .map_err(|error| ConsoleError::Invalid(format!("the OpenCode Console skill {slug} is not valid: {error}")))?;
    if bundle.slug != slug {
        return Err(ConsoleError::Invalid(format!("expected skill {slug}, received {}", bundle.slug)));
    }
    let unsafe_path = bundle.files.iter().find(|file| !is_safe_relative_path(&file.path));
    if let Some(file) = unsafe_path {
        return Err(ConsoleError::Invalid(format!("unsafe skill file path: {}", file.path)));
    }
    let main = bundle
        .files
        .iter()
        .find(|file| file.path == "SKILL.md")
        .ok_or_else(|| ConsoleError::Invalid(format!("skill {slug} has no SKILL.md")))?;
    Ok(ConsoleSkill {
        slug: slug.to_owned(),
        main: main.content.clone(),
        files: bundle
            .files
            .into_iter()
            .filter(|file| file.path != "SKILL.md")
            .map(|file| (file.path, file.content))
            .collect(),
    })
}

fn follow_error(error: FollowError) -> ConsoleError {
    match error {
        FollowError::Status(status) => ConsoleError::Transient(HttpError::Status(http::BufferedResponse {
            status,
            headers: Vec::new(),
            body: bytes::Bytes::new(),
        })),
        FollowError::Transport(message) => ConsoleError::Transient(HttpError::Transport(message)),
        error @ FollowError::Redirect(_) => ConsoleError::Invalid(error.to_string()),
    }
}

async fn read_body(response: HttpResponse) -> Result<Vec<u8>, ConsoleError> {
    let mut body = Vec::new();
    let mut stream = response.body;
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(
            &chunk.map_err(|error| ConsoleError::Transient(HttpError::Transport(error.message().to_owned())))?,
        );
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{decode_segment, is_safe_relative_path, is_safe_segment};

    #[test]
    fn rejects_unsafe_skill_paths() {
        assert!(is_safe_segment("deploy"));
        assert!(!is_safe_segment("."));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment("a\\b"));

        assert!(is_safe_relative_path("SKILL.md"));
        assert!(is_safe_relative_path("references/guide.md"));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("../escape.md"));
        assert!(!is_safe_relative_path("a/../b"));
        assert!(!is_safe_relative_path("https://example.com/file"));
        assert!(!is_safe_relative_path("guide.md?raw=1"));
        assert!(!is_safe_relative_path("a%2Fb"));
        assert!(!is_safe_relative_path("a%zzb"));
    }

    #[test]
    fn decodes_percent_encoded_segments() {
        assert_eq!(decode_segment("a%20b").as_deref(), Some("a b"));
        assert_eq!(decode_segment("a%zzb"), None);
    }
}
