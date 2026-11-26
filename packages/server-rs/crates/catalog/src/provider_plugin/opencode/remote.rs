use std::time::Duration;

use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConnectionStatus, ConnectionStatusStatus, CredentialValue,
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::console_access;
use crate::integration::AuthorizationError;
use crate::provider_plugin::http::{self, HttpError};
use crate::provider_plugin::opencode::sign_in::SignedOut;
use crate::shared_cache::Transient;

const TIMEOUT: Duration = Duration::from_secs(5);

const LISTS_HEADER: &str = "x-opencode-instructions-skills";
const WATCHES_HEADER: &str = "x-opencode-event-watches";
const ARTIFACTS_HEADER: &str = "x-opencode-artifacts";

const SIGNED_OUT_MESSAGE: &str = "Reconnect OpenCode Console to continue";

pub(super) const ENCODE_URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteConfig {
    pub info: ConfigEntryDocumentInfo,
    pub watches: bool,
    pub lists: bool,
    pub artifacts: bool,
    pub instructions: Vec<ListedInstruction>,
    pub skills: Vec<ListedSkill>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ListedInstruction {
    pub id: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ListedSkill {
    pub slug: String,
    pub version: String,
}

#[derive(Deserialize)]
struct InstructionsBody {
    instructions: Vec<ListedInstruction>,
}

#[derive(Deserialize)]
struct SkillsBody {
    skills: Vec<ListedSkill>,
}

#[derive(Clone, Debug)]
pub enum ConsoleError {
    SignedOut,
    SsoRequired { organization: Option<String>, url: String },
    Authorization(AuthorizationError),
    Transient(HttpError),
    Invalid(String),
}

impl Transient for ConsoleError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

#[derive(Deserialize)]
struct Unauthorized {
    #[serde(rename = "_tag")]
    tag: String,
}

#[derive(Deserialize)]
struct SsoRequiredBody {
    #[serde(rename = "_tag")]
    tag: String,
    #[serde(rename = "orgId")]
    _org_id: String,
    #[serde(rename = "connectionId")]
    connection_id: String,
}

pub async fn fetch_config(
    http: &dyn opencode_ai::HttpSender,
    user_agent: &str,
    credential: &CredentialValue,
) -> Result<Option<RemoteConfig>, ConsoleError> {
    let server = console_access::server(credential);
    let headers = request_headers(user_agent, credential);
    let request = http::get(format!("{server}/api/v2/config"), headers);
    let response = match http::send(http, request, Some(TIMEOUT)).await {
        Ok(response) => response,
        Err(HttpError::Status(response)) => {
            if response.status == 404 {
                return Ok(None);
            }
            if response.status == 401 && is_unauthorized(&response.body) {
                return Err(ConsoleError::SignedOut);
            }
            if response.status == 403
                && let Some(connection_id) = sso_connection(&response.body)
            {
                return Err(ConsoleError::SsoRequired {
                    organization: organization_name(credential),
                    url: sso_url(&server, &connection_id),
                });
            }
            return Err(ConsoleError::Transient(HttpError::Status(response)));
        }
        Err(error) => return Err(ConsoleError::Transient(error)),
    };
    let body: Value = serde_json::from_slice(&response.body)
        .map_err(|error| ConsoleError::Invalid(format!("the OpenCode Console config is not JSON: {error}")))?;
    let info = opencode_config::integration::remote(&body).map_err(ConsoleError::Invalid)?;
    Ok(Some(RemoteConfig {
        info,
        watches: has_header(&response.headers, WATCHES_HEADER),
        lists: has_header(&response.headers, LISTS_HEADER),
        artifacts: has_header(&response.headers, ARTIFACTS_HEADER),
        instructions: Vec::new(),
        skills: Vec::new(),
    }))
}

pub async fn fetch_instructions(
    http: &dyn opencode_ai::HttpSender,
    user_agent: &str,
    credential: &CredentialValue,
) -> Result<Vec<ListedInstruction>, ConsoleError> {
    Ok(fetch_list::<InstructionsBody>(http, user_agent, credential, "/api/v2/instructions")
        .await?
        .map(|body| body.instructions)
        .unwrap_or_default())
}

pub async fn fetch_skills(
    http: &dyn opencode_ai::HttpSender,
    user_agent: &str,
    credential: &CredentialValue,
) -> Result<Vec<ListedSkill>, ConsoleError> {
    Ok(fetch_list::<SkillsBody>(http, user_agent, credential, "/api/v2/skills")
        .await?
        .map(|body| body.skills)
        .unwrap_or_default())
}

async fn fetch_list<T: DeserializeOwned>(
    http: &dyn opencode_ai::HttpSender,
    user_agent: &str,
    credential: &CredentialValue,
    path: &str,
) -> Result<Option<T>, ConsoleError> {
    let server = console_access::server(credential);
    let request = http::get(format!("{server}{path}"), request_headers(user_agent, credential));
    let response = match http::send(http, request, Some(TIMEOUT)).await {
        Ok(response) => response,
        Err(HttpError::Status(response)) => {
            if response.status == 404 {
                return Ok(None);
            }
            if response.status == 403 && !is_sso_required(&response.body) {
                return Ok(None);
            }
            return Err(ConsoleError::Transient(HttpError::Status(response)));
        }
        Err(error) => return Err(ConsoleError::Transient(error)),
    };
    serde_json::from_slice(&response.body)
        .map(Some)
        .map_err(|error| ConsoleError::Invalid(format!("the OpenCode Console list {path} is not valid: {error}")))
}

pub fn connection_status(error: &ConsoleError) -> Option<ConnectionStatus> {
    match error {
        ConsoleError::SsoRequired { organization, url } => Some(ConnectionStatus {
            status: ConnectionStatusStatus::NeedsAuth,
            message: sso_message(organization.as_deref()),
            url: Some(url.clone()),
        }),
        ConsoleError::SignedOut => Some(signed_out_status()),
        ConsoleError::Authorization(error) if authorization_signed_out(error) => Some(signed_out_status()),
        ConsoleError::Authorization(_) | ConsoleError::Transient(_) | ConsoleError::Invalid(_) => None,
    }
}

fn authorization_signed_out(error: &AuthorizationError) -> bool {
    error.cause().is_some_and(|cause| {
        cause.downcast_ref::<SignedOut>().is_some()
            || cause.downcast_ref::<AuthorizationError>().is_some_and(authorization_signed_out)
    })
}

pub fn organization_name(credential: &CredentialValue) -> Option<String> {
    console_access::metadata_field(credential, "orgName").and_then(Value::as_str).map(str::to_owned)
}

fn request_headers(user_agent: &str, credential: &CredentialValue) -> Vec<(String, String)> {
    let mut headers =
        vec![("accept".to_owned(), "application/json".to_owned()), ("user-agent".to_owned(), user_agent.to_owned())];
    headers.extend(console_access::headers(credential));
    headers
}

fn is_unauthorized(body: &[u8]) -> bool {
    serde_json::from_slice::<Unauthorized>(body).is_ok_and(|body| body.tag == "Unauthorized")
}

fn sso_connection(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<SsoRequiredBody>(body)
        .ok()
        .filter(|body| body.tag == "SsoRequired")
        .map(|body| body.connection_id)
}

fn is_sso_required(body: &[u8]) -> bool {
    sso_connection(body).is_some()
}

fn sso_url(server: &str, connection_id: &str) -> String {
    let base = console_access::normalize_server(server).unwrap_or_else(|_| server.to_owned());
    let redirect = url::Url::parse(&base)
        .map(|url| format!("{}/", url.path().trim_end_matches('/')))
        .unwrap_or_else(|_| "/".to_owned());
    format!(
        "{base}/auth/sso/{}/start?redirectTo={}",
        utf8_percent_encode(connection_id, ENCODE_URI_COMPONENT),
        utf8_percent_encode(&redirect, ENCODE_URI_COMPONENT)
    )
}

fn sso_message(organization: Option<&str>) -> String {
    format!("Sign in with SSO again to use {}", organization.unwrap_or("your OpenCode Console organization"))
}

fn signed_out_status() -> ConnectionStatus {
    ConnectionStatus { status: ConnectionStatusStatus::NeedsAuth, message: SIGNED_OUT_MESSAGE.to_owned(), url: None }
}

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(key, _)| key.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::sso_url;

    #[test]
    fn builds_the_sso_url_under_the_console_base_path() {
        assert_eq!(
            sso_url("https://console.example.com/console", "ssoconn/1"),
            "https://console.example.com/console/auth/sso/ssoconn%2F1/start?redirectTo=%2Fconsole%2F"
        );
        assert_eq!(
            sso_url("https://console.example.com", "ssoconn_1"),
            "https://console.example.com/auth/sso/ssoconn_1/start?redirectTo=%2F"
        );
    }
}
