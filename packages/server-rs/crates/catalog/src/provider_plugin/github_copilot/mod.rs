pub mod models;
pub mod oauth;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::generated::{
    ConnectionInfo, CredentialValue, IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, ModelInfo, ProviderID,
};
use serde::Deserialize;
use serde_json::Value;

use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::provider;
use crate::provider_plugin::http::{self, HttpError};
use crate::provider_plugin::{AppInfo, PluginContext, refresh, settings};

pub const ID: &str = "opencode.provider.github.copilot";

const INTEGRATION_ID: &str = "github-copilot";

const API_VERSION: &str = "2026-08-01";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub interval: Option<Duration>,
}

const ANTHROPIC_PACKAGE: &str = "@opencode/ai/providers/anthropic";

const OPENAI_PACKAGE: &str = "@opencode/ai/providers/openai";

const USER_TIMEOUT: Duration = Duration::from_secs(5);

const TITLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Default, PartialEq)]
struct Discovery {
    base_url: Option<String>,
    #[allow(dead_code)]
    token: Option<String>,
    models: Option<models::Snapshot>,
    connection: Option<ConnectionInfo>,
}

pub struct UtilityTitleInput<'a> {
    pub base_url: &'a str,
    pub token: &'a str,
    pub model: &'a str,
    pub app: &'a AppInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UtilityTitleMessage {
    pub role: String,
    pub content: Vec<String>,
}

pub struct UtilityTitleRequest<'a> {
    pub session_id: &'a str,
    pub system: &'a [String],
    pub messages: &'a [UtilityTitleMessage],
    pub max_tokens: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum UtilityTitleError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("invalid GitHub Copilot utility title response: {0}")]
    Json(String),
    #[error("Utility title response was empty")]
    Empty,
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: CompletionMessage,
}

#[derive(Deserialize)]
struct CompletionMessage {
    #[serde(default)]
    content: Option<String>,
}

pub async fn utility_title(
    http: &dyn HttpSender,
    input: &UtilityTitleInput<'_>,
    request: &UtilityTitleRequest<'_>,
) -> Result<String, UtilityTitleError> {
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        messages.push(serde_json::json!({ "role": "system", "content": request.system.join("\n") }));
    }
    for message in request.messages {
        messages.push(serde_json::json!({ "role": message.role, "content": message.content.join("\n") }));
    }
    let mut body = serde_json::Map::new();
    body.insert("model".to_owned(), Value::String(input.model.to_owned()));
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("stream".to_owned(), Value::Bool(false));
    if let Some(max_tokens) = request.max_tokens {
        body.insert("max_tokens".to_owned(), token_limit(max_tokens));
    }
    let headers = vec![
        ("Authorization".to_owned(), format!("Bearer {}", input.token)),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), input.app.useragent()),
        ("X-GitHub-Api-Version".to_owned(), API_VERSION.to_owned()),
        ("Openai-Intent".to_owned(), "conversation-edits".to_owned()),
        ("X-Interaction-Type".to_owned(), "agent-session-name-generation".to_owned()),
        ("X-Interaction-Id".to_owned(), request.session_id.to_owned()),
        ("x-initiator".to_owned(), "agent".to_owned()),
    ];
    let response = http::send(
        http,
        http::json(format!("{}/chat/completions", input.base_url), headers, &Value::Object(body)),
        Some(TITLE_TIMEOUT),
    )
    .await?;
    let completion: Completion =
        serde_json::from_slice(&response.body).map_err(|error| UtilityTitleError::Json(error.to_string()))?;
    completion
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .map(|content| content.trim().to_owned())
        .filter(|title| !title.is_empty())
        .ok_or(UtilityTitleError::Empty)
}

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    register_with(services, context, builtin, Options::default()).await
}

pub async fn register_with(services: &Services, context: &PluginContext, builtin: &mut Builtin, options: Options) {
    let provider_id = ProviderID(INTEGRATION_ID.to_owned());
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());

    let integration_registration = {
        let integration_id = integration_id.clone();
        let http = context.http.clone();
        let user_agent = context.app.useragent();
        services
            .integration
            .transform(move |editor| {
                editor.method_remove(
                    &integration_id,
                    &IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: None,
                        form: None,
                    }),
                );
                editor.method_update(Implementation::Method {
                    integration_id: integration_id.clone(),
                    method: IntegrationMethod::Env(IntegrationEnvMethod {
                        r#type: IntegrationEnvMethodType::Env,
                        names: vec!["COPILOT_GITHUB_TOKEN".to_owned()],
                    }),
                });
                editor.method_update(Implementation::OAuth(Arc::new(oauth::CopilotOAuth::new(
                    http.clone(),
                    user_agent.clone(),
                ))));
                Ok(())
            })
            .await
    };
    builtin.registrations.push(integration_registration);

    let login = settings::credential_settings(services, builtin, &integration_id, |credential| match credential {
        CredentialValue::Oauth(oauth) => {
            let mut settings = serde_json::Map::new();
            settings.insert("baseURL".to_owned(), Value::String(oauth::copilot_base_url(oauth.metadata.as_ref())));
            provider::decode_object(settings)
        }
        _ => provider::decode_object(serde_json::Map::new()),
    })
    .await;

    let integrations = services.integration.clone();
    let providers = services.provider.clone();
    let http = context.http.clone();
    let user_agent = context.app.useragent();
    let cache = context.shared_cache.clone();
    let reload = services.provider.clone();
    let active_integrations = integrations.clone();
    let active_integration_id = integration_id.clone();
    let load_provider_id = provider_id.clone();
    let load_integration_id = integration_id.clone();

    let discovered = refresh::start(
        &services.bus,
        refresh::Start::<Discovery, Infallible> {
            integration_id: INTEGRATION_ID.to_owned(),
            initial: Discovery::default(),
            interval: options.interval,
            active: Box::new(move || {
                let integrations = active_integrations.clone();
                let integration_id = active_integration_id.clone();
                Box::pin(async move { integrations.connection().active(&integration_id).await.ok().flatten() })
            }),
            load: Box::new(move |_, options| {
                let integrations = integrations.clone();
                let providers = providers.clone();
                let http = http.clone();
                let user_agent = user_agent.clone();
                let cache = cache.clone();
                let provider_id = load_provider_id.clone();
                let integration_id = load_integration_id.clone();
                Box::pin(async move {
                    let (connection, credential) = settings::active_credential(&integrations, &integration_id).await;
                    let Some(connection) = connection else {
                        return Ok(refresh::Loaded { connection: None, value: Discovery::default() });
                    };
                    let Some(CredentialValue::Oauth(credential)) = credential else {
                        return Ok(refresh::Loaded { connection: Some(connection), value: Discovery::default() });
                    };
                    let endpoint = cache
                        .get(
                            &format!("github-copilot-endpoint:{}", crate::integration::key(&connection)),
                            options.interval,
                            options.force,
                            {
                                let integrations = integrations.clone();
                                let http = http.clone();
                                let user_agent = user_agent.clone();
                                let connection = connection.clone();
                                move || {
                                    let integrations = integrations.clone();
                                    let http = http.clone();
                                    let user_agent = user_agent.clone();
                                    let connection = connection.clone();
                                    async move {
                                        let Some(CredentialValue::Oauth(current)) =
                                            integrations.connection().resolve(&connection).await.ok().flatten()
                                        else {
                                            return Err(models::LoadError::MissingCredential);
                                        };
                                        let enterprise = current
                                            .metadata
                                            .as_ref()
                                            .and_then(|metadata| {
                                                metadata
                                                    .0
                                                    .iter()
                                                    .find(|(key, _)| key == "enterpriseUrl")
                                                    .map(|(_, value)| value)
                                            })
                                            .and_then(Value::as_str);
                                        let user = oauth::fetch_user(
                                            http.as_ref(),
                                            &current.refresh,
                                            &user_agent,
                                            enterprise,
                                            Some(USER_TIMEOUT),
                                        )
                                        .await?;
                                        Ok(user.and_then(|user| user.api_endpoint()))
                                    }
                                }
                            },
                        )
                        .await
                        .ok()
                        .flatten();
                    let url = endpoint.unwrap_or_else(|| oauth::copilot_base_url(credential.metadata.as_ref()));
                    let headers = providers
                        .get(&provider_id)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|provider| provider.headers)
                        .map(|headers| headers.0)
                        .unwrap_or_default();
                    let models = cache
                        .get(&models_key(Some(&connection), &url, &headers), options.interval, options.force, {
                            let integrations = integrations.clone();
                            let http = http.clone();
                            let user_agent = user_agent.clone();
                            let connection = connection.clone();
                            let url = url.clone();
                            move || {
                                let integrations = integrations.clone();
                                let http = http.clone();
                                let user_agent = user_agent.clone();
                                let connection = connection.clone();
                                let url = url.clone();
                                let headers = headers.clone();
                                async move {
                                    let Some(CredentialValue::Oauth(current)) =
                                        integrations.connection().resolve(&connection).await.ok().flatten()
                                    else {
                                        return Err(models::LoadError::MissingCredential);
                                    };
                                    let mut request_headers = headers;
                                    upsert(
                                        &mut request_headers,
                                        "Authorization",
                                        format!("Bearer {}", current.refresh),
                                    );
                                    upsert(&mut request_headers, "User-Agent", user_agent);
                                    upsert(&mut request_headers, "X-GitHub-Api-Version", API_VERSION.to_owned());
                                    models::load(http.as_ref(), &url, request_headers).await
                                }
                            }
                        })
                        .await
                        .ok();
                    Ok(refresh::Loaded {
                        connection: Some(connection.clone()),
                        value: Discovery {
                            base_url: Some(url),
                            token: Some(credential.refresh),
                            models,
                            connection: Some(connection),
                        },
                    })
                })
            }),
            reload: Box::new(move || {
                let providers = reload.clone();
                Box::pin(async move { providers.reload().await })
            }),
        },
    );

    let provider_registration = services
        .provider
        .transform({
            let provider_id = provider_id.clone();
            let login = login.clone();
            move |editor| {
                let Some(item) = editor.get(&provider_id) else {
                    return Ok(());
                };
                let provider = item.provider.clone();
                let existing: Vec<ModelInfo> = item.models.values().cloned().collect();
                let Discovery { base_url, models: snapshot, connection, .. } = discovered.current();
                let login = login.lock().expect("lock the GitHub Copilot login settings").clone();
                let url = base_url
                    .or_else(|| super::setting(Some(&login), "baseURL").map(str::to_owned))
                    .unwrap_or_else(|| oauth::api_base_url(None));
                let Some(snapshot) = snapshot else {
                    for model in &existing {
                        let advertised = match model.package.as_deref() {
                            Some(ANTHROPIC_PACKAGE) => Some(models::Endpoint::Messages),
                            Some(OPENAI_PACKAGE) => Some(models::Endpoint::Responses),
                            _ => None,
                        };
                        let route = models::route_for(&model.model_id, advertised, &url);
                        editor.models_update(&provider_id, &model.id, |model| {
                            model.package = Some(route.package.to_owned());
                            model.settings = Some(models::merge_settings(model.settings.as_ref(), &route.base_url));
                        });
                    }
                    return Ok(());
                };
                let router = models::auto_model(&url, &snapshot);
                let mut derived: Vec<ModelInfo> = models::derive(&url, &snapshot, &existing).into_values().collect();
                if let Some(router) = router {
                    derived.push(router);
                }
                editor.add(provider, &derived, connection);
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                if editor.get(&provider_id, "gpt-5-chat-latest").is_some() {
                    editor.update(&provider_id, "gpt-5-chat-latest", |model| model.enabled = false);
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(model_registration);
}

pub fn models_key(connection: Option<&ConnectionInfo>, url: &str, headers: &[(String, String)]) -> String {
    let access = connection.map(crate::integration::key).unwrap_or_default();
    let mut sorted = headers.to_vec();
    sorted.sort_by_cached_key(|(name, value)| format!("{name},{value}"));
    let fingerprint =
        crate::shared_cache::fingerprint(&serde_json::to_string(&sorted).expect("provider headers serialize"));
    format!("github-copilot:{access}:{url}:{fingerprint}")
}

fn upsert(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}

fn token_limit(value: f64) -> Value {
    if value.fract() == 0.0 {
        return Value::from(value as i64);
    }
    serde_json::Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
}
