use std::io;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationID, IntegrationKeyMethod,
    IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType, ProviderID, ProviderTransport,
};
use serde::Deserialize;
use serde_json::Value;

use crate::builtin::{Builtin, Services};
use crate::credential::Update;
use crate::integration::{
    AuthorizationError, AutoCallback, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
    RefreshCallback,
};
use crate::provider_plugin::http::{self, HttpError};

pub const ID: &str = "opencode.provider.xai";

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

const ISSUER: &str = "https://auth.x.ai/oauth2";

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

const POLLING_SAFETY_MARGIN_MS: f64 = 3000.0;

const INTEGRATION_ID: &str = "xai";

const DEVICE_METHOD_ID: &str = "device";

const BROWSER_METHOD_ID: &str = "browser";

const DEFAULT_DEVICE_LIFETIME_SECONDS: f64 = 300.0;

const DEFAULT_INTERVAL_SECONDS: f64 = 5.0;

const DEFAULT_TOKEN_LIFETIME_MS: f64 = 3_600_000.0;

const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: Option<f64>,
    interval: Option<f64>,
}

#[derive(Deserialize)]
struct DeviceError {
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    exp: Option<f64>,
}

struct XaiOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
}

impl OAuthImplementation for XaiOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_device(self.http.clone(), self.user_agent.clone()))
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
}

pub async fn register(services: &Services, context: &crate::provider_plugin::PluginContext, builtin: &mut Builtin) {
    migrate_browser_credentials(services);

    let registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            move |editor| {
                let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
                editor.update(&integration_id, |integration| integration.name = "xAI".to_owned());
                editor.method_update(Implementation::OAuth(Arc::new(XaiOAuth {
                    integration_id: integration_id.clone(),
                    method: IntegrationOAuthMethod {
                        id: IntegrationMethodID(DEVICE_METHOD_ID.to_owned()),
                        r#type: IntegrationOAuthMethodType::Oauth,
                        label: "SuperGrok Subscription".to_owned(),
                        form: None,
                    },
                    http: http.clone(),
                    user_agent: user_agent.clone(),
                })));
                editor.method_update(Implementation::Method {
                    integration_id,
                    method: IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: Some("Manually enter API Key".to_owned()),
                        form: None,
                    }),
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(registration);

    let provider_registration = services
        .provider
        .transform(|editor| {
            let provider_id = ProviderID(INTEGRATION_ID.to_owned());
            let Some(provider) = editor.get(&provider_id) else {
                return Ok(());
            };
            let transport = provider.provider.settings.as_ref().and_then(|settings| settings.transport);
            editor.update(&provider_id, |provider| {
                let mut settings =
                    provider.settings.take().unwrap_or_else(crate::provider_plugin::settings::empty_settings);
                settings.transport = Some(transport.unwrap_or(ProviderTransport::Websocket));
                provider.settings = Some(settings);
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(provider_registration);
}

fn migrate_browser_credentials(services: &Services) {
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
    let Ok(credentials) = services.credentials.list(&integration_id) else {
        return;
    };
    for credential in credentials {
        let CredentialValue::Oauth(value) = &credential.value else {
            continue;
        };
        if value.method_id.0 != BROWSER_METHOD_ID {
            continue;
        }
        let mut value = value.clone();
        value.method_id = IntegrationMethodID(DEVICE_METHOD_ID.to_owned());
        let _ = services
            .credentials
            .update(&credential.id, Update { label: None, value: Some(CredentialValue::Oauth(value)) });
    }
}

async fn begin_device(http: Arc<dyn HttpSender>, user_agent: String) -> Result<OAuthAuthorization, AuthorizationError> {
    let device: Device = request(
        http.as_ref(),
        &user_agent,
        &format!("{ISSUER}/device/code"),
        &[("client_id", CLIENT_ID), ("scope", SCOPE), ("referrer", "opencode")],
    )
    .await?;
    let created = opencode_util::time::now_ms();
    let lifetime = positive_seconds(device.expires_in, 0.0);
    let expires_at = if lifetime > 0.0 { Some(created + (lifetime * 1000.0) as i64) } else { None };
    let url = device.verification_uri_complete.clone().unwrap_or_else(|| device.verification_uri.clone());
    let instructions = format!("Open {} on any device and enter code: {}", device.verification_uri, device.user_code);
    let callback: AutoCallback = Box::new(move || {
        let http = http.clone();
        let user_agent = user_agent.clone();
        let device = device.clone();
        Box::pin(async move {
            let tokens = poll(http.as_ref(), &user_agent, &device).await?;
            credential(&tokens, None, None)
        })
    });
    Ok(OAuthAuthorization { url, instructions, expires_at, mode: OAuthMode::Auto { callback, submit: None } })
}

async fn poll(http: &dyn HttpSender, user_agent: &str, device: &Device) -> Result<Token, AuthorizationError> {
    let started = opencode_util::time::now_ms();
    let expires = started + (positive_seconds(device.expires_in, DEFAULT_DEVICE_LIFETIME_SECONDS) * 1000.0) as i64;
    let mut interval = (positive_seconds(device.interval, DEFAULT_INTERVAL_SECONDS) * 1000.0).max(1000.0);
    loop {
        if opencode_util::time::now_ms() >= expires {
            return Err(error("xAI device authorization timed out"));
        }
        let request = http::form(
            format!("{ISSUER}/token"),
            headers(user_agent),
            &[("grant_type", DEVICE_GRANT), ("client_id", CLIENT_ID), ("device_code", &device.device_code)],
        );
        match http::send(http, request, Some(TOKEN_TIMEOUT)).await {
            Ok(response) => {
                return serde_json::from_slice::<Token>(&response.body)
                    .map_err(|failure| error(format!("Invalid xAI token response: {failure}")));
            }
            Err(HttpError::Status(response)) => {
                let failure = serde_json::from_slice::<DeviceError>(&response.body).ok();
                match failure.as_ref().and_then(|failure| failure.error.as_deref()) {
                    Some("authorization_pending") => {
                        tokio::time::sleep(Duration::from_millis((interval + POLLING_SAFETY_MARGIN_MS) as u64)).await;
                    }
                    Some("slow_down") => {
                        interval += 5000.0;
                        tokio::time::sleep(Duration::from_millis((interval + POLLING_SAFETY_MARGIN_MS) as u64)).await;
                    }
                    Some("access_denied") | Some("authorization_denied") => {
                        return Err(error("xAI device authorization was denied"));
                    }
                    Some("expired_token") => {
                        return Err(error("xAI device code expired - please re-run login"));
                    }
                    _ => {
                        let detail = failure.and_then(|failure| {
                            failure.error_description.filter(|detail| !detail.is_empty()).or(failure.error)
                        });
                        return Err(error(format!(
                            "xAI device token exchange failed ({}){}",
                            response.status,
                            detail.map(|detail| format!(": {detail}")).unwrap_or_default()
                        )));
                    }
                }
            }
            Err(failure) => {
                return Err(error(format!("xAI device token exchange request failed: {failure}")));
            }
        }
    }
}

async fn refresh_credential(
    http: &dyn HttpSender,
    user_agent: &str,
    value: CredentialOAuth,
) -> Result<CredentialOAuth, AuthorizationError> {
    let tokens: Token = request(
        http,
        user_agent,
        &format!("{ISSUER}/token"),
        &[("grant_type", "refresh_token"), ("refresh_token", &value.refresh), ("client_id", CLIENT_ID)],
    )
    .await?;
    credential(&tokens, Some(value.refresh), value.metadata)
}

async fn request<T: serde::de::DeserializeOwned>(
    http: &dyn HttpSender,
    user_agent: &str,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<T, AuthorizationError> {
    let request = http::form(url, headers(user_agent), fields);
    match http::send(http, request, Some(TOKEN_TIMEOUT)).await {
        Ok(response) => {
            serde_json::from_slice(&response.body).map_err(|failure| error(format!("Invalid xAI response: {failure}")))
        }
        Err(HttpError::Status(response)) => {
            let detail = String::from_utf8_lossy(&response.body);
            Err(error(format!(
                "xAI request failed ({}){}",
                response.status,
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            )))
        }
        Err(failure) => Err(error(format!("xAI request failed: {failure}"))),
    }
}

fn headers(user_agent: &str) -> Vec<(String, String)> {
    vec![("accept".to_owned(), "application/json".to_owned()), ("user-agent".to_owned(), user_agent.to_owned())]
}

fn credential(
    tokens: &Token,
    current_refresh: Option<String>,
    metadata: Option<JsonMap<Value>>,
) -> Result<CredentialOAuth, AuthorizationError> {
    let Some(refresh) = tokens.refresh_token.clone().or(current_refresh) else {
        return Err(error("xAI token response is missing refresh_token"));
    };
    Ok(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(DEVICE_METHOD_ID.to_owned()),
        refresh,
        access: tokens.access_token.clone(),
        expires: token_expiration(tokens),
        metadata,
    })
}

fn token_expiration(tokens: &Token) -> f64 {
    let now = opencode_util::time::now_ms() as f64;
    if let Some(expires_in) = tokens.expires_in.filter(|value| *value != 0.0) {
        return now + positive_seconds(Some(expires_in), 3600.0) * 1000.0;
    }
    let Some(payload) = tokens.access_token.split('.').nth(1) else {
        return now + DEFAULT_TOKEN_LIFETIME_MS;
    };
    let expiration = decode_base64(payload)
        .and_then(|bytes| serde_json::from_slice::<Claims>(&bytes).ok())
        .and_then(|claims| claims.exp)
        .filter(|expiration| *expiration != 0.0);
    expiration.map(|expiration| expiration * 1000.0).unwrap_or(now + DEFAULT_TOKEN_LIFETIME_MS)
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value).or_else(|_| URL_SAFE.decode(value)).ok()
}

fn positive_seconds(value: Option<f64>, fallback: f64) -> f64 {
    value.filter(|seconds| seconds.is_finite() && *seconds > 0.0).unwrap_or(fallback)
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}
