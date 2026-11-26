use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use opencode_ai::HttpSender;
use opencode_config::seams::KvStore;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{ArtifactCapability, ArtifactProvider, ConnectionInfo, CredentialValue};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::console_access;
use crate::integration::IntegrationService;
use crate::provider_plugin::http::{self, BufferedResponse, HttpError, PreparedHttp};

/// The `GET /api/v2/config` response header Console sends while the caller may share artifacts.
pub const HEADER: &str = "x-opencode-artifacts";

const PROVIDER_ID: &str = "console";

const NAME: &str = "OpenCode Console";

const PROMPT: &str = "Artifacts are private pages the workspace's members open and comment on inside OpenCode Console. Each needs an index.html.";

const WAKE_PROMPT: &str = " Comments wake this session.";

const CONCURRENCY: usize = 4;

#[derive(Clone, Deserialize)]
struct Prepared {
    missing: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Published {
    url: String,
}

#[derive(Clone, Deserialize)]
struct Listing {
    revision: f64,
    files: Vec<ListedFile>,
}

#[derive(Clone, Deserialize)]
struct ListedFile {
    path: String,
    sha256: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Problem {
    message: Option<String>,
    owner: Option<String>,
}

enum Body {
    Json(Value),
    Bytes(Bytes),
}

/// The `console` artifact provider for one Location, the default while the Console connection may share and the
/// workspace advertises artifacts. While event watches run, publishing makes the session watch the artifact's
/// comments.
#[derive(Clone)]
pub struct ConsoleArtifacts {
    connection: ConnectionInfo,
    wakes: bool,
    integration: Arc<IntegrationService>,
    http: Arc<dyn HttpSender>,
    kv: Arc<dyn KvStore>,
}

impl ConsoleArtifacts {
    pub(crate) fn new(
        integration: Arc<IntegrationService>,
        http: Arc<dyn HttpSender>,
        kv: Arc<dyn KvStore>,
        connection: ConnectionInfo,
        wakes: bool,
    ) -> Self {
        Self { connection, wakes, integration, http, kv }
    }

    pub fn provider(&self) -> ArtifactProvider {
        ArtifactProvider {
            id: PROVIDER_ID.to_owned(),
            name: NAME.to_owned(),
            capabilities: vec![ArtifactCapability::Static],
            default: true,
        }
    }

    pub fn prompt(&self) -> String {
        format!("{PROMPT}{}", if self.wakes { WAKE_PROMPT } else { "" })
    }

    /// Stores the files Console doesn't have yet, then publishes them as the artifact for `id`. A create names the
    /// artifact after the title and a suffix from the ID; an update keeps the name the artifact already has.
    pub async fn publish(
        &self,
        id: &str,
        title: &str,
        files: BTreeMap<String, Bytes>,
        session_id: Option<String>,
        replace: bool,
    ) -> Result<String, String> {
        let slug = if replace { self.slug_of(id).await? } else { format!("{}-{}", slug_base(title), id_suffix(id)) };
        let credential = self.credential().await?;
        let files: Vec<(String, Bytes, String)> = files
            .into_iter()
            .map(|(path, bytes)| {
                let sha256 = opencode_util::hash::sha256_hex(&bytes);
                (path, bytes, sha256)
            })
            .collect();
        let described: Vec<Value> = files
            .iter()
            .map(|(path, bytes, sha256)| json!({ "path": path, "sha256": sha256, "bytes": bytes.len() }))
            .collect();
        let mut prepare = json!({ "slug": slug, "files": described });
        if replace {
            prepare["replace"] = json!(true);
        }
        let prepared: Prepared =
            self.send_json(&credential, "POST", "/api/artifacts/prepare", Some(Body::Json(prepare)), &slug).await?;
        let missing: std::collections::HashSet<&str> = prepared.missing.iter().map(String::as_str).collect();
        // Files with the same bytes upload once.
        let uploads: Vec<(&str, &Bytes)> = files
            .iter()
            .filter(|(_, _, sha256)| missing.contains(sha256.as_str()))
            .map(|(_, bytes, sha256)| (sha256.as_str(), bytes))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect();
        for chunk in uploads.chunks(CONCURRENCY) {
            let results = futures_util::future::join_all(chunk.iter().map(|(sha256, bytes)| {
                let slug = &slug;
                let credential = &credential;
                async move {
                    self.request(
                        credential,
                        "PUT",
                        &format!("/api/artifacts/blobs/{sha256}"),
                        Some(Body::Bytes((*bytes).clone())),
                        slug,
                    )
                    .await
                    .map(|_| ())
                }
            }))
            .await;
            for result in results {
                result?;
            }
        }
        let mut body = json!({
            "title": title.chars().take(200).collect::<String>().trim(),
            "files": files
                .iter()
                .map(|(path, _, sha256)| json!({ "path": path, "sha256": sha256 }))
                .collect::<Vec<_>>(),
        });
        if replace {
            body["replace"] = json!(true);
        }
        if self.wakes
            && let Some(session_id) = session_id
        {
            body["sessionID"] = json!(session_id);
        }
        let published: Published = self
            .send_json(&credential, "PUT", &format!("/api/artifacts/{slug}"), Some(Body::Json(body)), &slug)
            .await?;
        if !replace {
            self.kv.set(&slug_key(id), json!(slug)).await;
        }
        Ok(published.url)
    }

    /// Reads the files back, downloading the ones Console doesn't inline from the listed revision.
    pub async fn read(&self, id: &str) -> Result<BTreeMap<String, Bytes>, String> {
        let slug = self.slug_of(id).await?;
        let credential = self.credential().await?;
        let listing: Listing =
            self.send_json(&credential, "GET", &format!("/api/artifacts/{slug}/files"), None, &slug).await?;
        if let Some(file) = listing.files.iter().find(|file| !super::content::is_safe_relative_path(&file.path)) {
            return Err(format!("Unsafe OpenCode Console artifact path: {}", file.path));
        }
        let revision = encode_json(&listing.revision);
        let mut output = BTreeMap::new();
        for chunk in listing.files.chunks(CONCURRENCY) {
            let results = futures_util::future::join_all(chunk.iter().map(|file| {
                let credential = &credential;
                let slug = &slug;
                let revision = &revision;
                async move {
                    let bytes = match &file.text {
                        Some(text) => Bytes::from(text.clone()),
                        None => {
                            let path = format!("/api/artifacts/{slug}/blobs/{}?revision={revision}", file.sha256);
                            self.request(credential, "GET", &path, None, slug).await?.body
                        }
                    };
                    Ok::<_, String>((file.path.clone(), bytes))
                }
            }))
            .await;
            for result in results {
                let (path, bytes) = result?;
                output.insert(path, bytes);
            }
        }
        Ok(output)
    }

    /// Deletes the artifact; one Console no longer has, such as one deleted in Console, succeeds.
    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let slug = self.slug_of(id).await?;
        let credential = self.credential().await?;
        self.request(&credential, "DELETE", &format!("/api/artifacts/{slug}"), None, &slug).await?;
        self.kv.remove(&slug_key(id)).await;
        Ok(())
    }

    async fn slug_of(&self, id: &str) -> Result<String, String> {
        match self.kv.get(&slug_key(id)).await {
            Some(Value::String(slug)) => Ok(slug),
            _ => Err(format!("No OpenCode Console artifact for {id}")),
        }
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        credential: &CredentialValue,
        method: &str,
        path: &str,
        body: Option<Body>,
        slug: &str,
    ) -> Result<T, String> {
        let response = self.request(credential, method, path, body, slug).await?;
        serde_json::from_slice(&response.body)
            .map_err(|error| format!("the OpenCode Console artifact response is not valid: {error}"))
    }

    /// Sends Console requests as the connection's member; a refusal fails in plain words about the artifact `slug`.
    async fn request(
        &self,
        credential: &CredentialValue,
        method: &str,
        path: &str,
        body: Option<Body>,
        slug: &str,
    ) -> Result<BufferedResponse, String> {
        let server = console_access::normalize_server(&console_access::server(credential))?;
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        headers.extend(console_access::headers(credential));
        let (body, content_type) = match body {
            None => (Bytes::new(), None),
            Some(Body::Json(value)) => (
                Bytes::from(serde_json::to_vec(&value).expect("an artifact request body serializes")),
                Some("application/json"),
            ),
            Some(Body::Bytes(bytes)) => (bytes, Some("application/octet-stream")),
        };
        if let Some(content_type) = content_type {
            headers.push(("content-type".to_owned(), content_type.to_owned()));
        }
        let request = PreparedHttp::new(method, format!("{server}{path}"), headers, body);
        match http::send(self.http.as_ref(), request, Some(Duration::from_secs(30))).await {
            Ok(response) => Ok(response),
            Err(HttpError::Status(response)) if method == "DELETE" && response.status == 404 => Ok(response),
            Err(HttpError::Status(response)) => Err(problem(&response, method, slug)),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn credential(&self) -> Result<CredentialValue, String> {
        let resolved =
            self.integration.connection().resolve(&self.connection).await.map_err(|error| error.to_string())?;
        match resolved {
            Some(credential) if !matches!(credential, CredentialValue::External(_)) => Ok(credential),
            _ => Err("OpenCode Console is not connected".to_owned()),
        }
    }
}

fn slug_key(id: &str) -> String {
    format!("artifact-slug:{id}")
}

fn problem(response: &BufferedResponse, method: &str, slug: &str) -> String {
    let problem: Option<Problem> = serde_json::from_slice(&response.body).ok();
    if let Some(owner) = problem.as_ref().and_then(|problem| problem.owner.as_deref()) {
        return format!(
            "{owner} already shared an OpenCode Console artifact named {slug}; give this one another title"
        );
    }
    if response.status == 404 {
        return if method == "GET" {
            format!("OpenCode Console has no artifact named {slug}")
        } else {
            "Artifacts are off for this OpenCode Console workspace".to_owned()
        };
    }
    problem
        .and_then(|problem| problem.message)
        .unwrap_or_else(|| format!("OpenCode Console answered {}", response.status))
}

/// Console's slug rules: lowercase letters, digits and single dashes, at most 54 characters.
fn slugify(name: &str) -> Option<String> {
    let mut slug = String::new();
    let mut separator = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            slug.push(character);
        } else {
            separator = true;
        }
    }
    let slug: String = slug.chars().take(54).collect();
    if slug.is_empty() { None } else { Some(slug) }
}

fn slug_base(title: &str) -> String {
    match slugify(title) {
        Some(slug) => slug.chars().take(47).collect::<String>().trim_end_matches('-').to_owned(),
        None => "artifact".to_owned(),
    }
}

fn id_suffix(id: &str) -> String {
    let characters: Vec<char> = id.chars().collect();
    let start = characters.len().saturating_sub(6);
    characters[start..].iter().collect::<String>().to_lowercase()
}
