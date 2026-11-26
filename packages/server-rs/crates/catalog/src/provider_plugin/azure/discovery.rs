use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::generated::CredentialValue;
use opencode_util::time::{Date, UTC, Zone};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::provider_plugin::CommandRunner;
use crate::provider_plugin::http::{self, HttpError};

pub const MANAGEMENT_SCOPE: &str = "https://management.azure.com/.default";

pub(crate) const COGNITIVE_SCOPE: &str = "https://cognitiveservices.azure.com/.default";

pub(crate) const FOUNDRY_SCOPE: &str = "https://ai.azure.com/.default";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

const TOKEN_MARGIN_MS: f64 = 5.0 * 60_000.0;

pub type ResourceEndpoint = dyn Fn(&str) -> String + Send + Sync;

#[derive(Clone)]
pub struct Endpoints {
    pub resource: Arc<ResourceEndpoint>,
    pub management: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            resource: Arc::new(|name: &str| format!("https://{name}.openai.azure.com/openai")),
            management: "https://management.azure.com".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub name: String,
    pub model: String,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Azure CLI failed: {0}")]
    Command(String),
    #[error("invalid Azure response: {0}")]
    Json(String),
    #[error("{0}")]
    Message(String),
    #[error("Azure connection has no usable credential")]
    MissingCredential,
}

impl crate::shared_cache::Transient for LoadError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Http(error) if error.transient())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub(crate) access: String,
    expires: f64,
}

pub struct Discovery {
    http: Arc<dyn HttpSender>,
    commands: Arc<dyn CommandRunner>,
    user_agent: String,
    management: String,
    program: String,
    tokens: Mutex<HashMap<String, Token>>,
    token_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    token_generation: AtomicU64,
    resource_ids: Mutex<HashMap<String, String>>,
}

impl Discovery {
    pub fn new(
        http: Arc<dyn HttpSender>,
        commands: Arc<dyn CommandRunner>,
        user_agent: String,
        management: String,
        program: String,
    ) -> Self {
        Self {
            http,
            commands,
            user_agent,
            management,
            program,
            tokens: Mutex::new(HashMap::new()),
            token_locks: Mutex::new(HashMap::new()),
            token_generation: AtomicU64::new(0),
            resource_ids: Mutex::new(HashMap::new()),
        }
    }

    pub fn clear_tokens(&self) {
        self.token_generation.fetch_add(1, Ordering::SeqCst);
        self.tokens.lock().expect("lock the Azure tokens").clear();
    }

    pub async fn deployments(
        &self,
        url: &str,
        resource: &str,
        credential: &CredentialValue,
    ) -> Result<Vec<Deployment>, LoadError> {
        if matches!(credential, CredentialValue::External(_)) {
            return match self.management_deployments(resource).await {
                Ok(found) => Ok(found),
                Err(error) => match self.resource_deployments(url, credential).await {
                    Ok(found) => Ok(found),
                    Err(_) => Err(error),
                },
            };
        }
        self.resource_deployments(url, credential).await
    }

    pub(crate) async fn token(&self, scope: &str) -> Result<Token, LoadError> {
        if let Some(cached) = self.cached(scope, opencode_util::time::now_ms() as f64) {
            return Ok(cached);
        }
        let lock = self
            .token_locks
            .lock()
            .expect("lock the Azure token locks")
            .entry(scope.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _permit = lock.lock().await;
        if let Some(cached) = self.cached(scope, opencode_util::time::now_ms() as f64) {
            return Ok(cached);
        }
        let generation = self.token_generation.load(Ordering::SeqCst);
        let args: Vec<String> = ["account", "get-access-token", "--scope", scope, "--output", "json"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let response = self.command(&args).await?;
        let expires = match response.expires_on {
            Some(value) => value * 1000.0,
            None => parse_expires_on(response.expires_on_text.as_deref().unwrap_or_default())
                .ok_or_else(|| LoadError::Message("Azure CLI returned an invalid token expiration".to_owned()))?,
        };
        if !expires.is_finite() {
            return Err(LoadError::Message("Azure CLI returned an invalid token expiration".to_owned()));
        }
        let token = Token { access: response.access_token, expires };
        if self.token_generation.load(Ordering::SeqCst) == generation {
            self.tokens.lock().expect("lock the Azure tokens").insert(scope.to_owned(), token.clone());
        }
        Ok(token)
    }

    fn cached(&self, scope: &str, now: f64) -> Option<Token> {
        self.tokens
            .lock()
            .expect("lock the Azure tokens")
            .get(scope)
            .filter(|cached| cached.expires - now > TOKEN_MARGIN_MS)
            .cloned()
    }

    async fn command(&self, args: &[String]) -> Result<TokenResponse, LoadError> {
        let output =
            tokio::time::timeout(TOKEN_TIMEOUT, self.commands.run(self.program.clone(), args.to_vec(), Vec::new()))
                .await
                .map_err(|_| LoadError::Command("az did not answer within 10 seconds".to_owned()))?
                .map_err(|error| LoadError::Command(error.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(LoadError::Command(if stderr.is_empty() { "az failed".to_owned() } else { stderr }));
        }
        let response: TokenResponse =
            serde_json::from_slice(&output.stdout).map_err(|error| LoadError::Json(error.to_string()))?;
        if response.access_token.is_empty() {
            return Err(LoadError::Json("the Azure CLI access token is empty".to_owned()));
        }
        if response.expires_on_text.as_deref() == Some("") {
            return Err(LoadError::Json("the Azure CLI token expiration is empty".to_owned()));
        }
        Ok(response)
    }

    async fn management_get(&self, url: &str) -> Result<Value, LoadError> {
        let token = self.token(MANAGEMENT_SCOPE).await?;
        let headers = self.headers(token);
        let response = http::send(self.http.as_ref(), http::get(url, headers), Some(REQUEST_TIMEOUT)).await?;
        serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))
    }

    fn headers(&self, token: Token) -> Vec<(String, String)> {
        vec![
            ("authorization".to_owned(), format!("Bearer {}", token.access)),
            ("accept".to_owned(), "application/json".to_owned()),
            ("user-agent".to_owned(), self.user_agent.clone()),
        ]
    }

    async fn find_resource(&self, resource: &str) -> Result<String, LoadError> {
        let token = self.token(MANAGEMENT_SCOPE).await?;
        let url = format!("{}/providers/Microsoft.ResourceGraph/resources?api-version=2022-10-01", self.management);
        let request = http::json(url, self.headers(token), &json!({ "query": resource_query(resource) }));
        let response = http::send(self.http.as_ref(), request, Some(REQUEST_TIMEOUT)).await?;
        let body: Resources =
            serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))?;
        body.data
            .first()
            .map(|item| item.id.clone())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| LoadError::Message(format!("Azure resource \"{resource}\" was not found")))
    }

    async fn management_deployments(&self, resource: &str) -> Result<Vec<Deployment>, LoadError> {
        let key = resource.to_lowercase();
        let result = self.management_pages(resource, &key).await;
        if result.is_err() {
            self.resource_ids.lock().expect("lock the Azure resource ids").remove(&key);
        }
        result
    }

    async fn management_pages(&self, resource: &str, key: &str) -> Result<Vec<Deployment>, LoadError> {
        let cached = { self.resource_ids.lock().expect("lock the Azure resource ids").get(key).cloned() };
        let id = match cached {
            Some(id) => id,
            None => {
                let id = self.find_resource(resource).await?;
                self.resource_ids.lock().expect("lock the Azure resource ids").insert(key.to_owned(), id.clone());
                id
            }
        };
        let origin = url::Url::parse(&self.management).ok().map(|url| url.origin().ascii_serialization());
        let mut next = Some(format!("{}{}/deployments?api-version=2024-10-01", self.management, id));
        let mut seen = HashSet::new();
        let mut deployments = Vec::new();
        while let Some(url) = next {
            if !seen.insert(url.clone()) {
                return Err(LoadError::Message("Azure repeated a deployment page".to_owned()));
            }
            let page: ManagementDeployments = serde_json::from_value(self.management_get(&url).await?)
                .map_err(|error| LoadError::Json(error.to_string()))?;
            if let Some(link) = &page.next_link {
                let link_origin = url::Url::parse(link).ok().map(|parsed| parsed.origin().ascii_serialization());
                if link_origin != origin {
                    return Err(LoadError::Message(
                        "Azure returned a deployment page outside the management endpoint".to_owned(),
                    ));
                }
            }
            for raw in page.value {
                let Ok(item) = serde_json::from_value::<ManagementDeployment>(raw) else {
                    continue;
                };
                if item.properties.provisioning_state != "Succeeded"
                    || item.name.is_empty()
                    || item.properties.model.name.is_empty()
                {
                    continue;
                }
                deployments.push(Deployment { name: item.name, model: item.properties.model.name });
            }
            next = page.next_link;
        }
        Ok(deployments)
    }

    async fn resource_deployments(
        &self,
        url: &str,
        credential: &CredentialValue,
    ) -> Result<Vec<Deployment>, LoadError> {
        let mut headers = vec![
            ("accept".to_owned(), "application/json".to_owned()),
            ("user-agent".to_owned(), self.user_agent.clone()),
        ];
        match credential {
            CredentialValue::Key(credential) => {
                headers.push(("api-key".to_owned(), credential.key.clone()));
            }
            _ => {
                let token = self.token(COGNITIVE_SCOPE).await?;
                headers.push(("authorization".to_owned(), format!("Bearer {}", token.access)));
            }
        }
        let response = http::send(self.http.as_ref(), http::get(url, headers), Some(REQUEST_TIMEOUT)).await?;
        let body: ResourceDeployments =
            serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))?;
        Ok(body
            .data
            .into_iter()
            .filter_map(|raw| serde_json::from_value::<ResourceDeployment>(raw).ok())
            .filter(|item| item.status == "succeeded" && !item.id.is_empty() && !item.model.is_empty())
            .map(|item| Deployment { name: item.id, model: item.model })
            .collect())
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "expires_on", default)]
    expires_on: Option<f64>,
    #[serde(rename = "expiresOn", default)]
    expires_on_text: Option<String>,
}

#[derive(Deserialize)]
struct Resources {
    data: Vec<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    id: String,
}

#[derive(Deserialize)]
struct ResourceDeployments {
    data: Vec<Value>,
}

#[derive(Deserialize)]
struct ResourceDeployment {
    id: String,
    model: String,
    status: String,
}

#[derive(Deserialize)]
struct ManagementDeployments {
    value: Vec<Value>,
    #[serde(rename = "nextLink", default)]
    next_link: Option<String>,
}

#[derive(Deserialize)]
struct ManagementDeployment {
    name: String,
    properties: ManagementProperties,
}

#[derive(Deserialize)]
struct ManagementProperties {
    model: ManagementModel,
    #[serde(rename = "provisioningState")]
    provisioning_state: String,
}

#[derive(Deserialize)]
struct ManagementModel {
    name: String,
}

fn resource_query(resource: &str) -> String {
    [
        "resources",
        "| where type =~ 'microsoft.cognitiveservices/accounts' and kind in~ ('AIServices', 'OpenAI')",
        "| extend resourceName = tostring(properties.customSubDomainName)",
        &format!("| where resourceName =~ '{resource}'"),
        "| project id",
        "| take 1",
    ]
    .join(" ")
}

fn parse_expires_on(value: &str) -> Option<f64> {
    let value = value.trim();
    let (date_text, rest) = value.split_at_checked(10)?;
    let mut date = date_text.split('-');
    let year: i32 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        return None;
    }
    let date = Date::new(year, month, day)?;
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    let (rest, zone) = match rest.strip_suffix('Z') {
        Some(rest) => (rest, UTC),
        None => (rest, Zone::Local),
    };
    let (clock, fraction) = match rest.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (rest, None),
    };
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = fraction
        .map(|fraction| {
            let digits: String = fraction.chars().take(3).collect();
            let padded = format!("{digits:0<3}");
            padded.parse::<u32>().ok()
        })
        .unwrap_or(Some(0))?;
    Some((zone.instant(date, hour * 3_600 + minute * 60 + second) + i64::from(millis)) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKYO: f64 = 1_704_132_245_678.0;

    #[test]
    fn parses_the_cli_expiration_in_the_process_zone() {
        if std::env::var("TZ").as_deref() != Ok("Asia/Tokyo") {
            return;
        }
        assert_eq!(parse_expires_on("2024-01-02 03:04:05.678"), Some(TOKYO));
        assert_eq!(parse_expires_on("2024-07-01T12:00:00"), Some(1_719_802_800_000.0));
    }

    #[test]
    fn parses_the_cli_expiration_in_every_zone() {
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "provider_plugin::azure::discovery::tests::parses_the_cli_expiration_in_the_process_zone",
            ])
            .args(["--test-threads=1"])
            .env("TZ", "Asia/Tokyo")
            .output()
            .expect("run the zone test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success() && stdout.contains("1 passed"), "{stdout}\n{stderr}");
    }

    #[test]
    fn parses_utc_expirations_and_rejects_malformed_ones() {
        assert_eq!(parse_expires_on("2024-01-01T18:04:05.678Z"), Some(TOKYO));
        assert_eq!(parse_expires_on("2024-02-29T00:00:00Z"), Some(1_709_164_800_000.0));
        assert_eq!(parse_expires_on("2024-02-30T00:00:00Z"), None);
        assert_eq!(parse_expires_on("2024-01-01"), None);
        assert_eq!(parse_expires_on("2024-01-01T24:00:00Z"), None);
        assert_eq!(parse_expires_on(""), None);
    }
}
