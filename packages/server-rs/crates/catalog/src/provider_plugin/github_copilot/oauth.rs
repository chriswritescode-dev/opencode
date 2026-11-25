use std::sync::Arc;
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, FormAnswer, FormField1, FormFields1, FormOption, FormStringField1,
    FormStringField1Type, FormValue, FormWhen1, FormWhen1Op, FormWhen1Value, IntegrationID, IntegrationMethodID,
    IntegrationOAuthMethod, IntegrationOAuthMethodType,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::integration::{
    AuthorizationError, AutoCallback, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
};
use crate::provider_plugin::github_copilot::models;
use crate::provider_plugin::http::{self, HttpError, PreparedHttp};

pub const CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

pub const API_VERSION: &str = "2026-08-01";

pub const USER_API_VERSION: &str = "2025-04-01";

pub const METHOD_ID: &str = "device";

const INTEGRATION_ID: &str = "github-copilot";

const LABEL: &str = "Login with GitHub Copilot";

const PUBLIC_DOMAIN: &str = "github.com";

const SAFETY_MARGIN: Duration = Duration::from_secs(3);

const SLOW_DOWN_FALLBACK: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct User {
    #[serde(default)]
    pub chat_enabled: Option<bool>,
    #[serde(default)]
    pub can_signup_for_limited: Option<bool>,
    #[serde(default)]
    pub endpoints: Option<UserEndpoints>,
}

impl User {
    pub fn api_endpoint(&self) -> Option<String> {
        let api = self.endpoints.as_ref()?.api.as_deref()?;
        let endpoint = api.trim_end_matches('/');
        (!endpoint.is_empty()).then(|| endpoint.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct UserEndpoints {
    #[serde(default)]
    pub api: Option<String>,
}

pub fn copilot_entitlement_error(user: &User) -> Option<String> {
    if user.chat_enabled != Some(false) {
        return None;
    }
    if user.can_signup_for_limited == Some(true) {
        return Some(
            "This GitHub account is not signed up for GitHub Copilot. Sign up for Copilot Free at https://github.com/features/copilot/plans and connect again."
                .to_owned(),
        );
    }
    Some(
        "This GitHub account does not have GitHub Copilot access. It needs an active Copilot subscription or a seat assigned by an organization."
            .to_owned(),
    )
}

pub fn copilot_base_url(metadata: Option<&JsonMap<Value>>) -> String {
    if let Some(endpoint) = metadata
        .and_then(|metadata| metadata.0.iter().find(|(key, _)| key == "apiEndpoint"))
        .and_then(|(_, value)| value.as_str())
        .filter(|endpoint| !endpoint.is_empty())
    {
        return endpoint.to_owned();
    }
    let enterprise = metadata
        .and_then(|metadata| metadata.0.iter().find(|(key, _)| key == "enterpriseUrl"))
        .and_then(|(_, value)| value.as_str());
    api_base_url(enterprise)
}

pub fn api_base_url(enterprise: Option<&str>) -> String {
    match enterprise.filter(|enterprise| !enterprise.is_empty()) {
        Some(enterprise) => format!("https://copilot-api.{}", normalize_domain(enterprise)),
        None => "https://api.githubcopilot.com".to_owned(),
    }
}

pub fn user_url(enterprise: Option<&str>) -> String {
    match enterprise.filter(|enterprise| !enterprise.is_empty()) {
        Some(enterprise) => format!("https://api.{}/copilot_internal/user", normalize_domain(enterprise)),
        None => "https://api.github.com/copilot_internal/user".to_owned(),
    }
}

pub fn normalize_domain(input: &str) -> String {
    let trimmed = input.strip_prefix("https://").or_else(|| input.strip_prefix("http://")).unwrap_or(input);
    trimmed.strip_suffix('/').unwrap_or(trimmed).to_owned()
}

pub async fn fetch_user(
    http: &dyn HttpSender,
    token: &str,
    user_agent: &str,
    enterprise: Option<&str>,
    timeout: Option<Duration>,
) -> Result<Option<User>, models::LoadError> {
    let headers = vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Authorization".to_owned(), format!("Bearer {token}")),
        ("User-Agent".to_owned(), user_agent.to_owned()),
        ("X-GitHub-Api-Version".to_owned(), USER_API_VERSION.to_owned()),
    ];
    let response = http::send(http, http::get(user_url(enterprise), headers), timeout).await?;
    let user = serde_json::from_slice(&response.body).map_err(|error| models::LoadError::Json(error.to_string()))?;
    Ok(Some(user))
}

pub struct CopilotOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
}

impl CopilotOAuth {
    pub fn new(http: Arc<dyn HttpSender>, user_agent: String) -> Self {
        Self {
            integration_id: IntegrationID(INTEGRATION_ID.to_owned()),
            method: IntegrationOAuthMethod {
                id: IntegrationMethodID(METHOD_ID.to_owned()),
                r#type: IntegrationOAuthMethodType::Oauth,
                label: LABEL.to_owned(),
                form: Some(FormFields1(vec![
                    FormField1::String(FormStringField1 {
                        key: "deploymentType".to_owned(),
                        title: Some("Select GitHub deployment type".to_owned()),
                        description: None,
                        required: Some(true),
                        hidden: None,
                        when: None,
                        r#type: FormStringField1Type::String,
                        format: None,
                        min_length: None,
                        max_length: None,
                        pattern: None,
                        placeholder: None,
                        default: None,
                        options: Some(vec![
                            FormOption {
                                value: "github.com".to_owned(),
                                label: "GitHub.com".to_owned(),
                                description: Some("Public".to_owned()),
                            },
                            FormOption {
                                value: "enterprise".to_owned(),
                                label: "GitHub Enterprise".to_owned(),
                                description: Some("Data residency or self-hosted".to_owned()),
                            },
                        ]),
                        custom: None,
                    }),
                    FormField1::String(FormStringField1 {
                        key: "enterpriseUrl".to_owned(),
                        title: Some("Enter your GitHub Enterprise URL or domain".to_owned()),
                        description: None,
                        required: Some(true),
                        hidden: None,
                        when: Some(vec![FormWhen1 {
                            key: "deploymentType".to_owned(),
                            op: FormWhen1Op::Eq,
                            value: FormWhen1Value::String("enterprise".to_owned()),
                        }]),
                        r#type: FormStringField1Type::String,
                        format: None,
                        min_length: None,
                        max_length: None,
                        pattern: None,
                        placeholder: Some("company.ghe.com or https://company.ghe.com".to_owned()),
                        default: None,
                        options: None,
                        custom: None,
                    }),
                ])),
            },
            http,
            user_agent,
        }
    }
}

impl OAuthImplementation for CopilotOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        Box::pin(async move { begin(http, user_agent, answer).await })
    }
}

#[derive(Deserialize)]
struct Device {
    verification_uri: String,
    user_code: String,
    device_code: String,
    interval: f64,
}

#[derive(Deserialize)]
struct Token {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    interval: Option<f64>,
}

async fn begin(
    http: Arc<dyn HttpSender>,
    user_agent: String,
    answer: FormAnswer,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let enterprise = answer.0.iter().any(|(key, value)| {
        key == "deploymentType" && matches!(value, FormValue::String(value) if value == "enterprise")
    });
    let enterprise_url = answer.0.iter().find(|(key, _)| key == "enterpriseUrl").and_then(|(_, value)| match value {
        FormValue::String(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    });
    if enterprise && enterprise_url.is_none() {
        return Err(error("Enterprise URL is required"));
    }
    let domain =
        if enterprise { normalize_domain(enterprise_url.as_deref().unwrap_or("")) } else { PUBLIC_DOMAIN.to_owned() };
    let device: Device = send_request(
        http.as_ref(),
        http::json(
            format!("https://{domain}/login/device/code"),
            headers(&user_agent),
            &serde_json::json!({ "client_id": CLIENT_ID, "scope": "read:user" }),
        ),
    )
    .await?;
    let wait = duration(device.interval.max(1.0));
    let instructions = format!("Enter code: {}", device.user_code);
    let callback: AutoCallback = Box::new({
        let http = http.clone();
        let user_agent = user_agent.clone();
        let domain = domain.clone();
        let device_code = device.device_code.clone();
        move || {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let domain = domain.clone();
            let device_code = device_code.clone();
            Box::pin(async move { poll(http, &user_agent, &domain, &device_code, wait, enterprise).await })
        }
    });
    Ok(OAuthAuthorization {
        url: device.verification_uri,
        instructions,
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: None },
    })
}

async fn poll(
    http: Arc<dyn HttpSender>,
    user_agent: &str,
    domain: &str,
    device_code: &str,
    wait: Duration,
    enterprise: bool,
) -> Result<CredentialOAuth, AuthorizationError> {
    let mut wait = wait;
    loop {
        let token: Token = send_request(
            http.as_ref(),
            http::json(
                format!("https://{domain}/login/oauth/access_token"),
                headers(user_agent),
                &serde_json::json!({
                    "client_id": CLIENT_ID,
                    "device_code": device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                }),
            ),
        )
        .await?;
        if let Some(access) = token.access_token.clone() {
            let user =
                fetch_user(http.as_ref(), &access, user_agent, enterprise.then_some(domain), None).await.ok().flatten();
            if let Some(message) = user.as_ref().and_then(copilot_entitlement_error) {
                return Err(error(message));
            }
            let mut metadata = Vec::new();
            if enterprise {
                metadata.push(("enterpriseUrl".to_owned(), Value::String(domain.to_owned())));
            }
            if let Some(endpoint) = user.as_ref().and_then(User::api_endpoint) {
                metadata.push(("apiEndpoint".to_owned(), Value::String(endpoint)));
            }
            return Ok(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID(METHOD_ID.to_owned()),
                refresh: access.clone(),
                access,
                expires: 0.0,
                metadata: (!metadata.is_empty()).then(|| JsonMap(metadata)),
            });
        }
        match token.error.as_deref() {
            Some("authorization_pending") => tokio::time::sleep(wait + SAFETY_MARGIN).await,
            Some("slow_down") => {
                wait = token
                    .interval
                    .filter(|interval| *interval > 0.0)
                    .map(duration)
                    .unwrap_or(wait + SLOW_DOWN_FALLBACK);
                tokio::time::sleep(wait + SAFETY_MARGIN).await;
            }
            failure => return Err(device_error(failure)),
        }
    }
}

async fn send_request<T: DeserializeOwned>(
    http: &dyn HttpSender,
    request: PreparedHttp,
) -> Result<T, AuthorizationError> {
    match http::send(http, request, None).await {
        Ok(response) => {
            serde_json::from_slice(&response.body).map_err(|failure| error(format!("Request failed: {failure}")))
        }
        Err(HttpError::Status(response)) => Err(error(format!("Request failed: {}", response.status))),
        Err(failure) => Err(error(format!("Request failed: {failure}"))),
    }
}

fn headers(user_agent: &str) -> Vec<(String, String)> {
    vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), user_agent.to_owned()),
    ]
}

fn duration(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::from_secs(1))
}

fn device_error(failure: Option<&str>) -> AuthorizationError {
    match failure.filter(|failure| !failure.is_empty()) {
        Some(failure) => error(format!("Device authorization failed: {failure}")),
        None => error("Device authorization failed"),
    }
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(std::io::Error::other(message.into()))
}
