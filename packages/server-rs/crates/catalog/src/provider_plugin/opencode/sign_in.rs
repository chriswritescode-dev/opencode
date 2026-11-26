use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, FormField1, FormFields1, FormStringField1,
    FormStringField1Format, FormStringField1Type, FormValue, IntegrationID, IntegrationMethodID,
    IntegrationOAuthMethod, IntegrationOAuthMethodType,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::console_access;
use crate::integration::{
    AuthorizationError, AutoCallback, LabelCallback, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
    RefreshCallback,
};
use crate::provider_plugin::http::{self, HttpError};

const INTEGRATION_ID: &str = "opencode";

const CLIENT_ID: &str = "opencode-cli";

const METHOD_ID: &str = "device";

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

const SLOW_DOWN_MILLIS: u64 = 5000;

const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5);

const DEFAULT_EXPIRES_IN: Duration = Duration::from_secs(10 * 60);

const MAX_EXPIRES_IN: Duration = Duration::from_secs(24 * 60 * 60);

const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: f64,
    interval: f64,
}

#[derive(Debug, Deserialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    expires_in: f64,
    #[serde(default)]
    org_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
}

#[derive(Debug, Deserialize)]
struct User {
    id: String,
    email: String,
}

#[derive(Debug, Deserialize)]
struct Org {
    id: String,
    name: String,
}

enum DeviceToken {
    Token(Token),
    Pending(String),
}

#[derive(Debug)]
pub struct SignedOut;

impl fmt::Display for SignedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reconnect OpenCode Console to continue.")
    }
}

impl std::error::Error for SignedOut {}

pub struct DeviceSignIn {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
}

impl DeviceSignIn {
    pub fn new(http: Arc<dyn HttpSender>, user_agent: String) -> Self {
        Self { integration_id: IntegrationID(INTEGRATION_ID.to_owned()), method: method(), http, user_agent }
    }
}

impl OAuthImplementation for DeviceSignIn {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        Box::pin(begin(http, user_agent, answer))
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        Some(Box::new(move |value: CredentialOAuth| {
            let http = http.clone();
            let user_agent = user_agent.clone();
            Box::pin(async move { refresh_credential(http.as_ref(), &user_agent, value).await })
        }))
    }

    fn label(&self) -> Option<LabelCallback> {
        Some(Box::new(label))
    }
}

fn locale_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a))
}

fn method() -> IntegrationOAuthMethod {
    IntegrationOAuthMethod {
        id: IntegrationMethodID(METHOD_ID.to_owned()),
        r#type: IntegrationOAuthMethodType::Oauth,
        label: "OpenCode account".to_owned(),
        form: Some(FormFields1(vec![FormField1::String(FormStringField1 {
            key: "server".to_owned(),
            title: None,
            description: None,
            required: None,
            hidden: Some(true),
            when: None,
            r#type: FormStringField1Type::String,
            format: Some(FormStringField1Format::Uri),
            min_length: None,
            max_length: None,
            pattern: None,
            placeholder: None,
            default: Some(console_access::DEFAULT_SERVER.to_owned()),
            options: None,
            custom: None,
        })])),
    }
}

async fn begin(
    http: Arc<dyn HttpSender>,
    user_agent: String,
    answer: FormAnswer,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let requested = answer_string(&answer, "server").unwrap_or(console_access::DEFAULT_SERVER);
    let server = console_access::normalize_server(requested).map_err(error)?;
    let request = http::json(
        format!("{server}/auth/device/code"),
        headers(&user_agent),
        &serde_json::json!({ "client_id": CLIENT_ID, "supports_org_scope": true }),
    );
    let device: Device = decode(&send(http.as_ref(), request).await?.body)?;
    let Device { device_code, user_code, verification_uri_complete, expires_in, interval } = device;
    let url = verification_url(&server, &verification_uri_complete)?;
    let instructions = format!("Enter code: {user_code}");
    let wait = seconds(interval).max(MIN_POLL_INTERVAL);
    let lifetime = if expires_in.is_finite() && expires_in > 0.0 {
        Duration::try_from_secs_f64(expires_in).unwrap_or(DEFAULT_EXPIRES_IN).min(MAX_EXPIRES_IN)
    } else {
        DEFAULT_EXPIRES_IN
    };
    let expires_at =
        Some(opencode_util::time::now_ms().saturating_add(i64::try_from(lifetime.as_millis()).unwrap_or(i64::MAX)));
    let callback: AutoCallback = Box::new(move || {
        Box::pin(async move { poll(http.as_ref(), &user_agent, &server, &device_code, wait, lifetime).await })
    });
    Ok(OAuthAuthorization { url, instructions, expires_at, mode: OAuthMode::Auto { callback, submit: None } })
}

fn verification_url(server: &str, complete: &str) -> Result<String, AuthorizationError> {
    let base = url::Url::parse(&format!("{server}/"))
        .map_err(|failure| error(format!("Invalid device verification URL: {failure}")))?;
    let url = base.join(complete).map_err(|failure| error(format!("Invalid device verification URL: {failure}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(error("Invalid device verification URL: expected HTTP(S)"));
    }
    Ok(url.to_string())
}

async fn poll(
    http: &dyn HttpSender,
    user_agent: &str,
    server: &str,
    device_code: &str,
    mut wait: Duration,
    lifetime: Duration,
) -> Result<CredentialOAuth, AuthorizationError> {
    let deadline = tokio::time::Instant::now() + lifetime;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(error("Device authorization expired"));
        }
        tokio::time::sleep(wait.min(remaining)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(error("Device authorization expired"));
        }
        let request = http::json(
            format!("{server}/auth/device/token"),
            headers(user_agent),
            &serde_json::json!({
                "grant_type": DEVICE_GRANT,
                "device_code": device_code,
                "client_id": CLIENT_ID,
            }),
        );
        let response = match http::send(http, request, Some(TOKEN_TIMEOUT)).await {
            Ok(response) => response,
            Err(HttpError::Status(response)) => response,
            Err(failure) => return Err(error(format!("OpenCode device token request failed: {failure}"))),
        };
        match decode_device_token(&response.body)? {
            DeviceToken::Token(token) => return credential(http, user_agent, server, token).await,
            DeviceToken::Pending(message) if message == "authorization_pending" => {}
            DeviceToken::Pending(message) if message == "slow_down" => {
                wait = wait.saturating_add(Duration::from_millis(SLOW_DOWN_MILLIS));
            }
            DeviceToken::Pending(message) => return Err(error(format!("Device authorization failed: {message}"))),
        }
    }
}

async fn credential(
    http: &dyn HttpSender,
    user_agent: &str,
    server: &str,
    token: Token,
) -> Result<CredentialOAuth, AuthorizationError> {
    let user_url = format!("{server}/api/user");
    let orgs_url = format!("{server}/api/orgs");
    let (user, orgs) = tokio::try_join!(
        get::<User>(http, &user_url, &token.access_token, user_agent),
        get::<Vec<Org>>(http, &orgs_url, &token.access_token, user_agent),
    )?;
    let org = match token.org_id.as_deref() {
        Some(id) => Some(
            orgs.iter()
                .find(|org| org.id == id)
                .ok_or_else(|| error(format!("OpenCode organization not found: {id}")))?,
        ),
        None => orgs.iter().min_by(|a, b| locale_order(&a.name, &b.name).then_with(|| locale_order(&a.id, &b.id))),
    };
    let mut metadata = vec![
        ("server".to_owned(), Value::String(server.to_owned())),
        ("accountID".to_owned(), Value::String(user.id)),
        ("email".to_owned(), Value::String(user.email)),
    ];
    if let Some(org) = org {
        metadata.push(("orgID".to_owned(), Value::String(org.id.clone())));
        metadata.push(("orgName".to_owned(), Value::String(org.name.clone())));
    }
    Ok(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(METHOD_ID.to_owned()),
        access: token.access_token,
        refresh: token.refresh_token,
        expires: opencode_util::time::now_ms() as f64 + token.expires_in * 1000.0,
        metadata: Some(JsonMap(metadata)),
    })
}

async fn refresh_credential(
    http: &dyn HttpSender,
    user_agent: &str,
    value: CredentialOAuth,
) -> Result<CredentialOAuth, AuthorizationError> {
    let server = console_access::server(&CredentialValue::Oauth(value.clone()));
    let refresh = value.refresh.clone();
    let request = http::json(
        format!("{server}/auth/device/token"),
        headers(user_agent),
        &serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh,
            "client_id": CLIENT_ID,
        }),
    );
    let response = match http::send(http, request, Some(TOKEN_TIMEOUT)).await {
        Ok(response) => response,
        Err(HttpError::Status(response)) => response,
        Err(failure) => return Err(error(format!("OpenCode token request failed: {failure}"))),
    };
    if response.status == 400
        && serde_json::from_slice::<TokenError>(&response.body).is_ok_and(|failure| failure.error == "invalid_grant")
    {
        return Err(AuthorizationError::new(SignedOut));
    }
    if !(200..=299).contains(&response.status) {
        return Err(error(format!("OpenCode token request failed: HTTP status {}", response.status)));
    }
    let token: Token = decode(&response.body)?;
    let metadata = match token.org_id.as_deref() {
        Some(org_id) => Some(rotated_metadata(value.metadata.as_ref(), org_id)),
        None => value.metadata,
    };
    Ok(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: value.method_id,
        access: token.access_token,
        refresh: token.refresh_token,
        expires: opencode_util::time::now_ms() as f64 + token.expires_in * 1000.0,
        metadata,
    })
}

fn label(value: &CredentialOAuth) -> Option<String> {
    value.metadata.as_ref().and_then(|metadata| field(metadata, "orgName")).and_then(Value::as_str).map(str::to_owned)
}

fn rotated_metadata(current: Option<&JsonMap<Value>>, org_id: &str) -> JsonMap<Value> {
    let mut entries = current.map(|metadata| metadata.0.clone()).unwrap_or_default();
    let keeps_name = current
        .and_then(|metadata| field(metadata, "orgID"))
        .and_then(Value::as_str)
        .is_some_and(|value| value == org_id)
        && current.and_then(|metadata| field(metadata, "orgName")).is_some_and(Value::is_string);
    set(&mut entries, "orgID", Value::String(org_id.to_owned()));
    if !keeps_name {
        set(&mut entries, "orgName", Value::String(org_id.to_owned()));
    }
    JsonMap(entries)
}

fn field<'a>(metadata: &'a JsonMap<Value>, key: &str) -> Option<&'a Value> {
    metadata.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

fn set(entries: &mut Vec<(String, Value)>, key: &str, value: Value) {
    match entries.iter_mut().find(|(name, _)| name == key) {
        Some((_, existing)) => *existing = value,
        None => entries.push((key.to_owned(), value)),
    }
}

async fn get<T: DeserializeOwned>(
    http: &dyn HttpSender,
    url: &str,
    token: &str,
    user_agent: &str,
) -> Result<T, AuthorizationError> {
    let response = send(http, http::get(url, bearer_headers(user_agent, token))).await?;
    decode(&response.body)
}

async fn send(
    http: &dyn HttpSender,
    request: http::PreparedHttp,
) -> Result<http::BufferedResponse, AuthorizationError> {
    http::send(http, request, Some(TOKEN_TIMEOUT))
        .await
        .map_err(|failure| error(format!("OpenCode request failed: {failure}")))
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, AuthorizationError> {
    serde_json::from_slice(body).map_err(|failure| error(format!("Invalid OpenCode response: {failure}")))
}

fn decode_device_token(body: &[u8]) -> Result<DeviceToken, AuthorizationError> {
    if let Ok(token) = serde_json::from_slice::<Token>(body) {
        return Ok(DeviceToken::Token(token));
    }
    let pending: TokenError = serde_json::from_slice(body)
        .map_err(|failure| error(format!("Invalid OpenCode device token response: {failure}")))?;
    Ok(DeviceToken::Pending(pending.error))
}

fn answer_string<'a>(answer: &'a FormAnswer, key: &str) -> Option<&'a str> {
    match answer.0.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
        Some(FormValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn headers(user_agent: &str) -> Vec<(String, String)> {
    vec![("accept".to_owned(), "application/json".to_owned()), ("user-agent".to_owned(), user_agent.to_owned())]
}

fn bearer_headers(user_agent: &str, token: &str) -> Vec<(String, String)> {
    let mut headers = headers(user_agent);
    headers.push(("authorization".to_owned(), format!("Bearer {token}")));
    headers
}

fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value).unwrap_or(Duration::ZERO)
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(std::io::Error::other(message.into()))
}
